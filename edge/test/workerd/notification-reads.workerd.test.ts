import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { Notifications } from "../../src/notifications";
import type { Env } from "../../src/env";
import type { Row } from "../../src/registry-core";

function fixture(state: DurableObjectState) {
  const rows = new Map<string, Row>();
  const row = (kind: string, id: string, fields: Row["fields"]): Row =>
    ({ kind, id, seq: 1, deleted: false, fields, clocks: {} });
  for (const id of ["chat", "other"]) {
    rows.set(`chats/${id}`, row("chats", id, { deviceId: "host", spaceId: "project" }));
  }
  rows.set("spaces/project", row("spaces", "project", {}));
  const sends: { message: { id: string; chatId: string } }[] = [];
  let duringSend: (() => Promise<void>) | undefined;
  let replay: (() => Promise<void>) | undefined;
  const config = {
    NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456",
    APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test",
    PUSH_DEVICES: { idFromString: (id: string) => id, get: () => ({ fetch: async (request: Request) => {
      sends.push(await request.json() as typeof sends[number]);
      await duringSend?.();
      return Response.json({ sent: false }); // failures normally schedule retry
    } }) }
  } as unknown as Env;
  const make = () => new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
  let service = make();
  state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)", JSON.stringify([
    { id: "a".repeat(64), lease: "lease-a", installationId: "phone", epoch: 1 },
    { id: "b".repeat(64), lease: "lease-b", installationId: "phone2", epoch: 1 }
  ]));
  for (const id of ["chat", "other"]) {
    state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?)", `target:${id}`,
      JSON.stringify({ clientId: "phone", platform: "ios", at: Date.now() }));
  }
  const post = async (path: string, body: unknown) => {
    const response = await service.fetch(new Request("https://test", { method: "POST", body: JSON.stringify(body) }), path);
    expect(response.status).toBe(200);
    return response.json() as Promise<{ readEventIds?: string[] }>;
  };
  return {
    sends,
    post,
    flush: () => service.flush(),
    restart: () => { service = make(); },
    replay: async () => { await replay?.(); },
    nextDue: () => service.nextDue(),
    onSend: (callback: () => Promise<void>) => { duringSend = callback; },
    ids: () => [...state.storage.sql.exec("SELECT id FROM notify_events")].map(r => r.id),
    activity: (sequence: number, chatId: string | null, foreground = true, platform = "ios") =>
      post("activity", { clientId: "phone", platform, foreground, sequence, chatId, interactionAgeMs: 0 }),
    enqueue: async (source: "registry" | "event", chatId = "chat", status = "idle") => {
      const now = Date.now();
      const working = row("sessions", chatId, { chatId, deviceId: "host", status: "working",
        startedAt: now - 60_000, updatedAt: now - 1 });
      const done = row("sessions", chatId, { ...working.fields, status, updatedAt: now });
      rows.set(`sessions/${chatId}`, done);
      replay = async () => {
        if (source === "registry") {
          service.observe([{ before: row("sessions", chatId, { ...working.fields, status: "idle" }), after: working }], "host");
          service.observe([{ before: working, after: done }], "host");
        } else {
          await post("event", working.fields);
          await post("event", done.fields);
        }
      };
      await replay();
    }
  };
}

describe("reading cancels pending events, not future runs", () => {
  for (const source of ["registry", "event"] as const) {
    it.each(["idle", "errored", "awaitingInput"])(`${source}: viewing cancels %s durably after leaving`, async status => {
      const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName(`read-${source}-${status}`));
      await runInDurableObject(stub, async (_, state) => {
        const f = fixture(state), start = Date.now();
        const clock = vi.spyOn(Date, "now").mockReturnValue(start);
        try {
          await f.enqueue(source, "chat", status);
          const ids = f.ids();
          expect(ids).toHaveLength(1);
          clock.mockReturnValue(start + 5_000);
          const reply = await f.activity(1, "chat");
          expect(reply.readEventIds).toEqual(ids);
          expect(f.ids()).toEqual([]);
          await f.activity(2, null, false);
          f.restart(); // cancellation survives durable-object reconstruction
          await f.replay();
          expect(f.ids()).toEqual([], "Replayed terminal state must not resurrect a read event");
          clock.mockReturnValue(start + 20_000);
          await f.flush();
          expect(f.sends).toHaveLength(0);
          // A genuinely new run after leaving must still notify.
          await f.enqueue(source, "chat", status);
          expect(f.ids()).toHaveLength(1);
          clock.mockReturnValue(start + 31_000);
          await f.flush();
          expect(f.sends).toHaveLength(1);
        } finally { clock.mockRestore(); }
      });
    });
  }

  it("does not treat Home, a different chat, background or stale/reordered reads as viewing", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("read-stale"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      const clock = vi.spyOn(Date, "now").mockReturnValue(start);
      try {
        await f.activity(1, "chat");
        await f.activity(2, null); // left before this event existed
        clock.mockReturnValue(start + 1_000);
        await f.enqueue("registry");
        const ids = f.ids();
        await f.activity(1, "chat"); // delayed/replayed old entry report
        await f.activity(2, "chat"); // duplicate sequence cannot acknowledge
        await f.activity(3, "chat", false);
        await f.activity(4, "other");
        expect(f.ids()).toEqual(ids);
        clock.mockReturnValue(start + 12_000);
        await f.flush();
        expect(f.sends).toHaveLength(1);
      } finally { clock.mockRestore(); }
    });
  });

  it("does not cancel another chat and drops new events while the viewer remains present", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("read-still-viewing"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      const clock = vi.spyOn(Date, "now").mockReturnValue(start);
      try {
        await f.activity(1, "chat");
        await f.enqueue("event");
        await f.enqueue("event", "other");
        expect(f.ids()).toHaveLength(1); // event arrived while "chat" was visible
        await f.activity(2, null); // leaving before flush must not bring it back
        clock.mockReturnValue(start + 11_000);
        await f.flush();
        expect(f.sends.map(s => s.message.chatId)).toEqual(["other"]);
        clock.mockReturnValue(start + 20_000);
        await f.flush();
        expect(f.sends.every(s => s.message.chatId === "other")).toBe(true);
      } finally { clock.mockRestore(); }
    });
  });

  it("a read during APNs await stops other recipients and prevents retry resurrection", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("read-during-send"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      state.storage.sql.exec("UPDATE notify_kv SET value=? WHERE key='target:chat'",
        JSON.stringify({ clientId: "desktop", platform: "desktop", at: start - 60_000 }));
      await f.enqueue("event");
      const clock = vi.spyOn(Date, "now").mockReturnValue(start + 11_000);
      try {
        f.onSend(async () => { await f.activity(1, "chat"); });
        await f.flush();
        expect(f.sends).toHaveLength(1); // first request was already submitted
        expect(f.nextDue()).toBeUndefined();
        clock.mockReturnValue(start + 60_000);
        await f.flush();
        expect(f.sends).toHaveLength(1);
      } finally { clock.mockRestore(); }
    });
  });
});

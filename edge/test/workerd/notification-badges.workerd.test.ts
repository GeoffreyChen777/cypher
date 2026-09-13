import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { Notifications } from "../../src/notifications";
import { PushDevice } from "../../src/push-device";
import { defaultNotificationSettings } from "../../src/notifications-model";
import type { BadgeSnapshot, PushMessage } from "../../src/apns";
import type { Env } from "../../src/env";
import type { Row } from "../../src/registry-core";

function fixture(state: DurableObjectState) {
  const rows = new Map<string, Row>();
  const row = (kind: string, id: string, fields: Row["fields"]): Row =>
    ({ kind, id, seq: 1, deleted: false, fields, clocks: {} });
  for (const id of ["one", "two"]) rows.set(`chats/${id}`, row("chats", id, { spaceId: "project", deviceId: "host" }));
  rows.set("spaces/project", row("spaces", "project", {}));
  const calls: PushMessage[] = [];
  let sendHook: ((message: PushMessage) => Promise<void>) | undefined;
  const config = { NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456",
    APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test", PUSH_DEVICES: {
      idFromString: (id: string) => id, get: () => ({ fetch: async (r: Request) => {
        const body = await r.json() as { message: PushMessage };
        calls.push(body.message);
        await sendHook?.(body.message);
        return Response.json({ sent: true });
      } })
    } } as unknown as Env;
  const make = () => new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
  let service = make();
  state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)", JSON.stringify([
    { id: "a".repeat(64), installationId: "phone", lease: "lease-a", epoch: 1 },
    { id: "b".repeat(64), installationId: "phone2", lease: "lease-b", epoch: 1 }
  ]));
  for (const id of ["one", "two"]) state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?)", `target:${id}`,
    JSON.stringify({ clientId: "phone", platform: "ios", at: Date.now() }));
  const request = async (path: string, method: string, body?: unknown) => {
    const response = await service.fetch(new Request("https://test", { method,
      ...(body === undefined ? {} : { body: JSON.stringify(body) }) }), path);
    expect(response.status).toBe(200);
    return response.json() as Promise<BadgeSnapshot & { readEventIds?: string[] }>;
  };
  return {
    rows, calls, config,
    restart: () => { service = make(); },
    flush: () => service.flush(),
    snapshot: () => request("settings", "GET"),
    settings: (body: unknown) => request("settings", "PUT", body),
    onSend: (hook: (message: PushMessage) => Promise<void>) => { sendHook = hook; },
    view: (chatId: string | null, sequence: number, foreground = true) => request("activity", "POST",
      { clientId: "phone", platform: "ios", chatId, sequence, foreground, interactionAgeMs: 0 }),
    enqueue: async (chatId: string, status = "idle", duration = 60_000) => {
      const now = Date.now();
      await request("event", "POST", { chatId, deviceId: "host", status: "working", updatedAt: now - 1, startedAt: now - duration });
      await request("event", "POST", { chatId, deviceId: "host", status, updatedAt: now, startedAt: now - duration });
    }
  };
}

describe("durable unread-conversation badges", () => {
  it("counts chats once, survives alert delivery/restart, and clears only the opened chat", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badges-unread"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      const clock = vi.spyOn(Date, "now").mockReturnValue(start);
      try {
        await f.enqueue("one");
        clock.mockReturnValue(start + 1_000);
        await f.enqueue("one", "errored");
        expect((await f.snapshot()).badgeCount).toBe(1);
        await f.enqueue("two", "awaitingInput");
        expect((await f.snapshot()).badgeCount).toBe(2);
        clock.mockReturnValue(start + 12_000);
        await f.flush();
        expect(f.calls.filter(m => m.kind !== "badge").map(m => m.badgeCount)).toEqual([2, 2]);
        expect(f.calls.filter(m => m.kind === "badge").map(m => m.badgeCount)).toEqual([2, 2]);
        f.restart();
        expect((await f.snapshot()).badgeCount).toBe(2);
        expect((await f.view(null, 1)).badgeCount).toBe(2); // Home does not read all
        const read = await f.view("one", 2);
        expect(read.badgeCount).toBe(1);
        expect(read.readEventIds).toHaveLength(1); // event has left the outbox
        await f.flush();
        expect(f.calls.slice(-2).map(m => [m.kind, m.badgeCount])).toEqual([["badge", 1], ["badge", 1]]);
        await f.view("two", 3);
        await f.flush();
        expect(f.calls.slice(-2).map(m => m.badgeCount)).toEqual([0, 0]);
        expect((await f.snapshot()).badgeCount).toBe(0);
        await f.view(null, 4);
        clock.mockReturnValue(start + 120_000);
        await f.enqueue("one");
        expect((await f.snapshot()).badgeCount).toBe(1);
      } finally { clock.mockRestore(); }
    });
  });

  it("excludes short/disabled/muted/archived events and prunes unread badges on preference changes", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badges-filter"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      const clock = vi.spyOn(Date, "now").mockReturnValue(start);
      try {
        await f.enqueue("one", "idle", 1_000);
        expect((await f.snapshot()).badgeCount).toBe(0);
        clock.mockReturnValue(start + 1_000);
        await f.enqueue("one", "errored");
        expect((await f.snapshot()).badgeCount).toBe(1);
        expect((await f.settings({ ...defaultNotificationSettings(), failed: false })).badgeCount).toBe(0);
        clock.mockReturnValue(start + 2_000);
        await f.enqueue("two");
        expect((await f.snapshot()).badgeCount).toBe(1);
        f.rows.get("chats/two")!.fields.archived = true;
        expect((await f.snapshot()).badgeCount).toBe(0);
        f.rows.get("chats/two")!.fields.archived = false;
        await f.settings({ ...defaultNotificationSettings(), mutedProjects: ["project"] });
        clock.mockReturnValue(start + 3_000);
        await f.enqueue("two");
        expect((await f.snapshot()).badgeCount).toBe(0);
      } finally { clock.mockRestore(); }
    });
  });

  it("a read during badge delivery replaces the old job with an absolute zero", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badges-race"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      const clock = vi.spyOn(Date, "now").mockReturnValue(start);
      try {
        await f.enqueue("one");
        f.onSend(async message => {
          if (message.kind === "badge" && message.badgeCount === 1) {
            await f.view("one", 1);
          }
        });
        clock.mockReturnValue(start + 11_000);
        await f.flush();
        await f.flush();
        expect((await f.snapshot()).badgeCount).toBe(0);
        expect(f.calls.slice(-2).map(m => [m.kind, m.badgeCount])).toEqual([["badge", 0], ["badge", 0]]);
        const count = f.calls.length;
        clock.mockReturnValue(start + 60_000);
        await f.flush();
        expect(f.calls).toHaveLength(count);
      } finally { clock.mockRestore(); }
    });
  });

  it("disabled delivery clears badge work instead of spinning an overdue alarm", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badges-disabled"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state);
      await f.enqueue("one");
      f.config.NOTIFICATIONS_ENABLED = "false";
      await f.flush();
      expect([...state.storage.sql.exec("SELECT value FROM notify_kv WHERE key='badgeJob'")][0].value).toBe("null");
      expect(f.calls).toHaveLength(0);
    });
  });

  it("alert expiry is not a read and does not erase the unread session", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badges-expiry"));
    await runInDurableObject(stub, async (_, state) => {
      const f = fixture(state), start = Date.now();
      await f.enqueue("one");
      const clock = vi.spyOn(Date, "now").mockReturnValue(start + 601_000);
      try {
        await f.flush();
        expect(f.calls).toHaveLength(0);
        expect((await f.snapshot()).badgeCount).toBe(1);
        expect((await f.view("one", 1)).badgeCount).toBe(0);
      } finally { clock.mockRestore(); }
    });
  });
});

describe("badge token ownership and ordering", () => {
  it("normalizes old counts, deduplicates retries and isolates account switches", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("badge-token-order"));
    await runInDurableObject(stub, async (_, state) => {
      const calls: PushMessage[] = [];
      const device = new PushDevice(state, { APNS_SENDER: { fetch: async (r: Request) => {
        calls.push((await r.json() as { message: PushMessage }).message);
        return Response.json({ result: "sent" });
      } } } as unknown as Env);
      const post = (path: string, body: unknown) => device.fetch(new Request(`https://test/${path}`,
        { method: "POST", body: JSON.stringify(body) }));
      const registration = { installationId: "phone", epoch: 1, token: "0".repeat(64), environment: "production" };
      const scope = "a".repeat(64);
      const first = await (await post("register", { ...registration, scope })).json() as { lease: string };
      const fresh: PushMessage = { id: crypto.randomUUID(), scope, kind: "badge", badgeCount: 0, badgeRevision: 5, expires: Date.now() + 60_000 };
      await post("send", { scope, lease: first.lease, message: fresh });
      const old: PushMessage = { ...fresh, id: crypto.randomUUID(), kind: "completed", chatId: "one", projectId: "project",
        badgeCount: 3, badgeRevision: 2 };
      await post("send", { scope, lease: first.lease, message: old });
      expect(calls[1]).toMatchObject({ kind: "completed", badgeCount: 0, badgeRevision: 5 });
      await post("send", { scope, lease: first.lease, message: old });
      expect(calls).toHaveLength(2);
      const nextScope = "b".repeat(64);
      const next = await (await post("register", { ...registration, epoch: 2, scope: nextScope })).json() as { lease: string };
      expect(await (await post("send", { scope, lease: first.lease, message: { ...fresh, id: crypto.randomUUID() } })).json())
        .toEqual({ permanent: true });
      await post("send", { scope: nextScope, lease: next.lease, message: { ...fresh, id: crypto.randomUUID(),
        scope: nextScope, badgeCount: 4, badgeRevision: 1 } });
      expect(calls[2]).toMatchObject({ scope: nextScope, badgeCount: 4, badgeRevision: 1 });
      expect((await post("send", { scope: nextScope, lease: next.lease,
        message: { ...fresh, scope: nextScope, badgeCount: -1 } })).status).toBe(400);
    });
  });
});

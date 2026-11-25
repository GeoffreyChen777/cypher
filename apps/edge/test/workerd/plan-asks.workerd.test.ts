/**
 * A plan's questions reach the owner's phone (docs/design/plan-asks.md).
 *
 * Why it matters: a session a workspace's plan started could ask without
 * the owner ever hearing of it. The host's push was keyed on status
 * transitions, so a second question while the row already said
 * `awaitingInput` was never news (case h); a question left open by a run
 * that died read as `idle` and withdrew its alert (e1); a subagent's
 * question mirrored into its session could push twice, once per chat (c).
 *
 * What must stay true: a session row's open question (`asking`) awaits input
 * whatever its status says; each new `asking.requestId` is one new `input`
 * notice, the same id never twice; a plan's question goes out as its
 * workspace's Needs you item, naming the plan's session, whichever chat
 * under it asked; a question mirrored into a chat above pushes once, for
 * that chat; an answer withdraws it unless a chat under the session still
 * asks; a host that writes no `asking` behaves as before.
 */
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { Notifications } from "../../src/notifications/notifications";
import type { PushMessage } from "../../src/notifications/apns";
import type { Row } from "../../src/registry/registry-core";
import { pushEnv, row } from "./support";

type Sent = PushMessage & { chatId?: string; workspaceId?: string; itemKind?: string; itemId?: string };
type Ask = { requestId: string; askedAt?: number; fromChatId?: string } | null;

function fixture(state: DurableObjectState) {
  const start = Date.now();
  const rows = new Map<string, Row>();
  const put = (r: Row) => { rows.set(`${r.kind}/${r.id}`, r); return r; };
  put(row("bots", "ws", { id: "ws", chatId: "wschat", hostDeviceId: "host", name: "Atlas" }));
  put(row("chats", "wschat", { id: "wschat", deviceId: "host", bot: { botId: "ws", origin: "owner" } }));
  put(row("spaces", "project", {}));
  // A plan's step session, its subagent's chat and that one's own subagent.
  put(row("chats", "step", { deviceId: "host", spaceId: "project", startedBy: { botId: "ws", workId: "w" } }));
  put(row("chats", "kid", { deviceId: "host", spaceId: "project",
    child: { parentChatId: "step", parentRunId: "run", agent: "reviewer" } }));
  put(row("chats", "grandkid", { deviceId: "host", spaceId: "project",
    child: { parentChatId: "kid", parentRunId: "run2", agent: "checker" } }));
  // The owner's own session and its subagent.
  put(row("chats", "mine", { deviceId: "host", spaceId: "project" }));
  put(row("chats", "minekid", { deviceId: "host", spaceId: "project",
    child: { parentChatId: "mine", parentRunId: "run", agent: "planner" } }));
  const sent: Sent[] = [];
  const service = new Notifications(state, pushEnv(async request => {
    sent.push((await request.json() as { message: Sent }).message);
    return Response.json({ sent: true });
  }), (kind, id) => rows.get(`${kind}/${id}`), () => {});
  state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
    JSON.stringify([{ id: "b".repeat(64), lease: "lease", installationId: "phone", epoch: 1 }]));
  // The phone opened the owner's session and its subagent (a plan's items
  // need no target).
  for (const chat of ["mine", "minekid"]) {
    state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?)", `target:${chat}`,
      JSON.stringify({ clientId: "phone", platform: "ios", at: start }));
  }
  const clock = vi.spyOn(Date, "now").mockReturnValue(start);
  let now = start;
  const at = (t: number) => { now = t; clock.mockReturnValue(t); };
  const request = async (path: string, method: string, body?: unknown) => {
    const response = await service.fetch(new Request("https://test", { method,
      ...(body === undefined ? {} : { body: JSON.stringify(body) }) }), path);
    expect(response.status).toBe(200);
    return response.json() as Promise<Record<string, unknown>>;
  };
  return {
    rows, sent, service, start, at,
    restore: () => clock.mockRestore(),
    get now() { return now; },
    alerts: () => sent.filter(m => m.kind !== "badge"),
    badge: async () => (await request("settings", "GET")).badgeCount,
    /** The host's report for `chatId`, one second on; `asking` as the
     * engine sends it (`null` while none is open, absent from old hosts). */
    event: (chatId: string, status: string, asking?: Ask) => {
      at(now + 1_000);
      return request("event", "POST", { chatId, deviceId: "host", status, updatedAt: now, startedAt: start,
        ...(asking === undefined ? {} : { asking }) });
    },
    /** A registry write of the session row, merged over what is there. */
    session: (chatId: string, fields: Row["fields"]) => {
      at(now + 1_000);
      const before = rows.get(`sessions/${chatId}`);
      const after = put(row("sessions", chatId, { chatId, deviceId: "host", startedAt: start,
        ...(before?.fields ?? {}), ...fields, updatedAt: now }));
      service.observe([{ before, after }], "host");
    },
    flush: async () => { at(now + 11_000); await service.flush(); }
  };
}

const route = (m: Sent) => [m.chatId, m.workspaceId ?? null, m.itemKind ?? null, m.itemId ?? null];
const item = ["wschat", "ws", "asking", "step"];

describe("a plan's questions", () => {
  it("go out as the workspace's Needs you item, once per question, even without a status change", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-each")), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event("step", "working", null);
        expect(await f.event("step", "awaitingInput", { requestId: "r1" })).toEqual({ ok: true, queued: true });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item]);
        // Its subagent asks while the session's own prompt is open: the
        // status stays awaiting input, the question is new (case h).
        expect(await f.event("step", "awaitingInput", { requestId: "m2", fromChatId: "kid" }))
          .toEqual({ ok: true, queued: true });
        // The same question reported again is not.
        expect(await f.event("step", "awaitingInput", { requestId: "m2", fromChatId: "kid" }))
          .toEqual({ ok: true, duplicate: true });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item, item]);
        expect(await f.badge()).toBe(1);
        // Answered: the session works again, the item stops counting.
        await f.event("step", "working", null);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it("stay open across a run that died under them (row idle, question kept)", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-restart")), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event("step", "working", null);
        await f.event("step", "awaitingInput", { requestId: "r1" });
        // Recovery set the row idle; the question is still open (e1).
        expect(await f.event("step", "idle", { requestId: "r1" })).toEqual({ ok: true, duplicate: true });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item]);
        expect(await f.badge()).toBe(1);
        // The question is gone (answered as a new turn that already ended).
        await f.event("step", "idle", null);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it("asked in a subagent's chat push once, for the chat they are mirrored into", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-mirror")), async (_, state) => {
      const f = fixture(state);
      try {
        // The session's row names the child first: the child's report is skipped.
        await f.event("step", "awaitingInput", { requestId: "m1", fromChatId: "kid" });
        expect(await f.event("kid", "awaitingInput", { requestId: "r1" })).toEqual({ ok: true, mirrored: true });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item]);
        // The child first, then its mirror: still one alert for the item.
        await f.event("kid", "working", null);
        await f.event("step", "working", null);
        await f.event("kid", "awaitingInput", { requestId: "r2" });
        await f.event("step", "awaitingInput", { requestId: "m2", fromChatId: "kid" });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item, item]);
      } finally { f.restore(); }
    });
  });

  it("asked deeper down without a mirror stay until that chat moves on", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-deep")), async (_, state) => {
      const f = fixture(state);
      try {
        // The step session's run ended; its subagent's subagent asks.
        await f.event("step", "working", null);
        await f.event("step", "idle", null);
        expect(await f.event("grandkid", "awaitingInput", { requestId: "r1" })).toEqual({ ok: true, queued: true });
        // Another chat under the session settling is not this answer.
        await f.event("kid", "working", null);
        await f.event("kid", "idle", null);
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item]);
        expect(await f.badge()).toBe(1);
        await f.event("grandkid", "working", null);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it("reach the phone through the registry row too", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-rows")), async (_, state) => {
      const f = fixture(state);
      try {
        f.session("step", { status: "working", asking: null });
        f.session("step", { status: "awaitingInput", asking: { requestId: "r1", askedAt: f.now } });
        // A heartbeat of the same question is not news; a new one is.
        f.session("step", { status: "awaitingInput", asking: { requestId: "r1", askedAt: f.now } });
        await f.flush();
        f.session("step", { status: "awaitingInput", asking: { requestId: "r2", askedAt: f.now } });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([item, item]);
        f.session("step", { status: "working", asking: null });
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });
});

describe("the owner's own session", () => {
  it("pushes a subagent's mirrored question once, for the session", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-own")), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event("minekid", "working", null);
        await f.event("mine", "working", null);
        // The child's report lands first, then its mirror in the session.
        await f.event("minekid", "awaitingInput", { requestId: "r1" });
        await f.event("mine", "awaitingInput", { requestId: "m1", fromChatId: "minekid" });
        await f.flush();
        expect(f.alerts().map(route)).toEqual([["mine", null, null, null]]);
        expect(await f.badge()).toBe(1);
      } finally { f.restore(); }
    });
  });

  it("from an old host (no asking) notifies exactly as before", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("plan-asks-old")), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event("mine", "working");
        expect(await f.event("mine", "awaitingInput")).toEqual({ ok: true, queued: true });
        expect(await f.event("mine", "awaitingInput")).toEqual({ ok: true, duplicate: true });
        await f.flush();
        expect(f.alerts().map(m => [m.chatId, m.kind])).toEqual([["mine", "input"]]);
      } finally { f.restore(); }
    });
  });
});

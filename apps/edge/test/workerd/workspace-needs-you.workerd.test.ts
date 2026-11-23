/**
 * Workspaces reach the owner's phone.
 *
 * Why it matters: a workspace's own chat has no project, so the Worker dropped
 * every one of its events (`reason: "space"`) before the Dock rule could count
 * it, and nothing on a workspace's Needs you card (a plan or an action
 * awaiting approval, a peer answer awaiting review, a plan or routine thread
 * asking) was ever pushed. The owner's iPhone stayed silent while a workspace
 * waited on them.
 *
 * What must stay true: the workspace's chat notifies like a session, filed
 * under the workspace (`projectId` = bot id) for muting and the payload; each
 * Needs you item is ONE `input` notice of that chat, naming the item, never
 * repeated by a later sync; decided items stop counting; hidden threads that
 * are not asking, peer threads and memory passes never notify; any other
 * project-less chat still never notifies.
 */
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { Notifications } from "../../src/notifications/notifications";
import { defaultNotificationSettings } from "../../src/notifications/notifications-model";
import type { PushMessage } from "../../src/notifications/apns";
import type { Row } from "../../src/registry/registry-core";
import { pushEnv, row } from "./support";

type Sent = PushMessage & { workspaceId?: string; itemKind?: string; itemId?: string };

function fixture(state: DurableObjectState) {
  const start = Date.now();
  const rows = new Map<string, Row>();
  const put = (r: Row) => { rows.set(`${r.kind}/${r.id}`, r); return r; };
  put(row("bots", "ws", { id: "ws", chatId: "wschat", hostDeviceId: "host", name: "Atlas" }));
  put(row("chats", "wschat", { id: "wschat", deviceId: "host", bot: { botId: "ws", origin: "owner" } }));
  put(row("spaces", "project", {}));
  put(row("chats", "one", { deviceId: "host", spaceId: "project" }));
  put(row("chats", "routine-thread", { deviceId: "host",
    bot: { botId: "ws", origin: "routine", thread: { kind: "routine", routineId: "r" } } }));
  put(row("chats", "work-thread", { deviceId: "host",
    bot: { botId: "ws", origin: "owner", thread: { kind: "work", workId: "w" } } }));
  put(row("chats", "routine-session", { deviceId: "host", spaceId: "project",
    bot: { botId: "ws", origin: "routine", thread: { kind: "routineSession", routineId: "r2" } } }));
  put(row("chats", "peer-thread", { deviceId: "host",
    bot: { botId: "ws", origin: "peer", thread: { kind: "peer", connectionId: "c" } } }));
  put(row("chats", "memory", { deviceId: "host", spaceId: "project", memory: { kind: "consolidate" } }));
  put(row("chats", "memory-thread", { deviceId: "host", memory: { kind: "consolidate" },
    bot: { botId: "ws", origin: "routine", thread: { kind: "routine", routineId: "r" } } }));
  const sent: Sent[] = [];
  const service = new Notifications(state, pushEnv(async request => {
    sent.push((await request.json() as { message: Sent }).message);
    return Response.json({ sent: true });
  }), (kind, id) => rows.get(`${kind}/${id}`), () => {});
  // One phone, and NO session target for the workspace's chat: no phone has
  // opened the workspace yet (the desktop never reports it as on screen).
  state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
    JSON.stringify([{ id: "b".repeat(64), lease: "lease", installationId: "phone", epoch: 1 }]));
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
    settings: (body: unknown) => request("settings", "PUT", body),
    /** The host's status report for `chatId`, one second on. */
    event: (chatId: string, status: string) => {
      at(now + 1_000);
      return request("event", "POST", { chatId, deviceId: "host", status, updatedAt: now, startedAt: start });
    },
    /** A registry write of `kind/id` from `device`, merged over what is there. */
    record: (kind: string, id: string, fields: Row["fields"], device = "host") => {
      const before = rows.get(`${kind}/${id}`);
      const after = put(row(kind, id, { ...(before?.fields ?? {}), ...fields }));
      service.observe([{ before, after }], device);
      return after;
    },
    /** The same row arriving again with no earlier copy (a rebuilt registry). */
    replay: (kind: string, id: string, device = "host") => {
      service.observe([{ before: undefined, after: rows.get(`${kind}/${id}`)! }], device);
    },
    seen: (chatId: string, seenAt: number) => {
      const before = rows.get(`chats/${chatId}`)!;
      const after = put({ ...before, fields: { ...before.fields, lastSeenAt: seenAt } });
      service.observe([{ before, after }], "desktop");
    },
    flush: async () => { at(now + 11_000); await service.flush(); }
  };
}

const plan = (createdAt: number) => ({ id: "w1", botId: "ws", title: "Ship it", state: "proposed",
  chatId: "wschat", createdAt, steps: [] });

describe("a workspace's own chat", () => {
  it("notifies like a session, filed under the workspace", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-own-chat")), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event("wschat", "working");
        expect(await f.event("wschat", "awaitingInput")).toEqual({ ok: true, queued: true });
        expect(await f.badge()).toBe(1);
        await f.flush();
        expect(f.alerts()).toEqual([expect.objectContaining({
          chatId: "wschat", projectId: "ws", workspaceId: "ws", kind: "input", badgeCount: 1 })]);
        expect(f.alerts()[0]).not.toHaveProperty("itemKind");

        // Finishing a turn notifies too (the registry mirror path).
        f.at(f.now + 60_000);
        const asking = row("sessions", "wschat", { chatId: "wschat", deviceId: "host", status: "working",
          startedAt: f.now - 60_000, updatedAt: f.now - 1 });
        const done = row("sessions", "wschat", { ...asking.fields, status: "idle", updatedAt: f.now });
        f.rows.set("sessions/wschat", done);
        f.service.observe([{ before: asking, after: done }], "host");
        await f.flush();
        expect(f.alerts().map(m => [m.chatId, m.kind])).toEqual([["wschat", "input"], ["wschat", "completed"]]);

        // Muting the workspace (its bot id is a valid muted "project").
        await f.settings({ ...defaultNotificationSettings(), mutedProjects: ["ws"] });
        expect(await f.badge()).toBe(0);
        await f.event("wschat", "working");
        await f.event("wschat", "awaitingInput");
        await f.flush();
        expect(f.alerts()).toHaveLength(2);
      } finally { f.restore(); }
    });
  });

  it("other project-less chats still never notify", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-projectless")), async (_, state) => {
      const f = fixture(state);
      try {
        f.rows.set("chats/loose", row("chats", "loose", { deviceId: "host" }));
        // An owner chat marker whose workspace is gone, or names another chat.
        f.rows.set("chats/orphan", row("chats", "orphan", { deviceId: "host", bot: { botId: "gone" } }));
        f.rows.set("chats/impostor", row("chats", "impostor", { deviceId: "host", bot: { botId: "ws" } }));
        for (const id of ["loose", "orphan", "impostor"]) {
          expect(await f.event(id, "awaitingInput")).toEqual({ ok: true, ignored: true, reason: "space" });
        }
        await f.flush();
        expect(f.alerts()).toHaveLength(0);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });
});

describe("Needs you", () => {
  it("a plan awaiting approval is one input notice naming it, not repeated on resync", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-plan")), async (_, state) => {
      const f = fixture(state);
      try {
        f.record("botWork", "w1", plan(f.now));
        expect(await f.badge()).toBe(1);
        // Later syncs of the same waiting plan: an edit, the same row again
        // with no earlier copy, an owner device re-pushing it.
        f.at(f.now + 1_000);
        f.record("botWork", "w1", { title: "Ship it today" });
        f.replay("botWork", "w1");
        f.record("botWork", "w1", { title: "Ship it today" }, "phone");
        await f.flush();
        await f.flush();
        expect(f.alerts()).toEqual([expect.objectContaining({ chatId: "wschat", projectId: "ws",
          workspaceId: "ws", kind: "input", itemKind: "plan", itemId: "w1", badgeCount: 1 })]);
        // Approved anywhere: it stops counting.
        f.record("botWork", "w1", { decision: { approve: true, at: f.now } }, "phone");
        expect(await f.badge()).toBe(0);
        f.record("botWork", "w1", { state: "running", startedAt: f.now });
        f.replay("botWork", "w1");
        await f.flush();
        expect(f.alerts()).toHaveLength(1);
      } finally { f.restore(); }
    });
  });

  it("only the workspace's host proposes, and an old row is a baseline", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-plan-baseline")), async (_, state) => {
      const f = fixture(state);
      try {
        f.record("botWork", "w1", plan(f.now), "phone");
        f.record("botWork", "w2", { ...plan(f.now - 9 * 3_600_000), id: "w2" });
        f.record("botWork", "w3", { ...plan(f.now), id: "w3", botId: "gone" });
        await f.flush();
        expect(f.alerts()).toHaveLength(0);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it("a plan decided inside the alert delay is never sent", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-plan-withdrawn")), async (_, state) => {
      const f = fixture(state);
      try {
        f.record("botWork", "w1", plan(f.now));
        f.at(f.now + 3_000);
        f.record("botWork", "w1", { decision: { approve: false, at: f.now } }, "phone");
        await f.flush();
        expect(f.alerts()).toHaveLength(0);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it("a pending approval and a review draft, each once", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-approval-review")), async (_, state) => {
      const f = fixture(state);
      try {
        f.record("botApprovals", "a1", { id: "a1", botId: "ws", threadChatId: "work-thread",
          action: "Start a session in web", reason: "Coordinate", createdAt: f.now, state: "pending" });
        await f.flush();
        f.record("botApprovals", "a1", { action: "Start a session in web: fix" });
        await f.flush();
        expect(f.alerts().map(m => [m.chatId, m.projectId, m.kind, m.itemKind, m.itemId]))
          .toEqual([["wschat", "ws", "input", "approval", "a1"]]);

        // An incoming question is not the owner's until its answer is drafted.
        const exchange = { id: "x1", botId: "ws", connectionId: "c", direction: "incoming",
          question: "?", state: "working", askedAt: f.now };
        f.record("botExchanges", "x1", exchange);
        f.record("botExchanges", "x2", { ...exchange, id: "x2", direction: "outgoing", state: "awaitingReview" });
        await f.flush();
        expect(f.alerts()).toHaveLength(1);
        f.record("botExchanges", "x1", { state: "awaitingReview", answer: "draft" });
        f.record("botExchanges", "x1", { answer: "draft, edited by the host" });
        await f.flush();
        await f.flush();
        expect(f.alerts().map(m => [m.itemKind, m.itemId])).toEqual([["approval", "a1"], ["review", "x1"]]);
        expect(await f.badge()).toBe(1); // both wait in one chat: one unread conversation
        const claims = () => [...state.storage.sql.exec("SELECT key FROM notify_kv WHERE key LIKE 'needs:%' ORDER BY key")]
          .map(r => r.key);
        expect(claims()).toEqual(["needs:approval:a1", "needs:review:x1"]);
        // The review is the chat's unread mark now; deciding the older
        // approval leaves it, sending the review clears it.
        f.record("botApprovals", "a1", { decision: { approve: true, at: f.now } }, "phone");
        expect(await f.badge()).toBe(1);
        f.record("botExchanges", "x1", { review: { send: true, at: f.now } }, "phone");
        expect(await f.badge()).toBe(0);
        // A decided item's claim is dropped (it can never wait again), and so
        // is a waiting one's when its host prunes the row.
        expect(claims()).toEqual([]);
        f.record("botApprovals", "a2", { ...f.rows.get("botApprovals/a1")!.fields, id: "a2", decision: null });
        expect(claims()).toEqual(["needs:approval:a2"]);
        expect(await f.badge()).toBe(1);
        f.service.observe([{ before: f.rows.get("botApprovals/a2"),
          after: { ...f.rows.get("botApprovals/a2")!, deleted: true, fields: {} } }], "host");
        expect(claims()).toEqual([]);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });

  it.each(["routine-thread", "work-thread", "routine-session"])("a %s asking notifies its workspace's chat", async thread => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName(`ws-asking-${thread}`)), async (_, state) => {
      const f = fixture(state);
      try {
        await f.event(thread, "working");
        expect(await f.event(thread, "awaitingInput")).toEqual({ ok: true, queued: true });
        // The registry mirror of the same question is a duplicate.
        const working = row("sessions", thread, { chatId: thread, deviceId: "host", status: "working",
          startedAt: f.start, updatedAt: f.now - 1 });
        const asking = row("sessions", thread, { ...working.fields, status: "awaitingInput", updatedAt: f.now });
        f.rows.set(`sessions/${thread}`, asking);
        f.service.observe([{ before: working, after: asking }], "host");
        await f.flush();
        expect(f.alerts()).toEqual([expect.objectContaining({ chatId: "wschat", projectId: "ws",
          workspaceId: "ws", kind: "input", itemKind: "asking", itemId: thread })]);
        expect(await f.badge()).toBe(1);
        // Answered (the thread works again): it stops counting.
        f.rows.set(`sessions/${thread}`, row("sessions", thread, { ...asking.fields, status: "working" }));
        await f.event(thread, "working");
        expect(await f.badge()).toBe(0);
        // Its next question is new.
        f.rows.set(`sessions/${thread}`, asking);
        await f.event(thread, "awaitingInput");
        await f.flush();
        expect(f.alerts().map(m => m.itemId)).toEqual([thread, thread]);
      } finally { f.restore(); }
    });
  });

  it("hidden threads that are not asking, peer threads and memory passes never notify", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-quiet-threads")), async (_, state) => {
      const f = fixture(state);
      try {
        for (const status of ["working", "idle", "working", "errored"]) {
          expect(await f.event("routine-thread", status)).not.toHaveProperty("queued");
        }
        for (const id of ["peer-thread", "memory", "memory-thread"]) {
          for (const status of ["working", "awaitingInput", "idle"]) {
            expect(await f.event(id, status)).toEqual({ ok: true, ignored: true, reason: "hidden" });
          }
        }
        await f.flush();
        expect(f.alerts()).toHaveLength(0);
        expect(await f.badge()).toBe(0);
      } finally { f.restore(); }
    });
  });
});

describe("the badge counts conversations, as the Dock does", () => {
  it("everything a workspace waits on is its one chat, and a plan outranks the turn that proposed it", async () => {
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("ws-badge")), async (_, state) => {
      const f = fixture(state);
      try {
        // A project session finished.
        await f.event("one", "working");
        f.at(f.now + 60_000);
        await f.event("one", "idle");
        expect(await f.badge()).toBe(1);
        // The workspace proposes a plan, then its turn ends.
        await f.event("wschat", "working");
        f.at(f.now + 60_000);
        f.record("botWork", "w1", plan(f.now));
        expect(await f.event("wschat", "idle")).toEqual({ ok: true, superseded: true });
        expect(await f.badge()).toBe(2);
        // A routine thread asks too: still the same chat.
        await f.event("routine-thread", "working");
        await f.event("routine-thread", "awaitingInput");
        expect(await f.badge()).toBe(2);
        await f.flush();
        // "one" has no session target here, so only the workspace's alert goes out.
        expect(f.alerts().map(m => [m.chatId, m.kind, m.itemKind])).toEqual([["wschat", "input", "asking"]]);
        // The plan's mark was replaced by the question's: deciding the plan
        // keeps the chat counted while the thread still asks.
        f.record("botWork", "w1", { decision: { approve: true, at: f.now } }, "phone");
        expect(await f.badge()).toBe(2);
        // Opening the workspace's chat on any device reads it.
        f.seen("wschat", f.now + 1_000);
        expect(await f.badge()).toBe(1);
      } finally { f.restore(); }
    });
  });
});

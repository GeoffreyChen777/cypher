import { env, runInDurableObject } from "cloudflare:test";
import { afterEach, expect, it } from "vitest";
import { HubRows, decode, operations, HUB_ROW_BYTES } from "../../src/workspace3-core";
import { WorkspaceHub } from "../../src/workspace3-hub";
import { AUTH_USER_HEADER, AUTH_ORG_HEADER, AUTH_DEADLINE_HEADER } from "../../src/env";

const sockets: WebSocket[] = [];
afterEach(async () => {
  await Promise.all(sockets.splice(0).map(ws => new Promise<void>(resolve => {
    if (ws.readyState === WebSocket.CLOSED) { resolve(); return; }
    ws.addEventListener("close", () => resolve(), { once: true });
    ws.close(1000, "test complete");
  })));
});
function room() { return env.TEST_WORKSPACE3.get(env.TEST_WORKSPACE3.idFromName(crypto.randomUUID())); }
type Frame = Record<string, any>;
async function connect(stub: DurableObjectStub, actor: string, role = "viewer", after = 0) {
  const response = await stub.fetch("https://workspace/ws", { headers: {
    Upgrade: "websocket", [AUTH_USER_HEADER]: "user", [AUTH_ORG_HEADER]: "org", [AUTH_DEADLINE_HEADER]: String(Date.now() + 300000),
  } });
  expect(response.status).toBe(101);
  const ws = response.webSocket!;
  sockets.push(ws);
  ws.accept();
  const queued: Frame[] = [];
  const readers: { matches: (v: Frame) => boolean; receive: (v: Frame) => void }[] = [];
  ws.addEventListener("message", event => {
    const frame = JSON.parse(String(event.data)) as Frame;
    const index = readers.findIndex(r => r.matches(frame));
    if (index >= 0) readers.splice(index, 1)[0].receive(frame); else queued.push(frame);
  });
  const take = (type: string, matches: (v: Frame) => boolean = () => true): Promise<Frame> => {
    const accepts = (v: Frame) => v.type === type && matches(v);
    const index = queued.findIndex(accepts);
    if (index >= 0) return Promise.resolve(queued.splice(index, 1)[0]);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`waiting for ${type}; queued=${JSON.stringify(queued)}`)), 2000);
      readers.push({ matches: accepts, receive: value => { clearTimeout(timer); resolve(value); } });
    });
  };
  const send = (value: Frame) => ws.send(JSON.stringify({ version: 3, ...value }));
  send({ type: "hello", user: "user", org: "org", actor, role, after });
  const welcome = await take("welcome");
  return { ws, send, take, welcome };
}
const op = (id: string, n = 1) => ({
  kind: "chats", id, op: "upsert", hlc: `1700000000000-${String(n).padStart(6, "0")}-host`,
  set: { title: id, deviceId: "host" },
});

it("validates closed v3 envelopes and rejects private override/unsafe JSON shapes", () => {
  expect(() => decode('{"version":2,"type":"hello"}')).toThrow("upgrade_required");
  expect(() => decode('{"version":3,"type":"call","params":1e309}')).toThrow("invalid_number");
  expect(() => decode('{"version":3,"type":"call","params":"\\ud800"}')).toThrow("invalid_unicode");
  expect(() => operations([{ ...op("chat"), clocks: {} }], "host")).toThrow("invalid_fields");
  expect(() => operations([op("chat")], "phone")).toThrow("clock_actor_mismatch");
  expect(() => operations([{ ...op("chat"), kind: "sessions" }], "host")).toThrow("invalid_kind");
});

it("pages current rows without equal-seq loss and rolls a poisoned push back", async () => {
  const stub = room();
  await runInDurableObject(stub, (_, ctx) => {
    const rows = new HubRows(ctx.storage.sql);
    for (let i = 0; i < 80; i++) ctx.storage.transactionSync(() => rows.push([op(`chat-${i}`, i)] as any));
    const all: string[] = [];
    let after = 0;
    for (;;) {
      const page = rows.page(after);
      expect(page.rows.length).toBeLessThanOrEqual(32);
      all.push(...page.rows.map(r => r.id));
      after = page.next;
      if (page.done) break;
    }
    expect(all).toEqual(Array.from({ length: 80 }, (_, i) => `chat-${i}`));
    expect(() => ctx.storage.transactionSync(() => rows.push([
      op("first-new", 10),
      { ...op("poison", 11), set: { title: "x".repeat(HUB_ROW_BYTES) } },
    ] as any))).toThrow("row_too_large");
    expect(rows.head()).toBe(80);
    expect(rows.row("chats", "first-new")).toBeUndefined();
  });
});

it("isolates accounts, emits demand and keeps presence/probes out of durable state", async () => {
  const stub = room(), host = await connect(stub, "host", "host");
  const viewer = await connect(stub, "phone");
  expect((await stub.fetch("https://workspace/ws", { headers: {
    Upgrade: "websocket", [AUTH_USER_HEADER]: "other", [AUTH_DEADLINE_HEADER]: String(Date.now() + 300000),
  } })).status).toBe(403);
  host.send({ type: "push", id: "create", ops: [op("chat")] });
  const pushed = await host.take("pushed");
  expect(pushed.through).toBe(1);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify({
    version: 3, type: "push", id: "create", ops: [op("chat")],
  })));
  expect(pushed.requestHash).toBe([...new Uint8Array(digest)].map(v => v.toString(16).padStart(2, "0")).join(""));
  viewer.send({ type: "watch", chats: ["chat"] });
  expect((await host.take("demand", v => v.chats.includes("chat"))).chats).toEqual(["chat"]);
  let before = 0;
  await runInDurableObject(stub, (_, ctx) => { before = [...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n as number; });
  host.send({ type: "presence", state: { sessions: [{ chatId: "chat", status: "working" }] } });
  const presence = await viewer.take("presence");
  expect(presence.actor).toBe("host");
  expect(presence.expiresAt).toBeGreaterThan(Date.now());
  viewer.send({ type: "probe", id: "quiet" });
  expect((await viewer.take("probeOk")).through).toBe(1);
  await runInDurableObject(stub, (_, ctx) => {
    expect([...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n).toBe(before);
  });
});

it("routes RPC across hibernation with a bounded credit window and no durable per-call writes", async () => {
  const stub = room(), host = await connect(stub, "host", "host"), viewer = await connect(stub, "phone");
  viewer.send({ type: "call", id: "read", target: "host", method: "ReadFile", params: { path: "/tmp/file" } });
  const routed = await viewer.take("routed"), call = await host.take("call");
  expect(call.token).toBe(routed.token);
  let changes = 0;
  await runInDurableObject(stub, (_, ctx) => {
    changes = [...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n as number;
  });
  // A new JS instance has no pending-call map, but the signed route and
  // hibernation attachments still authorize exactly this host/caller pair.
  await runInDurableObject(stub, async (_, ctx) => {
    const fresh = new WorkspaceHub(ctx, env as unknown as import("../../src/env").Env);
    const ws = ctx.getWebSockets().find(s => s.deserializeAttachment()?.peer?.actor === "host")!;
    await fresh.webSocketMessage(ws, JSON.stringify({ version: 3, type: "reply", token: call.token, sequence: 0, done: false, value: "a" }));
  });
  expect((await viewer.take("reply")).value).toBe("a");
  host.send({ type: "reply", token: call.token, sequence: 1, done: false, value: "b" });
  expect((await viewer.take("reply")).value).toBe("b");
  host.send({ type: "reply", token: call.token, sequence: 2, done: true, value: "c" });
  expect((await host.take("error")).code).toBe("rpc_backpressure");
  viewer.send({ type: "ack", token: routed.token, through: 2 });
  expect((await host.take("credit")).through).toBe(2);
  host.send({ type: "reply", token: call.token, sequence: 2, done: true, value: "c" });
  expect((await viewer.take("reply")).done).toBe(true);
  viewer.send({ type: "ack", token: routed.token, through: 3 });
  await host.take("credit");
  viewer.send({ type: "call", id: "read", target: "host", method: "ReadFile", params: {} });
  const next = await viewer.take("routed");
  expect(next.token).not.toBe(routed.token);
  await host.take("call");
  host.send({ type: "reply", token: call.token, sequence: 0, done: true, value: "stale" });
  expect((await host.take("error")).code).toBe("request_closed");
  viewer.send({ type: "cancel", token: next.token });
  await host.take("cancel");
  await runInDurableObject(stub, (_, ctx) => {
    expect([...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n).toBe(changes);
  });
});

it("does not let another host forge a route or turn disconnect into an automatic retry", async () => {
  const stub = room(), host = await connect(stub, "host", "host"), viewer = await connect(stub, "phone");
  const other = await connect(stub, "other", "host");
  viewer.send({ type: "call", id: "effect", target: "host", method: "CreateTerminal", params: {} });
  const routed = await viewer.take("routed");
  await host.take("call");
  other.send({ type: "reply", token: routed.token, sequence: 0, done: true, value: "forged" });
  expect((await other.take("error")).code).toBe("not_rpc_host");
  other.send({ type: "reply", token: `${routed.token}bad`, sequence: 0, done: true, value: "tampered" });
  expect((await other.take("error")).code).toBe("invalid_route");
  host.ws.close(1000, "host gone");
  expect((await viewer.take("error", v => v.id === "effect")).code).toBe("delivery_unknown");
});

it("bounds in-flight calls and releases capacity only by completion/cancel/closure", async () => {
  const stub = room(), host = await connect(stub, "host", "host"), viewer = await connect(stub, "phone");
  let first = "";
  for (let i = 0; i < 8; i++) {
    viewer.send({ type: "call", id: `call-${i}`, target: "host", method: "WatchFile", params: {} });
    const routed = await viewer.take("routed");
    await host.take("call");
    if (i === 0) first = routed.token;
  }
  viewer.send({ type: "call", id: "excess", target: "host", method: "ReadFile", params: {} });
  expect((await viewer.take("error")).code).toBe("rpc_capacity");
  viewer.send({ type: "cancel", token: first });
  await host.take("cancel");
  viewer.send({ type: "call", id: "now-fits", target: "host", method: "ReadFile", params: {} });
  expect((await viewer.take("routed")).id).toBe("now-fits");
  await host.take("call");
});

it("fences writes on an already-open socket after its auth lifetime expires", async () => {
  const stub = room(), host = await connect(stub, "host", "host");
  await runInDurableObject(stub, (_, ctx) => {
    const socket = ctx.getWebSockets()[0];
    socket.serializeAttachment({ ...socket.deserializeAttachment(), expires: Date.now() - 1 });
  });
  host.send({ type: "push", id: "expired", ops: [op("must-not-exist")] });
  expect((await host.take("error")).code).toBe("reauth_required");
  await runInDurableObject(stub, (_, ctx) => {
    const rows = new HubRows(ctx.storage.sql);
    expect(rows.head()).toBe(0);
    expect(rows.row("chats", "must-not-exist")).toBeUndefined();
  });
});

it("streams bounded input across hibernation with independent authenticated credits", async () => {
  const stub = room(), host = await connect(stub, "host", "host"), caller = await connect(stub, "phone");
  const stranger = await connect(stub, "other", "host");
  caller.send({ type: "call", id: "large", target: "host", method: "StartSideChat", params: {}, input: true });
  const { token } = await caller.take("routed");
  expect((await host.take("call")).input).toBe(true);
  const changes = await runInDurableObject(stub, (_, ctx) => [...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n);
  for (let sequence = 0; sequence < 2; sequence++) {
    caller.send({ type: "input", token, sequence, done: false, value: "a".repeat(44000) });
    expect((await host.take("input")).sequence).toBe(sequence);
  }
  caller.send({ type: "input", token, sequence: 2, done: true, value: "end" });
  expect((await caller.take("error")).code).toBe("rpc_backpressure");
  host.send({ type: "reply", token, sequence: 0, done: false, value: "premature" });
  expect((await host.take("error")).code).toBe("input_incomplete");
  stranger.send({ type: "inputAck", token, through: 2 });
  expect((await stranger.take("error")).code).toBe("not_rpc_host");
  host.send({ type: "inputAck", token, through: 3 });
  expect((await host.take("error")).code).toBe("rpc_sequence");
  await runInDurableObject(stub, async (_, ctx) => {
    const fresh = new WorkspaceHub(ctx, env as unknown as import("../../src/env").Env);
    const socket = ctx.getWebSockets().find(s => s.deserializeAttachment()?.peer?.actor === "host")!;
    await fresh.webSocketMessage(socket, JSON.stringify({ version: 3, type: "inputAck", token, through: 2 }));
    expect(JSON.stringify(socket.deserializeAttachment()).length).toBeLessThanOrEqual(2048);
  });
  expect((await caller.take("inputCredit")).through).toBe(2);
  caller.send({ type: "input", token, sequence: 2, done: true, value: "end" });
  expect((await host.take("input")).done).toBe(true);
  caller.send({ type: "input", token, sequence: 3, done: true, value: "again" });
  expect((await caller.take("error")).code).toBe("request_closed");
  host.send({ type: "reply", token, sequence: 0, done: true, value: "accepted" });
  expect((await caller.take("reply")).value).toBe("accepted");
  caller.send({ type: "ack", token, through: 1 });
  await host.take("credit");
  await runInDurableObject(stub, (_, ctx) => {
    expect([...ctx.storage.sql.exec("SELECT total_changes() AS n")][0].n).toBe(changes);
  });
});

it("bounds host work across callers and cancels only a closing caller's original routes", async () => {
  const stub = room(), host = await connect(stub, "host", "host"), a = await connect(stub, "a"), b = await connect(stub, "b");
  const tokens: string[] = [];
  for (let i = 0; i < 8; i++) {
    const caller = i < 4 ? a : b;
    caller.send({ type: "call", id: `call${i}`, target: "host", method: "WatchFile", params: {} });
    tokens.push((await caller.take("routed")).token);
    await host.take("call");
  }
  b.send({ type: "call", id: "extra", target: "host", method: "ReadFile", params: {} });
  expect((await b.take("error")).code).toBe("host_capacity");
  a.ws.close(1000, "caller gone");
  for (const token of tokens.slice(0, 4)) expect((await host.take("cancel")).token).toBe(token);
  b.send({ type: "call", id: "fits", target: "host", method: "ReadFile", params: {} });
  await b.take("routed"); await host.take("call");
});

it("serves notification policy against native metadata in the same account Hub", async () => {
  const stub = room(), host = await connect(stub, "host", "host");
  const clock = "1700000000000-000001-host";
  host.send({ type: "push", id: "project-and-chat", ops: [
    { kind: "spaces", id: "project", op: "upsert", hlc: clock, set: { id: "project", deviceId: "host", path: "/repo" } },
    { kind: "chats", id: "chat", op: "upsert", hlc: clock, set: { id: "chat", deviceId: "host", spaceId: "project", title: "native" } }
  ] });
  await host.take("pushed");
  const call = (path: string, body?: object, user = "user") => stub.fetch(`https://workspace/notifications/${path}`, {
    method: body ? "POST" : "GET",
    headers: { [AUTH_USER_HEADER]: user, [AUTH_ORG_HEADER]: "org", "content-type": "application/json" },
    ...(body ? { body: JSON.stringify(body) } : {})
  });
  expect((await call("settings")).status).toBe(200);
  expect((await call("settings", undefined, "other")).status).toBe(403);
  await runInDurableObject(stub, (_, ctx) => {
    ctx.storage.sql.exec("CREATE TRIGGER fail_notice BEFORE INSERT ON notify_kv WHEN NEW.key='eventStatus:chat' BEGIN SELECT RAISE(ABORT,'fault'); END");
  });
  expect((await call("event", { chatId: "chat", deviceId: "host", status: "working", startedAt: Date.now(), updatedAt: Date.now() })).status).toBe(400);
  await runInDurableObject(stub, (_, ctx) => {
    expect([...ctx.storage.sql.exec("SELECT value FROM notify_kv WHERE key='eventState:chat'")]).toHaveLength(0);
    ctx.storage.sql.exec("DROP TRIGGER fail_notice");
  });
  expect((await call("event", { chatId: "chat", deviceId: "host", status: "working", startedAt: Date.now(), updatedAt: Date.now() })).status).toBe(200);
  await runInDurableObject(stub, (_, ctx) => {
    const value = [...ctx.storage.sql.exec("SELECT value FROM notify_kv WHERE key='eventStatus:chat'")][0]?.value;
    expect(value).toBe('"working"');
    expect([...ctx.storage.sql.exec("SELECT name FROM sqlite_master WHERE name='rows'")]).toHaveLength(0);
  });
});

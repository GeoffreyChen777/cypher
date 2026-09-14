// Tiny fixture only. Never point this at production; quota exhaustion is local-only.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import WebSocket from "ws";

const base = new URL(process.env.CYPHER_DEV_EDGE_URL ?? "http://127.0.0.1:27649");
const local = ["127.0.0.1", "localhost"].includes(base.hostname);
assert(local || base.hostname === "cypher-edge-development.geoffreychen777.workers.dev", "Not a development endpoint");
const token = process.env.CYPHER_DEV_ACCESS_TOKEN;
assert(token, "CYPHER_DEV_ACCESS_TOKEN is required");
const headers = { Authorization: `Bearer ${token}` };
const request = (path, options = {}) => fetch(new URL(path, base), { headers, signal: AbortSignal.timeout(10000), ...options });
const sockets = [];
const timeout = setTimeout(() => { console.error("development smoke timed out"); process.exit(1); }, 30000);
const encode = (kind, header, payload = Buffer.alloc(0)) => {
  const json = Buffer.from(JSON.stringify(header));
  const prefix = Buffer.alloc(5); prefix[0] = kind; prefix.writeUInt32LE(json.length, 1);
  return Buffer.concat([prefix, json, payload]);
};
async function socket(device) {
  const url = new URL(`/chat2/development-smoke/ws?device=${device}`, base);
  url.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("token", token);
  const ws = new WebSocket(url); sockets.push(ws);
  const queue = [], waiters = [];
  ws.on("message", data => {
    const waiter = waiters.shift();
    if (waiter) waiter(data); else queue.push(data);
  });
  await new Promise((resolve, reject) => {
    ws.once("open", resolve);
    ws.once("error", () => reject(new Error("Development WebSocket handshake failed")));
  });
  const raw = async () => queue.shift() ?? await new Promise(resolve => waiters.push(resolve));
  const next = async kind => {
    const data = await raw();
    assert.equal(data[0], kind);
    const size = data.readUInt32LE(1);
    return { header: JSON.parse(data.subarray(5, 5 + size)), payload: data.subarray(5 + size) };
  };
  ws.send(encode(1, { cursor: 0, device }));
  const state = await next(2);
  return { ws, next, raw, state };
}
try {
  const health = await (await request("/health", { headers: {} })).json();
  assert.equal(health.environment, "development");
  for (const authorization of [undefined, "Bearer dev-user@dev-org", "Bearer invalid"]) {
    const result = await request("/registry/dev-org/stats", { headers: authorization ? { Authorization: authorization } : {} });
    assert.equal(result.status, 401);
  }
  for (const path of ["/install.sh", "/releases/manifest.json", "/auth/refresh", "/notifications/revoke"]) {
    assert.equal((await request(path)).status, 404);
  }
  assert.equal((await request("/blob/development-smoke/oversized", { method: "PUT", body: Buffer.alloc(65537) })).status, 413);
  const a = await socket("dev-a"), b = await socket("dev-b");
  const id = randomUUID(), payload = Buffer.from("development-smoke-only");
  a.ws.send(encode(6, { batchId: id }, payload));
  const ack = await a.next(7), row = await b.next(4);
  assert.equal(ack.header.seq, a.state.header.headSeq + 1);
  assert.equal(ack.header.batchId, id);
  assert.deepEqual(row.payload, payload);
  a.ws.send(encode(6, { batchId: id }, payload));
  assert.equal((await a.next(7)).header.dup, true);
  a.ws.send("ping");
  // Runtime auto-response: text ping does not execute a metered handler.
  assert.equal((await a.raw()).toString(), "pong");
  a.ws.send(encode(9, {}));
  assert.equal((await a.next(10)).header.headSeq, ack.header.seq);
  const registry = await request("/registry/dev-org/rows");
  assert.equal(registry.status, 200);
  const budget = (await (await request("/dev/budget")).json()).budget;
  assert(budget.events >= 7 && budget.rows > budget.events);
  if (process.env.DEV_SMOKE_EXHAUST === "1") {
    assert(local, "Quota exhaustion is local-only");
    const replies = await Promise.all(Array.from({ length: 140 }, () => request("/chat2/development-smoke/stats")));
    assert(replies.some(r => r.status === 429));
    // A burst hits the four-operation concurrency cap first. Continue
    // sequentially to prove the persistent minute limit as well.
    for (let i = 0; i < 120; i++) {
      if ((await request("/chat2/development-smoke/stats")).status === 429) break;
    }
    const capped = (await (await request("/dev/budget")).json()).budget;
    assert.equal(capped.minuteEvents, 120, "Concurrent admissions must not lose increments");
    const closed = new Promise(resolve => a.ws.once("close", resolve));
    a.ws.send(encode(9, {}));
    assert.equal(await closed, 1013, "Existing WebSockets must obey the same budget");
    console.log("local concurrent HTTP and existing WebSocket rate limits verified");
  }
  console.log(JSON.stringify({ ok: true, host: base.hostname, budget }));
} finally {
  clearTimeout(timeout);
  for (const ws of sockets) ws.terminate();
}

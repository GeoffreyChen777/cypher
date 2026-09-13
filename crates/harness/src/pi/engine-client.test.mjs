import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { randomBytes } from "node:crypto";
import { mkdirSync, chmodSync, rmSync, realpathSync } from "node:fs";
import { join } from "node:path";
import http from "node:http";
import { connectEngine } from "./engine-client.mjs";

const require = createRequire(join(realpathSync(process.env.PI_PACKAGE_DIR), "package.json"));
const { WebSocketServer } = require("ws");

test("isolated Pi bridge: Unix RPC, streaming, cancellation, disconnect and permissions", async () => {
  const root = `/tmp/cypher-ipc-${process.getuid()}`;
  mkdirSync(root, { mode: 0o700, recursive: true });
  const dir = join(root, randomBytes(16).toString("hex"));
  mkdirSync(dir, { mode: 0o700 });
  const socketPath = join(dir, "engine.sock");
  const server = http.createServer();
  const ws = new WebSocketServer({ server });
  const cancellations = new Set();
  ws.on("connection", (connection) => {
    connection.on("message", (raw) => {
      const { id, method, params, cancel } = JSON.parse(raw.toString());
      if (cancel) { cancellations.add(id); return; }
      if (method === "Echo") connection.send(JSON.stringify({ id, ok: params }));
      else if (method === "Count") {
        connection.send(JSON.stringify({ id, item: 0 }));
        connection.send(JSON.stringify({ id, item: 1 }));
        connection.send(JSON.stringify({ id, done: true }));
      } else if (method !== "Never") connection.send(JSON.stringify({ id, err: "unknown method" }));
    });
  });
  let client;
  try {
    await new Promise((resolve, reject) => {
      server.once("error", reject);
      server.listen(socketPath, resolve);
    });
    chmodSync(socketPath, 0o600);
    client = await connectEngine({ socketPath });
    assert.deepEqual(await client.call("Echo", { value: 42 }), { value: 42 });
    const values = [];
    for await (const item of client.subscribe("Count")) values.push(item);
    assert.deepEqual(values, [0, 1]);
    await assert.rejects(client.call("Unknown"), /unknown method/);
    await assert.rejects(client.call("Never", {}, { timeoutMs: 20 }), /timed out/);
    const abort = new AbortController();
    const stream = client.subscribe("Never", {}, { signal: abort.signal });
    const waiting = stream.next();
    abort.abort();
    await assert.rejects(waiting, /canceled/);
    await new Promise((resolve) => setTimeout(resolve, 30));
    assert.ok(cancellations.size >= 2);
    chmodSync(dir, 0o755);
    await assert.rejects(connectEngine({ socketPath }), /not private/);
    chmodSync(dir, 0o700);
    await assert.rejects(connectEngine({ socketPath: "ws://127.0.0.1:27654" }), /Invalid/);
    const disconnected = client.call("Never");
    client.close();
    await assert.rejects(disconnected, /disconnected/);
  } finally {
    client?.close();
    for (const connection of ws.clients) connection.terminate();
    await new Promise((resolve) => ws.close(resolve));
    await new Promise((resolve) => server.close(resolve));
    rmSync(dir, { recursive: true, force: true });
  }
});

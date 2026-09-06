// Engine-owned adapter, injected as CYPHER_ENGINE_CLIENT_MODULE. It uses the
// locked ws dependency inside the isolated Pi Runtime, not system/global npm.
import { createRequire } from "node:module";
import { lstatSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";

function validateSocket(path) {
  if (typeof path !== "string" || !path.startsWith(`/tmp/cypher-ipc-${process.getuid()}/`)
      || Buffer.byteLength(path) > 100 || !/\/[a-f0-9]{32}\/engine\.sock$/.test(path)) {
    throw new Error("Invalid private Engine socket");
  }
  for (const [entry, socket] of [[dirname(dirname(path)), false], [dirname(path), false], [path, true]]) {
    const stat = lstatSync(entry);
    if (stat.uid !== process.getuid() || (stat.mode & 0o077) !== 0
        || (socket ? !stat.isSocket() : !stat.isDirectory())) {
      throw new Error("Engine socket is not private to this user");
    }
  }
}

export async function connectEngine({ socketPath = process.env.CYPHER_ENGINE_SOCKET,
  timeoutMs = 5000 } = {}) {
  validateSocket(socketPath);
  if (!process.env.PI_PACKAGE_DIR) throw new Error("Isolated Pi Runtime package directory is missing");
  const require = createRequire(join(realpathSync(process.env.PI_PACKAGE_DIR), "package.json"));
  const WebSocket = require("ws");
  const socket = new WebSocket(`ws+unix://${socketPath}:/ipc`, "cypher.rpc.v1", {
    handshakeTimeout: timeoutMs, maxPayload: 1024 * 1024, perMessageDeflate: false,
  });
  await new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", () => reject(new Error("Engine IPC handshake failed")));
  });
  let nextId = 1;
  const pending = new Map();
  const failAll = () => {
    for (const entry of pending.values()) entry.fail(new Error("Engine IPC disconnected"));
    pending.clear();
  };
  socket.on("close", failAll);
  socket.on("error", failAll);
  socket.on("message", (data) => {
    try {
      for (const line of data.toString().split("\n").filter(Boolean)) {
        const frame = JSON.parse(line);
        pending.get(frame.id)?.frame(frame);
      }
    } catch { socket.terminate(); failAll(); }
  });
  function send(frame) {
    if (socket.readyState !== WebSocket.OPEN) throw new Error("Engine IPC disconnected");
    socket.send(JSON.stringify(frame));
  }
  return {
    call(method, params = {}, { timeoutMs: requestTimeout = 10000 } = {}) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const finish = (error, value) => {
          clearTimeout(timer); pending.delete(id);
          if (error) reject(error); else resolve(value);
        };
        const timer = setTimeout(() => {
          try { send({ id, cancel: true }); } catch {}
          finish(new Error("Engine IPC request timed out"));
        }, requestTimeout);
        pending.set(id, { fail: finish, frame(frame) {
          if (frame.err !== undefined) finish(new Error(frame.err));
          else if ("ok" in frame) finish(null, frame.ok);
        } });
        try { send({ id, method, params }); } catch (error) { finish(error); }
      });
    },
    async *subscribe(method, params = {}, { signal } = {}) {
      const id = nextId++;
      const queue = [];
      let wake, ended = false, failure;
      const notify = () => { wake?.(); wake = undefined; };
      const fail = (error) => { failure = error; ended = true; notify(); };
      const abort = () => fail(new Error("Engine IPC subscription canceled"));
      pending.set(id, { fail, frame(frame) {
        if (frame.err !== undefined) return fail(new Error(frame.err));
        if ("item" in frame) {
          if (queue.length >= 256) return fail(new Error("Engine IPC subscription overflow"));
          queue.push(frame.item);
        }
        if (frame.done || "ok" in frame) ended = true;
        notify();
      } });
      signal?.addEventListener("abort", abort, { once: true });
      try {
        if (signal?.aborted) abort();
        if (!ended) send({ id, method, params });
        while (true) {
          if (failure) throw failure;
          if (queue.length) { yield queue.shift(); continue; }
          if (ended) return;
          await new Promise((resolve) => { wake = resolve; });
        }
      } finally {
        pending.delete(id);
        signal?.removeEventListener("abort", abort);
        try { send({ id, cancel: true }); } catch {}
      }
    },
    close() { socket.terminate(); failAll(); },
  };
}

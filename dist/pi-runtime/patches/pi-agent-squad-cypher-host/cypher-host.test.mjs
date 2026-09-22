// Unit tests for the Cypher host patch (`cypher-host.ts`), against a fake
// engine client — no engine, Pi, or network. Run: node --test <this file>.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CypherHostUnavailable,
  cypherHostAvailable,
  spawnCypherHostedSubagent,
  taskLabel,
} from "./cypher-host.ts";

const ENV = {
  CYPHER_SUBAGENT_BRIDGE: "2",
  CYPHER_ENGINE_SOCKET: "/tmp/cypher-ipc-501/0123456789abcdef0123456789abcdef/engine.sock",
  CYPHER_CHAT_ID: "parent-chat",
  CYPHER_ENGINE_CLIENT_MODULE: "/tmp/engine-client.mjs",
};

/** A scripted engine: `StartSubagent` answers, `WatchAgentEvents` yields
 * whatever the test pushes, `QueueCommand` is recorded. */
function fakeEngine({ start = async () => ({ childChatId: "child-1" }) } = {}) {
  const calls = [];
  const commands = [];
  let push;
  let end;
  const pending = [];
  let subscriptions = 0;
  let closed = false;
  const client = {
    async call(method, params) {
      calls.push({ method, params });
      if (method === "StartSubagent") return await start(params);
      if (method === "QueueCommand") {
        commands.push(params.command);
        return { commandId: `cmd-${commands.length}` };
      }
      throw new Error(`unexpected ${method}`);
    },
    async *subscribe(method, params, { signal } = {}) {
      assert.equal(method, "WatchAgentEvents");
      assert.deepEqual(params, { chatId: "child-1" });
      subscriptions++;
      const queue = [...pending];
      let wake;
      let ended = false;
      push = (event) => {
        queue.push(event);
        wake?.();
      };
      end = () => {
        ended = true;
        wake?.();
      };
      signal?.addEventListener("abort", () => end(), { once: true });
      while (true) {
        if (queue.length) {
          yield queue.shift();
          continue;
        }
        if (ended) return;
        await new Promise((resolve) => {
          wake = resolve;
        });
        wake = undefined;
      }
    },
    close() {
      closed = true;
    },
  };
  return {
    client,
    calls,
    commands,
    get subscriptions() {
      return subscriptions;
    },
    get closed() {
      return closed;
    },
    /** Events for the live stream (buffered until someone subscribes). */
    emit(...events) {
      for (const event of events) {
        pending.push(event);
        push?.(event);
      }
    },
    /** Drop the current stream (a re-subscription replays `pending`). */
    drop() {
      end?.();
    },
  };
}

const AGENT = {
  name: "planner",
  systemPrompt: "You plan.",
  tools: ["read", "bash"],
  model: "anthropic/claude-sonnet-4",
};

function options(extra = {}) {
  return {
    agent: AGENT,
    task: "Plan the panel",
    address: "planner",
    messageRoot: "/tmp/pi-subagents-messages/session",
    runId: "run-1",
    childIndex: 0,
    ...extra,
  };
}

const tick = () => new Promise((resolve) => setImmediate(resolve));

test("hosting needs a bridge version this module speaks", () => {
  assert.equal(cypherHostAvailable(ENV), true);
  assert.equal(cypherHostAvailable({ ...ENV, CYPHER_SUBAGENT_BRIDGE: "1" }), false, "an older engine truncates tasks");
  assert.equal(cypherHostAvailable({ ...ENV, CYPHER_SUBAGENT_BRIDGE: undefined }), false);
  assert.equal(cypherHostAvailable({ ...ENV, CYPHER_CHAT_ID: "" }), false, "discovery processes have no chat");
  assert.equal(cypherHostAvailable({ ...ENV, CYPHER_ENGINE_CLIENT_MODULE: undefined }), false);
});

test("a run becomes a child chat and answers with the child's final text", async () => {
  const engine = fakeEngine();
  const linked = [];
  const events = [];
  let started = false;
  let session;
  const run = spawnCypherHostedSubagent(
    options({
      onStarted: () => {
        started = true;
      },
      onSession: (s) => {
        session = s;
      },
      onEvent: (event) => events.push(event),
    }),
    { mode: "sync", toolCallId: "toolu_1", onChildChat: (id) => linked.push(id) },
    { env: ENV, connect: async () => engine.client },
  );
  await tick();
  engine.emit(
    { type: "sessionStarted", harness: "pi", model: "anthropic/claude-opus-5", tools: [], cwd: "/r", sessionId: "s", assistantMessageId: "a" },
    { type: "toolCall", id: "t1", call: { kind: "exec", command: "cargo test" } },
    { type: "textDelta", text: "Looking around. " },
    { type: "assistantMessageCompleted", assistantMessageId: "a" },
    { type: "textDelta", text: "The plan is ready." },
    { type: "done", status: "completed", result: "The plan is ready.", error: null, sessionId: "s" },
  );
  const result = await run;

  const start = engine.calls.find((c) => c.method === "StartSubagent").params;
  assert.deepEqual(start, {
    parentChatId: "parent-chat",
    runId: "run-1",
    agent: "planner",
    task: "Plan the panel",
    mode: "sync",
    toolCallId: "toolu_1",
    systemPrompt: "You plan.",
    tools: ["read", "bash"],
    model: "anthropic/claude-sonnet-4",
    messageRoot: "/tmp/pi-subagents-messages/session",
    childIndex: 0,
  });
  assert.deepEqual(linked, ["child-1"], "the inspector row links to the child chat");
  assert.equal(started, true);
  assert.equal(session.childChatId, "child-1");
  assert.equal(result.exitCode, 0);
  assert.equal(result.model, "anthropic/claude-opus-5", "the model the child actually ran");
  assert.deepEqual(result.messages, [
    { role: "assistant", content: [{ type: "text", text: "The plan is ready." }], model: "anthropic/claude-opus-5" },
  ]);
  assert.deepEqual(events.find((e) => e.type === "tool_execution_start"), {
    type: "tool_execution_start",
    toolName: "exec",
    args: { kind: "exec", command: "cargo test" },
  });
  assert.ok(events.some((e) => e.type === "message_end"), "progress sees assistant headlines");
  assert.equal(engine.closed, true);
});

test("a second concurrent agent keeps its name and messages under its address", async () => {
  const engine = fakeEngine();
  const run = spawnCypherHostedSubagent(options({ address: "planner#1a2b3c4d" }), { mode: "async" }, { env: ENV, connect: async () => engine.client });
  await tick();
  engine.emit({ type: "done", status: "completed", result: "ok", error: null, sessionId: null });
  await run;
  assert.equal(engine.calls[0].params.agent, "planner");
  assert.equal(engine.calls[0].params.address, "planner#1a2b3c4d");
});

test("a long task keeps a short label and sends the whole text as the prompt", async () => {
  const engine = fakeEngine();
  const task = `Plan it.\n${"Detail line. ".repeat(100)}`;
  const run = spawnCypherHostedSubagent(options({ task }), { mode: "async" }, { env: ENV, connect: async () => engine.client });
  await tick();
  engine.emit({ type: "done", status: "completed", result: "ok", error: null, sessionId: null });
  await run;
  const start = engine.calls[0].params;
  assert.equal(start.prompt, task);
  assert.ok([...start.task].length <= 500);
  assert.ok(!start.task.includes("\n"));
  assert.equal(start.toolCallId, undefined, "a retry has no parent tool call");
  assert.equal(taskLabel("  a \n b  "), "a b");
});

test("nothing started: an unreachable engine or a refusal falls back", async () => {
  await assert.rejects(
    spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => { throw new Error("ECONNREFUSED"); } }),
    CypherHostUnavailable,
  );
  const refused = fakeEngine({ start: async () => { throw new Error("parent chat not found"); } });
  await assert.rejects(
    spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => refused.client }),
    CypherHostUnavailable,
  );
  assert.equal(refused.closed, true);
});

test("a lost start reply fails the run instead of running it twice", async () => {
  const engine = fakeEngine({ start: async () => { throw new Error("Engine IPC request timed out"); } });
  await assert.rejects(
    spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => engine.client }),
    (error) => !(error instanceof CypherHostUnavailable) && /could not confirm/.test(error.message),
  );
});

test("cancelling the parent interrupts the child chat", async () => {
  const engine = fakeEngine();
  const controller = new AbortController();
  const run = spawnCypherHostedSubagent(options({ signal: controller.signal }), { mode: "sync" }, { env: ENV, connect: async () => engine.client });
  await tick();
  engine.emit({ type: "sessionStarted", harness: "pi", model: "m", tools: [], cwd: "/", sessionId: "s", assistantMessageId: "a" });
  await tick();
  controller.abort();
  await tick();
  engine.emit({ type: "done", status: "interrupted", result: null, error: null, sessionId: "s" });
  await assert.rejects(run, /Subagent was aborted/);
  assert.deepEqual(engine.commands, [{ kind: "interrupt" }]);
});

test("a timeout interrupts the child and reports exit 124", async () => {
  const engine = fakeEngine();
  const run = spawnCypherHostedSubagent(options({ timeoutMs: 1000 }), { mode: "async" }, { env: ENV, connect: async () => engine.client });
  setTimeout(() => engine.emit({ type: "done", status: "interrupted", result: null, error: null, sessionId: null }), 1100);
  const result = await run;
  assert.equal(result.exitCode, 124);
  assert.match(result.errorMessage, /timed out/);
  assert.deepEqual(engine.commands, [{ kind: "interrupt" }]);
});

test("the child's own terminal status becomes the result", async () => {
  const errored = fakeEngine();
  const run = spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => errored.client });
  await tick();
  errored.emit({ type: "done", status: "errored", result: null, error: "model overloaded", sessionId: null });
  const failed = await run;
  assert.equal(failed.stopReason, "error");
  assert.equal(failed.errorMessage, "model overloaded");

  // Stopped by the user from the child's own chat in Cypher.
  const stopped = fakeEngine();
  const run2 = spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => stopped.client });
  await tick();
  stopped.emit({ type: "done", status: "interrupted", result: null, error: null, sessionId: null });
  const aborted = await run2;
  assert.equal(aborted.exitCode, 130);
  assert.equal(aborted.stopReason, "aborted");
  assert.deepEqual(stopped.commands, [], "the parent did not interrupt anything");
});

test("a dropped event stream re-subscribes and still sees the end", async () => {
  const engine = fakeEngine();
  const run = spawnCypherHostedSubagent(options(), { mode: "sync" }, { env: ENV, connect: async () => engine.client });
  await tick();
  engine.emit({ type: "textDelta", text: "partial " });
  await tick();
  engine.drop();
  await new Promise((resolve) => setTimeout(resolve, 600));
  engine.emit({ type: "textDelta", text: "answer" }, { type: "done", status: "completed", result: null, error: null, sessionId: null });
  const result = await run;
  assert.equal(engine.subscriptions, 2);
  assert.deepEqual(
    result.messages.map((m) => m.content[0].text),
    ["partial answer"],
    "the replay replaces, not repeats, what was already streamed",
  );
});

test("a routed message steers the child and waits for its reply", async () => {
  const engine = fakeEngine();
  let session;
  const run = spawnCypherHostedSubagent(
    options({ onSession: (s) => { session = s; } }),
    { mode: "sync" },
    { env: ENV, connect: async () => engine.client },
  );
  await tick();
  engine.emit({ type: "sessionStarted", harness: "pi", model: "m", tools: [], cwd: "/", sessionId: "s", assistantMessageId: "a" });
  await tick();
  const reply = session.sendAndWait("How far along?", 5000);
  await tick();
  assert.deepEqual(engine.commands, [{ kind: "steer", prompt: "How far along?", messageId: null }]);
  assert.equal(await session.isStreaming(), true);
  engine.emit({ type: "textDelta", text: "Halfway." }, { type: "done", status: "completed", result: "Halfway.", error: null, sessionId: "s" });
  assert.equal(await reply, "Halfway.");
  await run;
  await assert.rejects(session.send("late"), /ended/);
});

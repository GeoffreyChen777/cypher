import assert from "node:assert/strict";
import { test } from "node:test";

import explain, {
  EXPLAIN_TITLE,
  parseArgs,
  sessionState,
  STATE_ENTRY_TYPE,
  trackingBlock,
  withTracking,
} from "./explain.ts";

const entry = (data) => ({ type: "custom", customType: STATE_ENTRY_TYPE, data });

test("arguments: on, off, status, or a focus", () => {
  assert.deepEqual(parseArgs(""), { action: "on" });
  assert.deepEqual(parseArgs(" ON "), { action: "on" });
  assert.deepEqual(parseArgs("off"), { action: "off" });
  assert.deepEqual(parseArgs("status"), { action: "status" });
  assert.deepEqual(parseArgs("the sync engine"), { action: "on", focus: "the sync engine" });
});

test("the last state on the branch wins; off without one", () => {
  assert.deepEqual(sessionState([]), { enabled: false });
  assert.deepEqual(
    sessionState([
      entry({ version: 1, enabled: true, focus: "sync" }),
      { type: "message" },
      entry({ version: 2, enabled: false }),
      entry({ version: 1, enabled: "yes" }),
    ]),
    { enabled: true, focus: "sync" },
  );
  assert.deepEqual(sessionState([entry({ version: 1, enabled: true }), entry({ version: 1, enabled: false })]), {
    enabled: false,
    focus: undefined,
  });
});

test("the tracking block goes after the appended prompt, once", () => {
  const once = withTracking("<cypher_rendering>x</cypher_rendering>");
  assert.ok(once.startsWith("<cypher_rendering>"));
  assert.ok(once.endsWith("</cypher_explain>"));
  assert.equal(withTracking(once), once);
  assert.ok(trackingBlock().includes(`"${EXPLAIN_TITLE}"`));
  assert.ok(trackingBlock("sync").includes("focus on: sync"));
});

function harness(entries = []) {
  const calls = { entries: [], sent: [], notes: [], commands: {}, handlers: {} };
  const pi = {
    registerCommand: (name, options) => (calls.commands[name] = options),
    on: (event, handler) => (calls.handlers[event] = handler),
    appendEntry: (type, data) => calls.entries.push({ type, data }),
    sendUserMessage: (content, options) => calls.sent.push({ content, options }),
  };
  let idle = true;
  const ctx = {
    isIdle: () => idle,
    ui: { notify: (message) => calls.notes.push(message) },
    sessionManager: { getBranch: () => entries },
  };
  explain(pi);
  calls.handlers.session_start({}, ctx);
  const prompt = () => {
    const event = { systemPromptOptions: { appendSystemPrompt: "rest" } };
    calls.handlers.before_agent_start(event);
    return event.systemPromptOptions.appendSystemPrompt;
  };
  return { calls, ctx, prompt, setIdle: (value) => (idle = value) };
}

test("/explain writes the page now and keeps it tracked until /explain off", async () => {
  const { calls, ctx, prompt, setIdle } = harness();
  assert.equal(prompt(), "rest");
  await calls.commands.explain.handler("the sync engine", ctx);
  assert.deepEqual(calls.entries.at(-1), {
    type: STATE_ENTRY_TYPE,
    data: { version: 1, enabled: true, focus: "the sync engine" },
  });
  assert.equal(calls.sent.length, 1);
  assert.ok(calls.sent[0].content.includes(`title="${EXPLAIN_TITLE}"`));
  assert.ok(calls.sent[0].content.includes("the sync engine"));
  assert.equal(calls.sent[0].options, undefined);
  assert.ok(prompt().includes("<cypher_explain>"));

  // Mid-turn, the request queues behind the running turn; the focus stays.
  setIdle(false);
  await calls.commands.explain.handler("", ctx);
  assert.deepEqual(calls.sent[1].options, { deliverAs: "followUp" });
  assert.ok(calls.sent[1].content.includes("the sync engine"));

  await calls.commands.explain.handler("off", ctx);
  assert.equal(calls.entries.at(-1).data.enabled, false);
  assert.equal(calls.sent.length, 2);
  assert.equal(prompt(), "rest");
});

test("a resumed session picks its state up", () => {
  const { prompt } = harness([entry({ version: 1, enabled: true })]);
  assert.ok(prompt().includes("<cypher_explain>"));
});

test("completions offer the choices", () => {
  const { calls } = harness();
  const complete = calls.commands.explain.getArgumentCompletions;
  assert.deepEqual(
    complete("o").map((choice) => choice.value),
    ["on", "off"],
  );
  assert.equal(complete("x"), null);
});

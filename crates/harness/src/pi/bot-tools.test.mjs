// The workspace tools as Pi runs them: a refused call is an error result
// with the engine's words, and propose_plan's schema carries the engine's
// caps. Needs no install: `typebox` is a stand-in that builds the same JSON
// Schema shapes, and the engine is a stub module.
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const TYPEBOX = `
const OPTIONAL = Symbol.for("bot-tools.test.optional");
const required = (properties) => Object.keys(properties).filter((key) => !properties[key][OPTIONAL]);
export const Type = {
  Object: (properties, options = {}) => ({ type: "object", properties, required: required(properties), ...options }),
  Optional: (schema) => ({ ...schema, [OPTIONAL]: true }),
  String: (options = {}) => ({ type: "string", ...options }),
  Number: (options = {}) => ({ type: "number", ...options }),
  Integer: (options = {}) => ({ type: "integer", ...options }),
  Boolean: (options = {}) => ({ type: "boolean", ...options }),
  Literal: (value) => ({ const: value }),
  Union: (anyOf, options = {}) => ({ anyOf, ...options }),
  Array: (items, options = {}) => ({ type: "array", items, ...options }),
};
`;
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "typebox") {
      return { url: `data:text/javascript,${encodeURIComponent(TYPEBOX)}`, shortCircuit: true };
    }
    return next(specifier, context);
  },
});

// The engine: each test sets what `BotTool` answers.
const dir = mkdtempSync(join(tmpdir(), "cypher-bot-tools-"));
const client = join(dir, "engine-client.mjs");
writeFileSync(
  client,
  "export async function connectEngine() { return { call: (...args) => globalThis.botToolsEngine(...args) }; }\n",
);
process.env.CYPHER_ENGINE_CLIENT_MODULE = client;
process.env.CYPHER_BOT_ID = "bot-1";
process.env.CYPHER_CHAT_ID = "bot-chat";

const { default: extension, PLAN_LIMITS } = await import("./bot-tools.ts");
const tools = new Map();
const handlers = new Map();
extension({
  on: (event, handler) => handlers.set(event, handler),
  appendEntry() {},
  registerTool: (tool) => tools.set(tool.name, tool),
});

const plan = { title: "Fix rounding", steps: [{ title: "Fix it", detail: "x".repeat(1_600) }] };
const REFUSAL = "Step 1's detail has 1600 characters; a step's detail holds at most 1500.";

test("a refusal is an error result with the engine's words", async () => {
  const calls = [];
  // The engine answers a refusal (rpc/bots.rs `bot_tool`), flagged.
  globalThis.botToolsEngine = async (method, params) => {
    calls.push({ method, params });
    return { text: REFUSAL, isError: true };
  };
  const result = await tools.get("propose_plan").execute("call-1", plan);
  assert.equal(result.isError, true);
  assert.deepEqual(result.content, [{ type: "text", text: REFUSAL }]);
  assert.deepEqual(calls, [
    { method: "BotTool", params: { chatId: "bot-chat", tool: "propose_plan", args: plan, toolCallId: "call-1" } },
  ]);
  // Every workspace tool is answered the same way.
  const finished = await tools.get("finish_work").execute("call-2", { outcome: "done", result: "r" });
  assert.equal(finished.isError, true);
  assert.equal(finished.content[0].text, REFUSAL);
});

test("a call that fails on the way is an error result naming the tool", async () => {
  globalThis.botToolsEngine = async () => {
    throw new Error("Engine IPC request timed out");
  };
  const result = await tools.get("propose_plan").execute("call-3", plan);
  assert.equal(result.isError, true);
  assert.deepEqual(result.content, [{ type: "text", text: "Propose a plan failed: Engine IPC request timed out" }]);
});

test("an answer is an ordinary result", async () => {
  const made = "The plan \"Fix rounding\" (work w1) is waiting for your owner's approval.";
  globalThis.botToolsEngine = async () => ({ text: made });
  const result = await tools.get("propose_plan").execute("call-4", plan);
  assert.equal(result.isError, undefined);
  assert.deepEqual(result.content, [{ type: "text", text: made }]);
});

test("the direct-tool gate lets a call through only on allow", async () => {
  const gate = handlers.get("tool_call");
  globalThis.botToolsEngine = async () => ({ text: "allow" });
  assert.equal(await gate({ toolName: "bash", input: { command: "ls" } }), undefined);
  assert.equal(await gate({ toolName: "memory", input: {} }), undefined, "not one of Pi's own tools");
  globalThis.botToolsEngine = async () => ({ text: "Work directly is off.", isError: true });
  assert.deepEqual(await gate({ toolName: "bash", input: { command: "ls" } }), { block: true, reason: "Work directly is off." });
  globalThis.botToolsEngine = async () => ({ text: "allow", isError: true });
  assert.equal((await gate({ toolName: "bash", input: { command: "ls" } })).block, true, "a flagged answer never allows");
});

test("propose_plan's schema carries the engine's caps", () => {
  const schema = tools.get("propose_plan").parameters;
  const step = schema.properties.steps.items;
  assert.equal(schema.properties.title.maxLength, PLAN_LIMITS.TITLE_MAX);
  assert.equal(schema.properties.summary.maxLength, PLAN_LIMITS.DETAIL_MAX);
  assert.equal(step.properties.title.maxLength, PLAN_LIMITS.STEP_TITLE_MAX);
  assert.equal(step.properties.detail.maxLength, PLAN_LIMITS.PLAN_BODY_MAX);
  assert.equal(schema.properties.steps.maxItems, PLAN_LIMITS.PLAN_STEPS_MAX);
  const limit = schema.properties.time_limit_minutes;
  assert.deepEqual(
    [limit.minimum, limit.maximum],
    [PLAN_LIMITS.PLAN_TIME_LIMIT_MIN_MINUTES, PLAN_LIMITS.PLAN_TIME_LIMIT_MAX_MINUTES],
  );
  // The descriptions say the same numbers.
  assert.match(schema.properties.title.description, /at most 120 characters/);
  assert.match(step.properties.detail.description, /at most 30,000 characters across the plan$/);
  const result = tools.get("finish_work").parameters.properties.result;
  assert.equal(result.maxLength, PLAN_LIMITS.RESULT_BODY_MAX);
  assert.match(result.description, /^Concise: .*at most 30,000 characters$/);
  assert.match(limit.description, /15 to 4320 \(72 hours\)/);
});

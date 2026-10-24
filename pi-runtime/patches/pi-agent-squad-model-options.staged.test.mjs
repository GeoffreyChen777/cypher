// Tests for the model-options patch: the staged pi-agent-squad, loaded
// through Pi's own extension loader, launches a `subagent` run with the
// caller's model and thinking level — or refuses a bad one before any run
// starts. The spawner is a stub; no network, provider or child process.
//
// Run against a staged runtime whose pi-agent-squad is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");

// Keep Pi and the squad off the real agent directory, and the run local.
const scratch = mkdtempSync(join(tmpdir(), "pi-agent-squad-model-options-"));
process.env.PI_CODING_AGENT_DIR = join(scratch, "agent");
process.env.TMPDIR = scratch; // the squad's message root
delete process.env.CYPHER_SUBAGENT_BRIDGE;

const pi = await import(
  pathToFileURL(join(modules, "@earendil-works/pi-coding-agent", "dist", "core", "extensions", "index.js")).href
);
const piRequire = createRequire(join(modules, "@earendil-works/pi-coding-agent", "package.json"));
const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
const { createAgentSquadExtension } = await createJiti(import.meta.url).import(
  join(modules, "pi-agent-squad", "index.ts"),
);

const spawned = [];
const runtime = pi.createExtensionRuntime();
const extension = await pi.loadExtensionFromFactory(
  (api) =>
    createAgentSquadExtension(api, {
      spawnInteractiveSubagent: async (opts) => {
        spawned.push(opts.agent);
        return {
          agent: opts.agent.name,
          task: opts.task,
          exitCode: 0,
          messages: [{ role: "assistant", content: [{ type: "text", text: "done" }] }],
          stderr: "",
          usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0, contextTokens: 0, turns: 1 },
          model: opts.agent.model,
          stopReason: "stop",
        };
      },
    }),
  scratch,
  undefined,
  runtime,
);
const tool = extension.tools.get("subagent")?.definition;
assert.ok(tool, "the squad registers `subagent`");

const MODELS = [
  { provider: "anthropic", id: "claude-sonnet-5-5" },
  { provider: "anthropic", id: "claude-opus-5-5" },
  { provider: "openai", id: "gpt-6" },
  { provider: "relay", id: "gpt-6" },
];
const ctx = {
  cwd: scratch,
  hasUI: false,
  model: { provider: "anthropic", id: "claude-opus-5-5" },
  modelRegistry: { getAvailable: () => MODELS },
  sessionManager: { getEntries: () => [], getBranch: () => [], getSessionId: () => "test" },
  ui: new Proxy({}, { get: () => () => undefined }),
};

/** Fire the squad's own handlers for one session lifecycle event. */
async function emit(type) {
  for (const handler of extension.handlers.get(type) ?? []) await handler({ type }, ctx);
}
// The squad takes tasks only inside a session.
await emit("session_start");
after(async () => {
  await emit("session_shutdown");
  rmSync(scratch, { recursive: true, force: true });
});

/** One sync `subagent` call: the agent the run launched with, or the refusal text. */
async function call(params) {
  spawned.length = 0;
  const result = await tool.execute(`call-${Math.random()}`, { agent: "planner", task: "t", ...params }, undefined, undefined, ctx);
  const text = result.content.map((part) => part.text).join("");
  assert.ok(spawned.length <= 1);
  return { launched: spawned[0], text };
}

test("the tool declares model and thinking", () => {
  assert.equal(tool.parameters.properties.model.type, "string");
  assert.deepEqual(tool.parameters.properties.thinking.enum, ["off", "minimal", "low", "medium", "high", "xhigh", "max"]);
  assert.ok(!tool.parameters.required.includes("model") && !tool.parameters.required.includes("thinking"));
});

test("without either, the run uses the agent definition", async () => {
  const { launched } = await call({});
  assert.equal(launched.model, undefined);
  assert.equal(launched.thinking, "xhigh", "planner.md's own level");
});

test("a full model id launches as given, beside the agent's thinking", async () => {
  const { launched, text } = await call({ model: "anthropic/claude-sonnet-5-5" });
  assert.equal(text, "done");
  assert.equal(launched.model, "anthropic/claude-sonnet-5-5");
  assert.equal(launched.thinking, "xhigh");
  assert.equal(launched.name, "planner");
});

test("a bare id resolves when one provider offers it", async () => {
  assert.equal((await call({ model: "claude-sonnet-5-5" })).launched.model, "anthropic/claude-sonnet-5-5");
  assert.equal((await call({ model: "Anthropic/Claude-Sonnet-5-5" })).launched.model, "anthropic/claude-sonnet-5-5");
});

test("an ambiguous or unknown model is refused before a run starts", async () => {
  const ambiguous = await call({ model: "gpt-6" });
  assert.equal(ambiguous.launched, undefined);
  assert.match(ambiguous.text, /several providers; use one of: openai\/gpt-6, relay\/gpt-6/);
  const unknown = await call({ model: "nope/model" });
  assert.equal(unknown.launched, undefined);
  assert.match(unknown.text, /Unknown model "nope\/model"\. Available: anthropic\/claude-sonnet-5-5/);
});

test("thinking replaces the agent's level and keeps a model beside it", async () => {
  const both = await call({ model: "openai/gpt-6", thinking: "low" });
  assert.deepEqual([both.launched.model, both.launched.thinking], ["openai/gpt-6", "low"]);
  // No model named: Cypher needs one for --thinking, so the caller's current model.
  const only = await call({ thinking: "HIGH" });
  assert.deepEqual([only.launched.model, only.launched.thinking], ["anthropic/claude-opus-5-5", "high"]);
  const bad = await call({ thinking: "huge" });
  assert.equal(bad.launched, undefined);
  assert.match(bad.text, /Unknown thinking level "huge"/);
});

test("an override never rewrites the agent definition", async () => {
  await call({ model: "openai/gpt-6", thinking: "off" });
  const { launched } = await call({});
  assert.deepEqual([launched.model, launched.thinking], [undefined, "xhigh"]);
});

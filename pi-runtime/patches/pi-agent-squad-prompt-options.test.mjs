// Integration test for the prompt-options patch: the real pi-agent-squad and
// pi-claude-bridge, loaded through Pi's own extension loader, in both load
// orders. It checks what the bridge would append to Claude Code's prompt — the
// place the squad's section used to vanish. No network, provider or session.
//
// Run against a staged runtime whose squad is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
/** A package's single declared extension entry, as Pi resolves it. */
function extensionEntry(name) {
  const [entry, ...rest] = JSON.parse(readFileSync(join(modules, name, "package.json"), "utf-8")).pi.extensions;
  assert.deepEqual(rest, [], `${name} declares more than one extension`);
  return join(modules, name, entry);
}
const SQUAD = extensionEntry("pi-agent-squad");
const BRIDGE = extensionEntry("pi-claude-bridge");

// Keep Pi and every extension off the real agent directory.
const scratch = mkdtempSync(join(tmpdir(), "pi-agent-squad-prompt-options-"));
process.env.PI_CODING_AGENT_DIR = join(scratch, "agent");
after(() => rmSync(scratch, { recursive: true, force: true }));

// The loader is not part of Pi's package exports; its own extension host uses it.
const pi = await import(
  pathToFileURL(join(modules, "@earendil-works/pi-coding-agent", "dist", "core", "extensions", "index.js")).href
);
const piRequire = createRequire(join(modules, "@earendil-works/pi-coding-agent", "package.json"));
const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
const { projectPromptCapture } = await createJiti(import.meta.url).import(join(modules, "pi-claude-bridge", "src", "prompt-capture.ts"));

const ORCHESTRATOR = "# You are the Orchestrator";
const GUARD = "# Subagent Usage Policy: Automatic Delegation Disabled";
const CAPTURES = Symbol.for("claude-bridge:promptCaptures");

/** One agent start with `/orchestrate` in `mode`: the prompt Pi renders, and
 * the part of it the bridge forwards to Claude Code. */
async function agentStart({ order, orchestrate }) {
  // Fresh bridge captures and a distinct prompt, so no earlier case can answer.
  delete globalThis[CAPTURES];
  const cwd = mkdtempSync(join(scratch, "cwd-"));
  const runtime = pi.createExtensionRuntime();
  const loaded = await pi.loadExtensions(order === "squad-first" ? [SQUAD, BRIDGE] : [BRIDGE, SQUAD], cwd, undefined, runtime);
  assert.deepEqual(loaded.errors, []);

  const entries = orchestrate === undefined
    ? []
    : [{ type: "custom", customType: "orchestrator-mode", data: { enabled: orchestrate } }];
  const runner = new pi.ExtensionRunner(loaded.extensions, runtime, cwd, { getEntries: () => entries }, undefined);
  const errors = [];
  runner.onError((error) => errors.push(error));

  const result = await runner.emitBeforeAgentStart("hello", undefined, {
    cwd,
    selectedTools: ["read"],
    contextFiles: [{ path: join(cwd, "AGENTS.md"), content: "project rules" }],
  });
  // What AgentSession hands the provider, and what ctx.getSystemPrompt() reads.
  const options = result.systemPromptOptions;
  let rendered;
  const probe = await pi.loadExtensionFromFactory((api) => {
    api.on("before_agent_start", (event) => {
      rendered = event.systemPrompt;
    });
  }, cwd, undefined, runtime);
  await new pi.ExtensionRunner([probe], runtime, cwd, { getEntries: () => entries }, undefined)
    .emitBeforeAgentStart("hello", undefined, options);
  runner.getSystemPromptFn = () => rendered;
  await runner.emit({ type: "agent_start" });
  assert.deepEqual(errors, []);

  const capture = globalThis[CAPTURES].resolveOrDerive(rendered);
  const forwarded = projectPromptCapture(capture, { skillReadTool: "mcp" }) ?? "";
  return { rendered, forwarded, options };
}

const count = (text, needle) => text.split(needle).length - 1;

for (const order of ["squad-first", "bridge-first"]) {
  test(`/orchestrate on reaches Claude Code (${order})`, async () => {
    const { rendered, forwarded, options } = await agentStart({ order, orchestrate: true });
    assert.equal(options.forceSystemPrompt, undefined);
    assert.equal(count(rendered, ORCHESTRATOR), 1);
    assert.equal(count(forwarded, ORCHESTRATOR), 1);
    assert.ok(forwarded.includes("project rules"));
    assert.ok(!forwarded.includes(GUARD));
  });

  test(`/orchestrate off keeps the delegation guard (${order})`, async () => {
    const { rendered, forwarded, options } = await agentStart({ order, orchestrate: false });
    assert.equal(options.forceSystemPrompt, undefined);
    assert.equal(count(rendered, GUARD), 1);
    assert.equal(count(forwarded, GUARD), 1);
    assert.ok(!forwarded.includes(ORCHESTRATOR));
  });
}

test("the default (never toggled) is the delegation guard", async () => {
  const { forwarded } = await agentStart({ order: "squad-first", orchestrate: undefined });
  assert.equal(count(forwarded, GUARD), 1);
  assert.ok(!forwarded.includes(ORCHESTRATOR));
});

test("after an earlier extension forces the prompt, the section still lands in it", async () => {
  const cwd = mkdtempSync(join(scratch, "cwd-"));
  const runtime = pi.createExtensionRuntime();
  const forcer = await pi.loadExtensionFromFactory((api) => {
    api.on("before_agent_start", () => ({ systemPrompt: "forced by an earlier extension" }));
  }, cwd, undefined, runtime);
  const loaded = await pi.loadExtensions([SQUAD], cwd, undefined, runtime);
  const runner = new pi.ExtensionRunner([forcer, ...loaded.extensions], runtime, cwd, { getEntries: () => [] }, undefined);
  const { systemPromptOptions } = await runner.emitBeforeAgentStart("hello", undefined, { cwd });
  assert.ok(systemPromptOptions.forceSystemPrompt.startsWith("forced by an earlier extension\n\n"));
  assert.equal(count(systemPromptOptions.forceSystemPrompt, GUARD), 1);
});

test("an --append-system-prompt orchestrator is not appended twice", async () => {
  delete globalThis[CAPTURES];
  const cwd = mkdtempSync(join(scratch, "cwd-"));
  const runtime = pi.createExtensionRuntime();
  const loaded = await pi.loadExtensions([SQUAD], cwd, undefined, runtime);
  const runner = new pi.ExtensionRunner(loaded.extensions, runtime, cwd, { getEntries: () => [] }, undefined);
  const { systemPromptOptions } = await runner.emitBeforeAgentStart("hello", undefined, {
    cwd,
    appendSystemPrompt: `${ORCHESTRATOR}\n\nfrom the command line`,
  });
  assert.equal(systemPromptOptions.appendSystemPrompt, `${ORCHESTRATOR}\n\nfrom the command line`);
});

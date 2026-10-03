import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import extension, {
  gptVersion,
  isOpenAiModel,
  sessionState,
  shouldApplyFastMode,
  STATE_ENTRY_TYPE,
  supportsFastMode,
  versionAtLeast,
} from "./cypher-fast-mode.ts";

test("GPT 5.4 and every later version qualify, whatever the provider", () => {
  for (const [provider, id] of [
    ["openai-codex", "gpt-6.1-sol"],
    ["openai", "gpt-6-sol"],
    ["openai", "gpt-6-luna"],
    ["mvp-lab", "gpt-6-astra"],
    ["mvp-lab", "gpt-5.6-sol"],
    ["some-proxy", "gpt-5.4"],
    ["some-proxy", "gpt-5.4-mini"],
    ["openrouter", "openai/gpt-7"],
    ["azure-openai-responses", "gpt-5.10"],
  ]) {
    assert.ok(supportsFastMode({ provider, id }), `${provider}/${id}`);
  }
});

test("older GPT versions and ids without a GPT version do not qualify", () => {
  for (const [provider, id] of [
    ["openai", "gpt-5.3-codex-spark"],
    ["openai", "gpt-5"],
    ["openai", "gpt-4o"],
    ["openai", "o3"],
    ["openrouter", "openai/gpt-oss-120b"],
    ["azure-openai-responses", "prod-deployment-2"],
    ["claude-bridge", "claude-opus-5-5"],
  ]) {
    assert.ok(!supportsFastMode({ provider, id }), `${provider}/${id}`);
  }
  assert.ok(!supportsFastMode(undefined));
  assert.ok(!supportsFastMode({ provider: "openai" }));
});

test("only openai or gpt marks an OpenAI model", () => {
  assert.ok(isOpenAiModel({ provider: "OpenAI", id: "x" }));
  assert.ok(isOpenAiModel({ provider: "gateway", id: "GPT-6" }));
  assert.ok(!isOpenAiModel({ provider: "gateway", id: "llama-6" }));
  assert.ok(!supportsFastMode({ provider: "gateway", id: "llama-6.1" }));
});

test("versions compare part by part", () => {
  assert.deepEqual(gptVersion("gpt-6.1-sol"), [6, 1]);
  assert.deepEqual(gptVersion("openai/GPT6"), [6]);
  assert.equal(gptVersion("gpt-oss-120b"), undefined);
  assert.ok(versionAtLeast([5, 4], [5, 4]));
  assert.ok(versionAtLeast([5, 10], [5, 4]));
  assert.ok(versionAtLeast([6], [5, 4]));
  assert.ok(!versionAtLeast([5], [5, 4]));
  assert.ok(!versionAtLeast([5, 3, 9], [5, 4]));
});

test("the request's model decides, so a routed request is judged by its target", () => {
  const selected = { provider: "openai-codex", id: "gpt-6.1-sol" };
  assert.ok(shouldApplyFastMode(selected, { model: "gpt-6.1-sol" }));
  assert.ok(shouldApplyFastMode({ provider: "auto", id: "auto" }, { model: "gpt-6-sol" }));
  assert.ok(!shouldApplyFastMode(selected, { model: "claude-haiku-4-5" }));
  assert.ok(!shouldApplyFastMode(selected, { messages: [] }));
  assert.ok(!shouldApplyFastMode(selected, undefined));
});

test("the last toggle on the branch wins", () => {
  const entry = (enabled, version = 1) => ({ type: "custom", customType: STATE_ENTRY_TYPE, data: { version, enabled } });
  assert.equal(sessionState([]), undefined);
  assert.equal(sessionState([entry(true), { type: "message" }]), true);
  assert.equal(sessionState([entry(true), entry(false)]), false);
  assert.equal(sessionState([entry(true), entry(false, 2)]), true);
  assert.equal(sessionState([{ type: "custom", customType: "other", data: { enabled: false } }]), undefined);
});

// Points PI_CODING_AGENT_DIR at a fresh agent dir, as Pi does for its whole
// process: the extension reads the default again at every session start.
function load(settings) {
  const agentDir = mkdtempSync(join(tmpdir(), "cypher-fast-mode-"));
  if (settings) writeFileSync(join(agentDir, "settings.json"), JSON.stringify(settings));
  process.env.PI_CODING_AGENT_DIR = agentDir;
  const commands = new Map();
  const handlers = new Map();
  const entries = [];
  extension({
    registerCommand: (name, command) => commands.set(name, command),
    on: (event, handler) => handlers.set(event, handler),
    appendEntry: (customType, data) => entries.push({ type: "custom", customType, data }),
  });
  return { commands, handlers, entries };
}

function context(model, entries = []) {
  const notes = [];
  return {
    notes,
    model,
    ui: { notify: (message, level) => notes.push([message, level]) },
    sessionManager: { getBranch: () => entries },
  };
}

test("/fast toggles the tier and records the toggle", async () => {
  const { commands, handlers, entries } = load();
  const ctx = context({ provider: "openai-codex", id: "gpt-6.1-sol" });
  const request = { payload: { model: "gpt-6.1-sol", input: [] } };
  assert.equal(handlers.get("before_provider_request")(request, ctx), undefined);

  await commands.get("fast").handler("", ctx);
  assert.deepEqual(entries.at(-1).data, { version: 1, enabled: true });
  assert.deepEqual(ctx.notes.at(-1), ["GPT Fast mode enabled (service_tier: priority).", "info"]);
  assert.deepEqual(handlers.get("before_provider_request")(request, ctx), {
    model: "gpt-6.1-sol", input: [], service_tier: "priority",
  });
  assert.equal(handlers.get("before_provider_request")({ payload: { model: "o3" } }, ctx), undefined);

  await commands.get("fast").handler("", ctx);
  assert.deepEqual(ctx.notes.at(-1), ["GPT Fast mode disabled.", "info"]);
  assert.equal(handlers.get("before_provider_request")(request, ctx), undefined);
});

test("/fast warns when the selected model cannot use it", async () => {
  const { commands } = load();
  const ctx = context({ provider: "claude-bridge", id: "claude-opus-5-5" });
  await commands.get("fast").handler("", ctx);
  assert.deepEqual(ctx.notes.at(-1), [
    "GPT Fast mode enabled, but claude-bridge/claude-opus-5-5 is not a supported GPT model.", "warning",
  ]);
});

test("a session restores its own toggle, else the settings default", () => {
  const request = { payload: { model: "gpt-6-sol" } };
  const on = load({ "pi-gpt-fast-mode": { enabled: true } });
  const fresh = context({ provider: "openai", id: "gpt-6-sol" });
  on.handlers.get("session_start")({}, fresh);
  assert.ok(on.handlers.get("before_provider_request")(request, fresh));

  const toggledOff = context(fresh.model, [{ type: "custom", customType: STATE_ENTRY_TYPE, data: { version: 1, enabled: false } }]);
  on.handlers.get("session_start")({}, toggledOff);
  assert.equal(on.handlers.get("before_provider_request")(request, toggledOff), undefined);

  const off = load({});
  off.handlers.get("session_start")({}, fresh);
  assert.equal(off.handlers.get("before_provider_request")(request, fresh), undefined);
});

import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import extension, {
  configuredIndirectServers,
  hasToolAllowlist,
  nextActiveTools,
  sessionState,
  STATE_ENTRY_TYPE,
} from "./cypher-codemode.ts";

const BUILTIN = ["read", "bash", "edit", "write"];
const tools = (...names) => names.map((name) => ({ name, exposure: "direct" }));
const REGISTERED = tools(...BUILTIN, "codemode", "tool_search");
const MCP_TOOL = { name: "mcp__docs__search", exposure: "codemode" };

test("on adds codemode, and nothing else", () => {
  assert.deepEqual(nextActiveTools(true, BUILTIN, REGISTERED, false, false), [[...BUILTIN, "codemode"], false]);
  assert.deepEqual(nextActiveTools(true, [...BUILTIN, "codemode"], REGISTERED, true, false), [undefined, false]);
});

test("off removes codemode; MCP tools then reach the model through tool_search", () => {
  const on = [...BUILTIN, "codemode"];
  assert.deepEqual(nextActiveTools(false, on, REGISTERED, false, false), [BUILTIN, false]);
  // Configured but not yet connected: the file says so.
  assert.deepEqual(nextActiveTools(false, on, REGISTERED, true, false), [[...BUILTIN, "tool_search"], true]);
  // Connected, from elsewhere than the agent's mcp.json.
  assert.deepEqual(nextActiveTools(false, on, [...REGISTERED, MCP_TOOL], false, false), [[...BUILTIN, "tool_search"], true]);
  // A direct MCP tool is declared already: nothing to search for.
  const direct = { name: "mcp__docs__search", exposure: "direct" };
  assert.deepEqual(nextActiveTools(false, on, [...REGISTERED, direct], false, false), [BUILTIN, false]);
});

test("tool_search goes again only when this extension turned it on", () => {
  const searched = [...BUILTIN, "tool_search"];
  assert.deepEqual(nextActiveTools(true, searched, REGISTERED, true, true), [[...BUILTIN, "codemode"], false]);
  assert.deepEqual(nextActiveTools(true, searched, REGISTERED, true, false), [[...searched, "codemode"], false]);
  // Already off and already searching: nothing to change, still ours.
  assert.deepEqual(nextActiveTools(false, searched, REGISTERED, true, true), [undefined, true]);
});

test("a tool Pi did not register is never activated", () => {
  const without = tools(...BUILTIN);
  assert.deepEqual(nextActiveTools(true, BUILTIN, without, true, false), [undefined, false]);
  assert.deepEqual(nextActiveTools(false, BUILTIN, without, true, false), [undefined, false]);
});

test("mcp.json servers count unless disabled or entirely direct", () => {
  assert.ok(configuredIndirectServers({ mcpServers: { docs: { url: "https://x/mcp" } } }));
  assert.ok(configuredIndirectServers({ mcpServers: { docs: { url: "u", exposure: "deferred" } } }));
  assert.ok(configuredIndirectServers({ mcpServers: { docs: { url: "u", exposure: "direct", toolExposure: { "get_*": "codemode" } } } }));
  assert.ok(!configuredIndirectServers({ mcpServers: { docs: { url: "u", exposure: "direct" } } }));
  assert.ok(!configuredIndirectServers({ mcpServers: { docs: { url: "u", enabled: false } } }));
  assert.ok(!configuredIndirectServers({ mcpServers: { docs: { url: "u", exposure: "hidden" } } }));
  assert.ok(!configuredIndirectServers({ mcpServers: {} }));
  assert.ok(!configuredIndirectServers(undefined));
});

test("an explicit tool list is recognized in each spelling", () => {
  for (const argv of [["--tools", "read"], ["-t", "read"], ["--tools=read"], ["--no-tools"], ["-nt"]]) {
    assert.ok(hasToolAllowlist(["node", "pi", "--mode", "rpc", ...argv]), argv.join(" "));
  }
  assert.ok(!hasToolAllowlist(["node", "pi", "--mode", "rpc", "--session-dir", "/tmp/tools"]));
});

test("the last toggle on the branch wins", () => {
  const entry = (enabled, version = 1) => ({ type: "custom", customType: STATE_ENTRY_TYPE, data: { version, enabled } });
  assert.equal(sessionState([]), undefined);
  assert.equal(sessionState([entry(false), { type: "message" }]), false);
  assert.equal(sessionState([entry(false), entry(true)]), true);
  assert.equal(sessionState([entry(false), entry(true, 2)]), false);
});

/** A minimal Pi: the active list, the registered tools, recorded entries. */
function load({ argv = [], mcp, registered = REGISTERED } = {}) {
  const agentDir = mkdtempSync(join(tmpdir(), "cypher-codemode-"));
  if (mcp) writeFileSync(join(agentDir, "mcp.json"), JSON.stringify(mcp));
  process.env.PI_CODING_AGENT_DIR = agentDir;
  const savedArgv = process.argv;
  process.argv = ["node", "pi", "--mode", "rpc", ...argv];
  const pi = {
    active: [...BUILTIN],
    commands: new Map(),
    handlers: new Map(),
    entries: [],
    notes: [],
    registerCommand: (name, command) => pi.commands.set(name, command),
    on: (event, handler) => pi.handlers.set(event, handler),
    appendEntry: (customType, data) => pi.entries.push({ type: "custom", customType, data }),
    getActiveTools: () => [...pi.active],
    getAllTools: () => registered,
    setActiveTools: (names) => { pi.active = [...names]; },
  };
  try {
    extension(pi);
  } finally {
    process.argv = savedArgv;
  }
  pi.ctx = (branch = []) => ({
    ui: { notify: (message, level) => pi.notes.push([message, level]) },
    sessionManager: { getBranch: () => branch },
  });
  pi.start = (branch) => pi.handlers.get("session_start")({}, pi.ctx(branch));
  pi.prompt = () => pi.handlers.get("before_agent_start")({}, pi.ctx());
  pi.run = (args = "") => pi.commands.get("scripts").handler(args, pi.ctx());
  return pi;
}

test("a new chat starts with codemode on", () => {
  const pi = load();
  pi.start();
  assert.deepEqual(pi.active, [...BUILTIN, "codemode"]);
});

test("off holds even when Pi turns codemode back on between prompts", async () => {
  const pi = load({ mcp: { mcpServers: { docs: { url: "https://x/mcp" } } } });
  pi.start();
  await pi.run();
  assert.deepEqual(pi.entries.at(-1).data, { version: 1, enabled: false });
  assert.deepEqual(pi.notes.at(-1), ["Scripts off. MCP tools are found through tool search instead.", "info"]);
  assert.deepEqual(pi.active, [...BUILTIN, "tool_search"]);
  pi.active.push("codemode"); // Pi's MCP extension, after a reconnect.
  pi.prompt();
  assert.deepEqual(pi.active, [...BUILTIN, "tool_search"]);
  await pi.run("on");
  assert.deepEqual(pi.notes.at(-1), ["Scripts on.", "info"]);
  assert.deepEqual(pi.active, [...BUILTIN, "codemode"]);
  await pi.run("on");
  assert.deepEqual(pi.entries.at(-1).data, { version: 1, enabled: true }, "on stays on");
});

test("a resumed chat keeps its last toggle", () => {
  const pi = load();
  pi.start([{ type: "custom", customType: STATE_ENTRY_TYPE, data: { version: 1, enabled: false } }]);
  assert.deepEqual(pi.active, BUILTIN);
});

test("a run with its own tool list is left alone until the chat toggles", async () => {
  const pi = load({ argv: ["--tools", "read,bash"] });
  pi.active = ["read", "bash"];
  pi.start();
  pi.prompt();
  assert.deepEqual(pi.active, ["read", "bash"]);
  await pi.run();
  assert.deepEqual(pi.active, ["read", "bash", "codemode"], "from off, the toggle turns it on");
});

test("without a codemode tool, /scripts only says so", async () => {
  const pi = load({ registered: tools(...BUILTIN) });
  pi.start();
  await pi.run("on");
  assert.equal(pi.entries.length, 0);
  assert.deepEqual(pi.notes.at(-1), ["Scripts are not available in this chat.", "warning"]);
});

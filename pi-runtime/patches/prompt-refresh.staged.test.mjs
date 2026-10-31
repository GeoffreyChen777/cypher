// Tests for the prompt-refresh patch: a pi session whose prompt changed since
// its Claude Code session was created is rebuilt rather than resumed (Claude
// Code keeps a resumed session's stored prompt), and the rebuilt file no longer
// carries that prompt. Real bridge, real session files under a scratch Claude
// config dir. No network, provider or Claude Code.
//
// Run against a staged runtime whose bridge is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const pi = join(modules, "@earendil-works", "pi-coding-agent");
const bridgePackage = JSON.parse(readFileSync(join(modules, "pi-claude-bridge", "package.json"), "utf-8"));
const BRIDGE = join(modules, "pi-claude-bridge", bridgePackage.pi.extensions[0]);

const scratch = realpathSync(mkdtempSync(join(tmpdir(), "pi-prompt-refresh-")));
after(() => rmSync(scratch, { recursive: true, force: true }));

// One bridge process walks a pi session through its turns and prints what the
// bridge decided at each step.
const CHILD = `
import { appendFileSync } from "node:fs";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const [cwd] = process.argv.slice(1);
const piRequire = createRequire(${JSON.stringify(join(pi, "package.json"))});
const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
const bridge = await createJiti(import.meta.url).import(${JSON.stringify(BRIDGE)});
const { getSessionPath } = await import(${JSON.stringify(pathToFileURL(join(modules, "cc-session-io", "dist", "index.js")).href)});
const t = bridge.__test;
const now = Date.now();
const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const messages = [
  { role: "user", content: "first", timestamp: now },
  { role: "assistant", content: [{ type: "text", text: "answer" }], api: "claude-bridge", provider: "claude-bridge",
    model: "claude-sonnet-5", usage, stopReason: "stop", timestamp: now },
  { role: "user", content: "second", timestamp: now },
];
const sync = () => t.syncSharedSession(messages, cwd, undefined, "claude-sonnet-5", "pi");
const state = () => t.getSharedSession("pi");
const out = {};

// A first turn after a restart: the bridge writes the session. No query has
// completed on it yet, so there is no prompt to compare.
const { sessionId } = sync();
out.markedBeforeAnyQuery = t.markPromptChange("pi", "A");

// The query completed with append A: what the provider records. Claude Code
// stores that prompt in the file, as an attachment record.
t.setSharedSession("pi", { ...state(), promptAppend: "A" });
const path = getSessionPath(sessionId, cwd, process.env.CLAUDE_CONFIG_DIR);
appendFileSync(path, JSON.stringify({ type: "attachment", uuid: "sp", parentUuid: null,
  attachment: { type: "system_prompt", systemPrompt: ["OLD-PROMPT-MARKER"] } }) + "\\n");

out.markedWhenUnchanged = t.markPromptChange("pi", "A");
out.unchangedResumes = sync().sessionId === sessionId && !state().needsRebuild;

// The prompt changed (say /explain turned on): the next sync rebuilds.
out.markedWhenChanged = t.markPromptChange("pi", "A + explainer");
out.needsRebuild = state().needsRebuild === true;
out.markedTwice = t.markPromptChange("pi", "A + explainer");
const rebuilt = sync();
out.rebuiltSameId = rebuilt.sessionId === sessionId;
out.rebuiltCleared = !state().needsRebuild && state().promptAppend === undefined;
out.oldPromptGone = !(await import("node:fs")).readFileSync(path, "utf-8").includes("OLD-PROMPT-MARKER");
console.log(JSON.stringify(out));
process.exit(0);
`;

test("a changed prompt rebuilds the Claude Code session instead of resuming it", async () => {
  const cwd = join(scratch, "project");
  mkdirSync(cwd, { recursive: true });
  const child = spawn(process.execPath, ["--input-type=module", "-e", CHILD, cwd], {
    env: { ...process.env, PI_CODING_AGENT_DIR: join(scratch, "agent"), CLAUDE_CONFIG_DIR: join(scratch, "claude") },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => (stdout += chunk));
  child.stderr.on("data", (chunk) => (stderr += chunk));
  const code = await new Promise((resolve) => child.on("exit", resolve));
  assert.equal(code, 0, stderr);
  const line = stdout.split("\n").find((l) => l.startsWith("{"));
  assert.ok(line, `no result: ${stdout}${stderr}`);
  assert.deepEqual(JSON.parse(line), {
    markedBeforeAnyQuery: false,
    markedWhenUnchanged: false,
    unchangedResumes: true,
    markedWhenChanged: true,
    needsRebuild: true,
    markedTwice: false,
    rebuiltSameId: true,
    rebuiltCleared: true,
    oldPromptGone: true,
  });
});

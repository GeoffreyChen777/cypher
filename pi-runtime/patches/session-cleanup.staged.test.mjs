// Tests for the session-cleanup patch: the Claude Code session files the
// patched pi-claude-bridge writes are deleted when its process exits, a killed
// process's are swept by the next bridge to load, and nothing else in the
// project directory is touched. Real bridge processes and session files, under
// a scratch Claude config dir. No network, provider or Claude Code.
//
// Run against a staged runtime whose bridge is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
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

// Keep Pi, the bridge and Claude Code's session store off the real directories.
const scratch = realpathSync(mkdtempSync(join(tmpdir(), "pi-session-cleanup-")));
const agentDir = join(scratch, "agent");
const claudeDir = join(scratch, "claude");
const manifests = join(agentDir, "claude-bridge-sessions");
// A held bridge outlives a failed assertion unless something kills it.
const children = new Set();
after(() => {
  for (const child of children) child.kill("SIGKILL");
  rmSync(scratch, { recursive: true, force: true });
});

// A bridge process. `write` mirrors a three-message pi history onto a Claude
// Code session (the REBUILD path of a first Claude turn after a restart) and
// prints its file; `load` only loads the extension through Pi's loader, as
// every pi process does at startup. `hold` keeps the process alive until killed.
const CHILD = `
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const [mode, cwd, hold] = process.argv.slice(1);
const piRequire = createRequire(${JSON.stringify(join(pi, "package.json"))});
if (mode === "load") {
  const loader = await import(pathToFileURL(${JSON.stringify(join(pi, "dist", "core", "extensions", "index.js"))}).href);
  const loaded = await loader.loadExtensions([${JSON.stringify(BRIDGE)}], cwd, undefined, loader.createExtensionRuntime());
  if (loaded.errors.length) throw new Error(JSON.stringify(loaded.errors));
} else {
  const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
  const bridge = await createJiti(import.meta.url).import(${JSON.stringify(BRIDGE)});
  const now = Date.now();
  const messages = [
    { role: "user", content: "first", timestamp: now },
    { role: "assistant", content: [{ type: "text", text: "answer" }], api: "claude-bridge", provider: "claude-bridge",
      model: "claude-sonnet-5", usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: "stop", timestamp: now },
    { role: "user", content: "second", timestamp: now },
  ];
  const { sessionId } = bridge.__test.syncSharedSession(messages, cwd, undefined, "claude-sonnet-5", "pi-session");
  const { getSessionPath } = await import(${JSON.stringify(pathToFileURL(join(modules, "cc-session-io", "dist", "index.js")).href)});
  console.log(JSON.stringify({ sessionId, path: getSessionPath(sessionId, cwd, process.env.CLAUDE_CONFIG_DIR), pid: process.pid }));
}
// pi's RPC mode: SIGTERM shuts down through process.exit.
process.on("SIGTERM", () => process.exit(143));
if (hold) setInterval(() => {}, 1000);
`;

function bridgeProcess(mode, cwd, { hold = false } = {}) {
  const child = spawn(process.execPath, ["--input-type=module", "-e", CHILD, mode, cwd, hold ? "hold" : ""], {
    env: { ...process.env, PI_CODING_AGENT_DIR: agentDir, CLAUDE_CONFIG_DIR: claudeDir },
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.add(child);
  child.on("exit", () => children.delete(child));
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => (stdout += chunk));
  child.stderr.on("data", (chunk) => (stderr += chunk));
  const exited = new Promise((resolve) => child.on("exit", (code, signal) => resolve({ code, signal, stderr })));
  const written = new Promise((resolve, reject) => {
    child.stdout.on("data", () => {
      const line = stdout.split("\n").find((l) => l.startsWith("{"));
      if (line) resolve(JSON.parse(line));
    });
    exited.then(({ code, signal }) => reject(new Error(`bridge exited (${code ?? signal}) before writing: ${stderr}`)));
  });
  written.catch(() => {}); // `load` never writes; only awaiting `written` may fail
  return { child, exited, written };
}

function project(name) {
  const cwd = join(scratch, name);
  mkdirSync(cwd, { recursive: true });
  return cwd;
}

const companion = (path) => path.slice(0, -".jsonl".length);

test("the sessions a bridge wrote are deleted when its process exits", async () => {
  const run = bridgeProcess("write", project("exit"), { hold: true });
  const { path, pid } = await run.written;
  mkdirSync(join(companion(path), "tool-results"), { recursive: true });
  assert.ok(existsSync(path), "the bridge wrote the session");
  assert.deepEqual(JSON.parse(readFileSync(join(manifests, `${pid}.json`), "utf-8")).sessions, [path]);

  // Cypher stops a parked pi with SIGTERM, which pi turns into process.exit.
  run.child.kill("SIGTERM");
  const { code, stderr } = await run.exited;
  assert.equal(code, 143, stderr);
  assert.ok(!existsSync(path), "exit deleted the session file");
  assert.ok(!existsSync(companion(path)), "exit deleted the companion directory");
  assert.ok(!existsSync(join(manifests, `${pid}.json`)), "exit removed the manifest");
});

test("a killed bridge's sessions are swept by the next one, nothing else", async () => {
  const cwd = project("killed");
  const run = bridgeProcess("write", cwd, { hold: true });
  const { path, pid } = await run.written;
  run.child.kill("SIGKILL");
  await run.exited;
  assert.ok(existsSync(path), "SIGKILL left the session behind");

  // The user's own Claude Code session beside it, and a live process's manifest.
  const projectDir = join(path, "..");
  const own = join(projectDir, "00000000-0000-4000-8000-000000000000.jsonl");
  writeFileSync(own, "{}\n");
  const liveSession = join(projectDir, "11111111-1111-4111-8111-111111111111.jsonl");
  writeFileSync(liveSession, "{}\n");
  writeFileSync(join(manifests, `${process.pid}.json`), JSON.stringify({ sessions: [liveSession] }));

  const next = bridgeProcess("load", project("next"));
  const { code, stderr } = await next.exited;
  assert.equal(code, 0, stderr);
  assert.ok(!existsSync(path), "the next bridge swept the killed one's session");
  assert.ok(!existsSync(join(manifests, `${pid}.json`)), "and its manifest");
  assert.ok(existsSync(own), "a session the bridge never recorded stays");
  assert.ok(existsSync(liveSession), "a live process's session stays");
  assert.ok(existsSync(join(manifests, `${process.pid}.json`)), "and so does its manifest");
  rmSync(join(manifests, `${process.pid}.json`));
});

test("a manifest cannot point the sweep outside session files", async () => {
  const outside = join(scratch, "precious.txt");
  writeFileSync(outside, "keep");
  mkdirSync(manifests, { recursive: true });
  // A pid that is certainly gone: a finished child's.
  const gone = spawn(process.execPath, ["-e", ""]);
  await new Promise((resolve) => gone.on("exit", resolve));
  writeFileSync(join(manifests, `${gone.pid}.json`), JSON.stringify({ sessions: [outside, scratch] }));

  const next = bridgeProcess("load", project("guard"));
  const { code, stderr } = await next.exited;
  assert.equal(code, 0, stderr);
  assert.equal(readFileSync(outside, "utf-8"), "keep");
  assert.ok(!readdirSync(manifests).includes(`${gone.pid}.json`), "the stale manifest is still removed");
});

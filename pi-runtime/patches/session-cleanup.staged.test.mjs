// Tests for the session-cleanup patch: the Claude Code session files the
// patched pi-claude-bridge writes are deleted when its process exits, a killed
// process's are swept by the next bridge to load, nothing else in the project
// directory is touched, a Claude Code child that still writes after its bridge
// exits cannot leave a recreated file behind, and a transcript that no longer
// holds its recording is replaced by pi's history instead of wedging the chat.
// Real bridge processes and session files, under a scratch Claude config dir.
// No network or provider; a stand-in plays the Claude Code child.
//
// Run against a staged runtime whose bridge is already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const pi = join(modules, "@earendil-works", "pi-coding-agent");
const bridgeDir = join(modules, "pi-claude-bridge");
const bridgePackage = JSON.parse(readFileSync(join(bridgeDir, "package.json"), "utf-8"));
const BRIDGE = join(bridgeDir, bridgePackage.pi.extensions[0]);

// Keep Pi, the bridge and Claude Code's session store off the real directories.
const scratch = realpathSync(mkdtempSync(join(tmpdir(), "pi-session-cleanup-")));
const agentDir = join(scratch, "agent");
const claudeDir = join(scratch, "claude");
const manifests = join(agentDir, "claude-bridge-sessions");
// A held bridge or a stand-in outlives a failed assertion unless something kills it.
const children = new Set();
const standIns = new Set();
after(() => {
  for (const child of children) child.kill("SIGKILL");
  for (const pid of standIns) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {}
  }
  rmSync(scratch, { recursive: true, force: true });
});

// Plays the Claude Code child of a query the bridge is stopping. Claude Code
// appends its interrupt records (and last-prompt / cost metadata) as it winds
// down, chained to records of the file it was writing; it creates the file if
// it is gone. `eof`: after its stdin ends and `delay` ms; `signal`: on SIGUSR2
// only (a child that outlives its bridge). It touches `marker` once written.
const STAND_IN = `
import { appendFileSync, writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { basename } from "node:path";
const [path, marker, trigger, delay] = process.argv.slice(1);
const late = () => {
  const sessionId = basename(path, ".jsonl");
  appendFileSync(path, [
    { parentUuid: randomUUID(), isSidechain: false, type: "user", uuid: randomUUID(), sessionId,
      message: { role: "user", content: [{ type: "text", text: "[Request interrupted by user]" }] } },
    { type: "last-prompt", lastPrompt: "second", sessionId },
  ].map((record) => JSON.stringify(record) + "\\n").join(""));
  writeFileSync(marker, "written");
  process.exit(0);
};
process.stdin.resume();
if (trigger === "eof") process.stdin.on("end", () => setTimeout(late, Number(delay)));
else {
  process.on("SIGUSR2", late);
  setInterval(() => {}, 1000);
}
`;

// A bridge process. `write` mirrors a three-message pi history onto a Claude
// Code session (the REBUILD path of a first Claude turn after a restart),
// commits it as the turn's query would, and prints its file; `load` only loads
// the extension through Pi's loader, as every pi process does at startup;
// `heal` runs the stale-transcript scenarios below. `hold` keeps the process
// alive until killed. Options (JSON):
// - writer: {trigger, delay}: start a Claude Code stand-in through the
//   bridge's own QueryProcess (the SDK's spawn hook), as a query does.
// - stopping: the query is being stopped: its child is registered as the
//   session's stopping writer, as the bridge does when a run is aborted, and
//   SIGTERM first closes its stdin (the SDK's close()).
// - shutdown: SIGTERM runs the bridge's session_shutdown handler before
//   process.exit, as pi's RPC shutdown does (extension loaded through Pi's
//   loader). Without it SIGTERM exits at once.
const CHILD = `
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
const [mode, cwd, hold, optionsJson] = process.argv.slice(1);
const options = optionsJson ? JSON.parse(optionsJson) : {};
const piRequire = createRequire(${JSON.stringify(join(pi, "package.json"))});
const loadThroughPi = async () => {
  const loader = await import(pathToFileURL(${JSON.stringify(join(pi, "dist", "core", "extensions", "index.js"))}).href);
  const loaded = await loader.loadExtensions([${JSON.stringify(BRIDGE)}], cwd, undefined, loader.createExtensionRuntime());
  if (loaded.errors.length) throw new Error(JSON.stringify(loaded.errors));
  return loaded.extensions[0];
};
const now = Date.now();
const messages = [
  { role: "user", content: "first", timestamp: now },
  { role: "assistant", content: [{ type: "text", text: "answer" }], api: "claude-bridge", provider: "claude-bridge",
    model: "claude-sonnet-5", usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: "stop", timestamp: now },
  { role: "user", content: "second", timestamp: now },
];
const MODEL = "claude-sonnet-5";
const PROMPT = "prompt-digest";
const { getSessionPath } = await import(${JSON.stringify(pathToFileURL(join(modules, "cc-session-io", "dist", "index.js")).href)});
const sessionPath = (sessionId) => getSessionPath(sessionId, cwd, process.env.CLAUDE_CONFIG_DIR);
if (mode === "load") {
  await loadThroughPi();
} else {
  const extension = options.shutdown ? await loadThroughPi() : undefined;
  const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
  const jiti = createJiti(import.meta.url);
  const bridge = await jiti.import(${JSON.stringify(BRIDGE)});
  if (mode === "heal") {
    const store = await jiti.import(${JSON.stringify(join(bridgeDir, "src", "session-store.ts"))});
    const results = {};
    for (const variant of options.variants) {
      const piSession = "pi-" + variant;
      // A completed turn's mirror, saved as its pointer: what a restarted pi reads.
      const first = bridge.__test.syncSharedSession(messages, cwd, undefined, MODEL, piSession, undefined, PROMPT);
      const state = { ccSessionId: first.sessionId, cwd, model: MODEL, promptDigest: PROMPT, ...first.recorded, dirtyTail: false };
      bridge.__test.setSharedSession(piSession, state);
      store.saveSessionState(piSession, state);
      const path = sessionPath(first.sessionId);
      const lines = readFileSync(path, "utf-8").split("\\n").filter(Boolean);
      const anchors = state.uuids.filter(Boolean);
      const late = (parentUuid) => [
        { parentUuid, isSidechain: false, type: "user", uuid: "00000000-0000-4000-8000-00000000000" + anchors.length, sessionId: first.sessionId,
          message: { role: "user", content: [{ type: "text", text: "[Request interrupted by user]" }] } },
        { type: "last-prompt", lastPrompt: "second", sessionId: first.sessionId },
      ].map((record) => JSON.stringify(record) + "\\n").join("");
      // Each damages the file differently; none is told apart by its shape.
      if (variant === "stub") writeFileSync(path, late(anchors.at(-1)));
      else if (variant === "truncated") writeFileSync(path, lines[0] + "\\n");
      else if (variant === "edited") writeFileSync(path, lines.filter((line) => !line.includes(anchors.at(-1))).join("\\n") + "\\n");
      else if (variant === "unparsable") writeFileSync(path, lines.map((line, i) => i === 1 ? line.slice(0, 25) : line).join("\\n") + "\\n");
      else if (variant === "appended") appendFileSync(path, late(anchors.at(-1)));
      // A crash cut the final append short, and pi's history changed there: the cut keeps only what landed.
      else if (variant === "torn") {
        const last = lines.findIndex((line) => line.includes('"uuid":"' + anchors.at(-1) + '"'));
        writeFileSync(path, lines.slice(0, last).join("\\n") + "\\n" + lines[last].slice(0, 40));
      }
      const nextMessages = variant === "torn"
        ? [messages[0], { ...messages[1], content: [{ type: "text", text: "answer, edited" }] }, messages[2]] : messages;
      bridge.__test.forgetSessionMemory();
      try {
        const next = bridge.__test.syncSharedSession(nextMessages, cwd, undefined, MODEL, piSession, undefined, PROMPT);
        results[variant] = {
          before: first.sessionId, anchors: anchors.length, sessionId: next.sessionId, forkAt: next.forkAt ?? null,
          state: bridge.__test.getSharedSession(piSession)?.ccSessionId ?? null,
          pointer: store.loadSessionState(piSession, () => {})?.ccSessionId ?? null,
          imported: next.sessionId && next.sessionId !== first.sessionId ? readFileSync(sessionPath(next.sessionId), "utf-8") : null,
        };
      } catch (error) {
        results[variant] = { error: String(error?.message ?? error) };
      }
    }
    console.log(JSON.stringify(results));
  } else {
    const { sessionId } = bridge.__test.syncSharedSession(messages, cwd, undefined, MODEL, "pi-session");
    // The turn's query completed: the bridge commits the mirror to this session.
    bridge.__test.setSharedSession("pi-session", { ccSessionId: sessionId, cwd, model: MODEL, promptDigest: "",
      digests: [], uuids: [], cuttable: [], dirtyTail: false });
    const path = sessionPath(sessionId);
    let writer;
    if (options.writer) {
      const { QueryProcess } = await jiti.import(${JSON.stringify(join(bridgeDir, "src", "query-process.ts"))});
      const query = new QueryProcess();
      writer = query.spawn({ command: process.execPath, cwd, env: process.env, signal: undefined,
        args: ["--input-type=module", "-e", ${JSON.stringify(STAND_IN)}, path, options.writer.marker, options.writer.trigger, String(options.writer.delay ?? 0)] });
      if (options.stopping) globalThis[Symbol.for("claude-bridge:sessionRegistry")].writers.set(sessionId, query.exited);
    }
    process.on("SIGTERM", async () => {
      if (options.stopping) writer.stdin.end();
      if (extension) {
        for (const handler of extension.handlers.get("session_shutdown") ?? []) {
          await handler({ type: "session_shutdown", reason: "quit" }, { sessionManager: { getSessionId: () => "pi-session" } });
        }
      }
      process.exit(143);
    });
    console.log(JSON.stringify({ sessionId, path, pid: process.pid, writerPid: writer?.pid ?? null }));
  }
}
// pi's RPC mode: SIGTERM shuts down through process.exit.
if (!process.listenerCount("SIGTERM")) process.on("SIGTERM", () => process.exit(143));
if (hold) setInterval(() => {}, 1000);
`;

function bridgeProcess(mode, cwd, { hold = false, options = {}, env = {} } = {}) {
  const child = spawn(process.execPath, ["--input-type=module", "-e", CHILD, mode, cwd, hold ? "hold" : "", JSON.stringify(options)], {
    env: { ...process.env, PI_CODING_AGENT_DIR: agentDir, CLAUDE_CONFIG_DIR: claudeDir, ...env },
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
      if (line) {
        const value = JSON.parse(line);
        if (value.writerPid) standIns.add(value.writerPid);
        resolve(value);
      }
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

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

async function until(condition, what, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail(`timed out waiting until ${what}`);
    await sleep(25);
  }
}

async function load(name) {
  const { code, stderr } = await bridgeProcess("load", project(name)).exited;
  assert.equal(code, 0, stderr);
}

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

  await load("next");
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

  await load("guard");
  assert.equal(readFileSync(outside, "utf-8"), "keep");
  assert.ok(!readdirSync(manifests).includes(`${gone.pid}.json`), "the stale manifest is still removed");
});

// The 2026-11-25 race: an interrupt stops the query, Cypher SIGTERMs pi, and
// Claude Code appends its interrupt records while pi is already exiting.
test("a stopping Claude Code child's late write lands before the bridge deletes its session", async () => {
  const marker = join(scratch, "late-write.marker");
  const run = bridgeProcess("write", project("late"), {
    hold: true,
    options: { writer: { trigger: "eof", delay: 400, marker }, stopping: true, shutdown: true },
  });
  const { path, pid, writerPid } = await run.written;
  run.child.kill("SIGTERM");
  const { code, stderr } = await run.exited;
  assert.equal(code, 143, stderr);
  await until(() => !alive(writerPid), "the Claude Code stand-in exits");
  assert.ok(existsSync(marker), "the stand-in made its late write");
  assert.ok(!existsSync(path), "no late write recreated the session file");
  assert.ok(!existsSync(join(manifests, `${pid}.json`)), "nothing could still write: the manifest is gone");
});

test("a child that outlives its bridge's shutdown keeps its sessions until it is gone", async () => {
  const marker = join(scratch, "outlived.marker");
  const run = bridgeProcess("write", project("outlived"), {
    hold: true,
    options: { writer: { trigger: "signal", marker }, stopping: true, shutdown: true },
  });
  const { path, pid, writerPid } = await run.written;
  const manifest = join(manifests, `${pid}.json`);
  const stopped = Date.now();
  run.child.kill("SIGTERM");
  const { code, stderr } = await run.exited;
  assert.equal(code, 143, stderr);
  assert.ok(Date.now() - stopped < 3000, "shutdown waits less than the engine's 3 s kill grace");
  assert.ok(!existsSync(path), "exit still deleted the session");
  assert.deepEqual(JSON.parse(readFileSync(manifest, "utf-8")), { sessions: [path], writers: [writerPid] },
    "the manifest stays, naming the child that can still write");

  await load("outlived-early");
  assert.ok(existsSync(manifest), "a bridge loading while the child runs leaves its sessions alone");

  process.kill(writerPid, "SIGUSR2");
  await until(() => !alive(writerPid), "the Claude Code stand-in exits");
  assert.ok(existsSync(path), "its late write recreated the session file");
  await load("outlived-late");
  assert.ok(!existsSync(path), "the next bridge swept the recreated file");
  assert.ok(!existsSync(manifest), "and the manifest");
});

test("a killed bridge's sessions wait for the Claude Code child it left running", async () => {
  const marker = join(scratch, "orphan.marker");
  const run = bridgeProcess("write", project("orphan"), {
    hold: true,
    options: { writer: { trigger: "signal", marker } },
  });
  const { path, pid, writerPid } = await run.written;
  const manifest = join(manifests, `${pid}.json`);
  assert.deepEqual(JSON.parse(readFileSync(manifest, "utf-8")).writers, [writerPid], "the manifest names the running child");
  run.child.kill("SIGKILL");
  await run.exited;

  await load("orphan-early");
  assert.ok(existsSync(path), "not swept while the orphaned child can still write");

  process.kill(writerPid, "SIGUSR2");
  await until(() => !alive(writerPid), "the Claude Code stand-in exits");
  await load("orphan-late");
  assert.ok(!existsSync(path), "swept once the child is gone, late write included");
  assert.ok(!existsSync(manifest), "and the manifest");
});

// Whatever left it so, a transcript lacking its recorded boundaries cannot be
// resumed or cut: the bridge imports pi's history, as for a missing file.
test("a transcript that no longer holds its recording is replaced by pi's history", async () => {
  const variants = ["intact", "appended", "torn", "stub", "truncated", "edited", "unparsable"];
  const run = bridgeProcess("heal", project("heal"), { options: { variants } });
  const results = await run.written;
  const { code, stderr } = await run.exited;
  assert.equal(code, 0, stderr);

  // Controls: the recorded file, with or without records after its recording, still forks.
  for (const variant of ["intact", "appended"]) {
    const result = results[variant];
    assert.equal(result.error, undefined, `${variant}: ${result.error}`);
    assert.ok(result.anchors > 0, `${variant}: the recording has anchors`);
    assert.equal(result.sessionId, result.before, `${variant}: resumes the recorded session`);
    assert.ok(result.forkAt, `${variant}: forks at its recorded boundary`);
    assert.equal(result.state, result.before, `${variant}: keeps its state`);
    assert.equal(result.pointer, result.before, `${variant}: keeps its pointer`);
  }

  // Control: a later anchor torn off by a crash is outside an earlier cut (upstream's own rule).
  const torn = results.torn;
  assert.equal(torn.error, undefined, `torn: ${torn.error}`);
  assert.equal(torn.forkAt, null, "torn: the edit is written after the kept prefix");
  assert.ok(torn.sessionId && torn.sessionId !== torn.before, "torn: into a new session");
  assert.match(torn.imported, /answer, edited/, "torn: with pi's edited message");
  assert.equal(torn.state, torn.before, "torn: the state stays until the query commits");
  assert.equal(torn.pointer, torn.before, "torn: and its pointer");

  const diagLog = join(agentDir, "claude-bridge-diag.log");
  const diag = (existsSync(diagLog) ? readFileSync(diagLog, "utf-8") : "")
    .split("\n").filter(Boolean).map((line) => JSON.parse(line)).filter((entry) => entry.label === "stale_transcript");
  for (const variant of ["stub", "truncated", "edited", "unparsable"]) {
    const result = results[variant];
    assert.equal(result.error, undefined, `${variant}: the sync failed: ${result.error}`);
    assert.ok(result.sessionId && result.sessionId !== result.before, `${variant}: imports into a new session`);
    assert.equal(result.forkAt, null, `${variant}: nothing is cut from the stale file`);
    assert.equal(result.state, null, `${variant}: the stale state is dropped`);
    assert.equal(result.pointer, null, `${variant}: and its pointer`);
    assert.match(result.imported, /"first"/, `${variant}: pi's history was imported`);
    assert.match(result.imported, /"answer"/, `${variant}: pi's history was imported`);
    const logged = diag.filter((entry) => entry.piSessionId === `pi-${variant}`);
    assert.equal(logged.length, 1, `${variant}: the reason is logged once`);
    assert.equal(logged[0].reason, "stale-transcript");
    assert.equal(logged[0].ccSessionId, result.before);
  }
  assert.ok(!diag.some((entry) => ["pi-intact", "pi-appended", "pi-torn"].includes(entry.piSessionId)), "controls log nothing");
});

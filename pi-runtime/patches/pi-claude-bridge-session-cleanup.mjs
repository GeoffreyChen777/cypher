// Build-time patch: delete the Claude Code session files the bridge created,
// only once nothing can write to them any more, and recover from a session
// file that no longer holds what the bridge recorded.
//
// Why this exists
// ---------------
// pi-claude-bridge mirrors each pi session onto a Claude Code session JSONL in
// ~/.claude/projects/<cwd>/. It never deletes one: every edit, rewind or
// prompt change rotates the mirror to a fresh file, and the pointer it keeps
// per pi session (`<agentDir>/claude-bridge/sessions/`) is never removed with
// the chat. Each left a dead JSONL (and its companion directory) beside the
// user's own Claude Code sessions, forever.
//
// The bridge now records every session it writes or Claude Code reports to it
// (one manifest per process, `<agentDir>/claude-bridge-sessions/<pid>.json`,
// holding resolved file paths so a deleted worktree cannot hide them) and
// deletes them when the process exits — pi's SIGTERM shutdown ends in
// `process.exit`. A process that died before it could (SIGKILL, crash) is
// swept by the next bridge to load, once its pid is gone. Only files the
// bridge recorded are ever deleted: its sessions are indistinguishable from
// Claude Code CLI ones on disk (cc-session-io writes `entrypoint: "cli"`).
//
// Late writers (2026-11-25)
// -------------------------
// Deleting at exit raced the Claude Code child. An interrupt ends the run, the
// SDK closes the query (stdin EOF now, SIGTERM 2 s later, and SIGTERM again
// from its own exit handler), and Cypher then stops pi with SIGTERM. pi exited
// and deleted the file while `claude` was still winding down; `claude` then
// appended its interrupt records (and its last-prompt / cost metadata),
// recreating the path as a small stub. The pointer still named that session and
// its boundary uuids, which the stub lacks, so every later turn failed with
// "Session boundary <uuid> is absent from the transcript" and the chat was
// stuck until the stub was deleted by hand.
//
// Two layers, both by mechanism:
//
// 1. A session file is released only once its writers are gone.
//    - Every Claude Code child a query spawns (query-process.ts, the SDK's
//      `spawnClaudeCodeProcess` hook) is tracked until it exits, and the
//      manifest names the live ones (`writers`) beside the sessions — written
//      as they start and exit, so it also holds when pi is SIGKILLed.
//    - pi awaits extensions' `session_shutdown` before `process.exit`, so the
//      bridge waits there (at most SHUTDOWN_WRITER_WAIT_MS, well inside the
//      engine's 3 s SIGTERM→SIGKILL grace) for the children it is already
//      stopping. In the common case they have exited, and the deletion at exit
//      is final.
//    - If a child still runs at exit (the wait ran out, or a parked query of
//      another session that the SDK only SIGTERMs at exit), the exit handler
//      deletes the files as before but keeps the manifest naming that child.
//      The sweep deletes a dead process's sessions only once every writer it
//      names is gone too (or WRITER_GRACE_MS after the manifest was last
//      written, so a reused pid cannot keep files forever), catching any file
//      a late write recreated.
//    Why not end the children ourselves: an exit handler cannot wait (it is
//    synchronous, and a not-yet-reaped child still answers kill(pid, 0)), and
//    SIGKILL still lets an in-flight append land while cutting Claude Code off
//    mid-write of its own state elsewhere. Waiting for the exit we already
//    asked for, and deferring the deletion when it does not come, needs neither.
//
// 2. A transcript that does not hold its recording is treated as missing.
//    Whatever the cause (a stub like the above, a truncated or edited file, a
//    committed line that no longer parses), a pointer whose transcript exists
//    but lacks an anchor the sync relies on cannot be resumed, forked or cut.
//    The sync now checks those anchors (findBoundary's own test: a record's
//    uuid or a steer's queued_command source_uuid) before using the state: all
//    of them for a resume as is, the retained prefix's for a fork or cut, so a
//    later anchor torn off by a crash stays allowed as upstream intends. If one
//    is absent, or the file does not parse, it drops the state and imports
//    pi's history exactly as for a missing file, with reason "stale-transcript"
//    (beside "missing-transcript") in the decision trace, the debug log and
//    `claude-bridge-diag.log`. The parsed records are reused by the fork and
//    prefix paths, which read the same file.
//    Deliberate difference from upstream: its readTranscript throws on a
//    malformed committed line (its unit-session-prefix test "a malformed
//    committed line fails"), which wedges the chat for good; here that line
//    makes the transcript stale instead.
//
// Lifetime
// --------
// Until the bridge cleans up its own sessions. Pinned to the bundled version:
// a version bump must re-check the anchors (session-cleanup.staged.test.mjs
// checks the patched bridge still loads, cleans up, defers to live writers and
// recovers from a stale transcript).
//
// Failure policy: hard. A missing anchor means the bridge changed shape;
// shipping it unpatched would keep leaking session files (or wedging chats).
// The packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-session-cleanup.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-session-cleanup.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: session-cleanup";
const EXPECTED_VERSION = "0.12.0";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-session-cleanup.mjs" });

const INDEX = [
  {
    label: "imports",
    from: `import { appendFileSync, mkdirSync, readFileSync, realpathSync, statSync } from "fs";`,
    to: `import { appendFileSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from "fs";
import { getAgentDir } from "@earendil-works/pi-coding-agent";`,
  },
  {
    label: "query children import",
    from: `import { QueryProcess } from "./query-process.js";`,
    to: `import { QueryProcess, queryChildren } from "./query-process.js";`,
  },
  {
    label: "transcript import",
    from: `import { readTranscript, recordedToolResults, resolveRecordingCut, savePrefixSession } from "./session-transcript.js";`,
    to: `import { absentRecordedAnchor, readTranscript, recordedToolResults, resolveRecordingCut, savePrefixSession } from "./session-transcript.js";`,
  },
  {
    label: "setSessionStateFor",
    from: `function setSessionStateFor(piSessionId: string, state: SessionState | null): void {
	knownSessionIds.add(piSessionId);
	loadedSessionStates.add(piSessionId);
	if (state === null) sharedSessions.delete(piSessionId);
	else sharedSessions.set(piSessionId, state);
}`,
    to: `function setSessionStateFor(piSessionId: string, state: SessionState | null): void {
	knownSessionIds.add(piSessionId);
	loadedSessionStates.add(piSessionId);
	if (state === null) sharedSessions.delete(piSessionId);
	else {
		ownSession(state.ccSessionId, state.cwd);
		sharedSessions.set(piSessionId, state);
	}
}

// ${MARKER}
// Claude Code session files this process wrote or resumed, deleted when this
// process exits. The bridge also persists a pointer per pi session so a later
// process can resume the file; with the file gone it imports pi's history
// instead ("missing-transcript"), as it did before pointers existed. The
// manifest lists them, and the Claude Code children that can still write to
// them, for the next bridge to sweep when this one dies before it can or a
// child outlives it.
// One registry per process (Symbol.for), however many module instances load.
interface OwnedSessions {
	paths: Set<string>; // resolved JSONL paths
	manifest: string;
}
const OWNED_SESSIONS_KEY = Symbol.for("claude-bridge:ownedSessions");
const SESSION_FILE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.jsonl$/i;
/** How long session_shutdown waits for the Claude Code children being stopped
 *  to exit. The engine SIGKILLs pi 3 s after its SIGTERM. */
const SHUTDOWN_WRITER_WAIT_MS = 1500;
/** How long after a dead bridge's manifest was written its writers' pids still
 *  hold off the sweep. Claude Code exits within seconds of losing its stdin
 *  (while pi lives, the SDK's close() SIGKILLs it after 7 s); a pid alive
 *  past this is a reused one or a hung child. */
const WRITER_GRACE_MS = 60_000;

function sessionManifestDir(): string {
	return join(getAgentDir(), "claude-bridge-sessions");
}

function ownedSessions(): OwnedSessions | undefined {
	return (globalThis as Record<symbol, unknown>)[OWNED_SESSIONS_KEY] as OwnedSessions | undefined;
}

/** Atomic: a sweep must never read a half-written manifest. */
function writeSessionManifest(owned: OwnedSessions, writers: readonly number[] = [...queryChildren.live]): void {
	try {
		mkdirSync(dirname(owned.manifest), { recursive: true });
		const tmp = \`\${owned.manifest}.tmp\`;
		writeFileSync(tmp, JSON.stringify({ sessions: [...owned.paths], writers }));
		renameSync(tmp, owned.manifest);
	} catch (error) {
		debug("WARNING: could not record owned Claude Code sessions:", error);
	}
}

// A child starting or exiting changes who can still write; only a manifest
// that names sessions needs to say so.
queryChildren.changed = () => {
	const owned = ownedSessions();
	if (owned?.paths.size) writeSessionManifest(owned);
};

/** Record \`sessionId\` (written under \`cwd\`) for deletion at exit. */
function ownSession(sessionId: string, cwd: string): void {
	let owned = ownedSessions();
	if (!owned) {
		owned = { paths: new Set(), manifest: join(sessionManifestDir(), \`\${process.pid}.json\`) };
		(globalThis as Record<symbol, unknown>)[OWNED_SESSIONS_KEY] = owned;
		// Synchronous on purpose: pi's SIGTERM shutdown ends in process.exit.
		process.on("exit", releaseOwnedSessions);
	}
	let path: string;
	try {
		path = getSessionPath(sessionId, cwd, process.env.CLAUDE_CONFIG_DIR);
	} catch (error) {
		debug(\`WARNING: could not resolve session \${sessionId.slice(0, 8)} for cleanup:\`, error);
		return;
	}
	if (owned.paths.has(path)) return;
	owned.paths.add(path);
	writeSessionManifest(owned);
}

/** Delete a session JSONL and its companion directory. */
function removeSessionFile(path: string): void {
	if (!SESSION_FILE.test(basename(path))) return;
	try {
		rmSync(path, { force: true });
		rmSync(path.slice(0, -".jsonl".length), { recursive: true, force: true });
	} catch (error) {
		debug(\`WARNING: could not delete session file \${path}:\`, error);
	}
}

function releaseOwnedSessions(): void {
	const owned = ownedSessions();
	if (!owned) return;
	for (const path of owned.paths) removeSessionFile(path);
	// A child still running can append after this and recreate a file (Claude
	// Code writes its interrupt records while it stops). Keep the manifest
	// naming it; the sweep deletes the files again once it is gone. A child
	// that exited but was not reaped yet still counts: that only defers.
	const writers = [...queryChildren.live].filter(processAlive);
	if (writers.length && owned.paths.size) writeSessionManifest(owned, writers);
	else rmSync(owned.manifest, { force: true });
	owned.paths.clear();
}

/** Let the Claude Code children this process is stopping (sessionWriters)
 *  exit before pi does, so the deletion at exit is final. Bounded: pi has
 *  only the engine's kill grace, and a child that takes longer is left to
 *  the manifest. A healthy parked query is not stopping and is not awaited. */
async function settleStoppingWriters(): Promise<void> {
	const exits = [...sessionWriters.values()];
	if (!exits.length) return;
	let timer: ReturnType<typeof setTimeout> | undefined;
	const expired = new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(true), SHUTDOWN_WRITER_WAIT_MS); });
	const timedOut = await Promise.race([Promise.all(exits).then(() => false), expired]);
	clearTimeout(timer);
	debug(\`session_shutdown: \${timedOut ? "gave up waiting for" : "waited for"} \${exits.length} stopping Claude Code child(ren)\`);
}

function processAlive(pid: number): boolean {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		return (error as NodeJS.ErrnoException).code === "EPERM";
	}
}

/** Delete the sessions of bridge processes that exited without releasing them,
 *  once no Claude Code child they started can still write to them. */
function sweepAbandonedSessions(): void {
	const dir = sessionManifestDir();
	let names: string[];
	try {
		names = readdirSync(dir);
	} catch {
		return;
	}
	for (const name of names) {
		const pid = Number(name.match(/^(\\d+)\\.json(?:\\.tmp)?$/)?.[1]);
		if (!pid) continue;
		// Our own pid is live only once this process has recorded a session;
		// before that, a manifest under it is a dead process's whose pid we reuse.
		if (pid === process.pid ? ownedSessions() !== undefined : processAlive(pid)) continue;
		const manifest = join(dir, name);
		if (name.endsWith(".json")) {
			try {
				const { sessions, writers } = JSON.parse(readFileSync(manifest, "utf-8")) as { sessions?: unknown; writers?: unknown };
				const writing = Array.isArray(writers) ? writers.filter((w): w is number => Number.isInteger(w) && w > 0 && processAlive(w)) : [];
				if (writing.length && Date.now() - statSync(manifest).mtimeMs < WRITER_GRACE_MS) {
					debug(\`sweep: bridge process \${pid} exited, but Claude Code \${writing.join(", ")} can still write its sessions; leaving them\`);
					continue;
				}
				if (Array.isArray(sessions)) {
					for (const path of sessions) if (typeof path === "string") removeSessionFile(path);
				}
				debug(\`swept \${Array.isArray(sessions) ? sessions.length : 0} session(s) of exited bridge process \${pid}\`);
			} catch (error) {
				debug(\`WARNING: unreadable session manifest \${manifest}:\`, error);
			}
		}
		rmSync(manifest, { force: true });
	}
}

/** The records of the transcript \`state\` names, or why this sync cannot use
 *  them: an anchor in \`relied\` is absent (a stub recreated by a late write, a
 *  truncated or edited file) or a committed line no longer parses. Resuming,
 *  forking or cutting such a file fails on every turn or loses history. */
function recordedTranscriptFor(state: SessionState, relied: readonly (string | null)[]): { records: ReturnType<typeof readTranscript> } | { stale: string } {
	let records: ReturnType<typeof readTranscript>;
	try {
		records = readTranscript(state);
	} catch (error) {
		if (error instanceof SyntaxError) return { stale: \`unparsable (\${error.message})\` };
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return { stale: "deleted while being read" };
		throw error;
	}
	const absent = absentRecordedAnchor(records, relied);
	return absent ? { stale: \`recorded boundary \${absent} is absent (\${records.length} records)\` } : { records };
}`,
  },
  {
    label: "path import",
    from: `import { dirname } from "path";`,
    to: `import { basename, dirname, join } from "path";`,
  },
  {
    label: "stale transcript check",
    from: `	const cwdChanged = Boolean(state && safeRealpath(state.cwd) !== safeRealpath(cwd));`,
    to: `	// ${MARKER}: a transcript that no longer holds its recording is as good as missing.
	let staleFile = false;
	let recordedTranscript: ReturnType<typeof readTranscript> | undefined;
	if (state) {
		// The anchors this sync keeps: all of them to resume the file as it is,
		// the retained prefix's to fork or cut it. A later anchor a crash tore
		// from the end stays allowed, as upstream intends; a model, prompt or
		// cwd change keeps nothing but still reads the file, so it must parse.
		const relied = compared.equal && !state.dirtyTail ? state.uuids : state.uuids.slice(0, compared.cutCount);
		const checked = recordedTranscriptFor(state, relied);
		if ("stale" in checked) {
			staleFile = true;
			debug(\`sync: stale transcript \${state.ccSessionId} in recorded cwd=\${state.cwd}: \${checked.stale}; importing pi history (reason=stale-transcript) \${sessionTag(piSessionId)}\`);
			diagDump("stale_transcript", { piSessionId, ccSessionId: state.ccSessionId, cwd: state.cwd, reason: "stale-transcript", detail: checked.stale });
			setSessionStateFor(piSessionId!, null);
			deleteSessionState(piSessionId!);
			state = null;
		} else recordedTranscript = checked.records;
	}
	const cwdChanged = Boolean(state && safeRealpath(state.cwd) !== safeRealpath(cwd));`,
  },
  {
    label: "stale transcript reason",
    from: `missingFile ? "missing-transcript" : !state ? "no-state"`,
    to: `missingFile ? "missing-transcript" : staleFile ? "stale-transcript" : !state ? "no-state"`,
  },
  {
    label: "cut reads the checked transcript",
    from: `		const transcript = readTranscript(state!);`,
    to: `		const transcript = recordedTranscript ?? readTranscript(state!);`,
  },
  {
    label: "import reads the checked transcript",
    from: `		const transcript = state ? readTranscript(state) : [];`,
    to: `		const transcript = state ? recordedTranscript ?? readTranscript(state) : [];`,
  },
  {
    label: "own Claude Code's own session id",
    from: `		if (message.type === "system" && message.subtype === "init") {
			capturedSessionId = message.session_id;`,
    to: `		if (message.type === "system" && message.subtype === "init") {
			capturedSessionId = message.session_id;
			// ${MARKER}: a clean start's session is Claude Code's; it reaches the
			// mirror only when the query completes, so an aborted first turn would leak it.
			if (capturedSessionId && typeof message.cwd === "string") ownSession(capturedSessionId, message.cwd);`,
  },
  {
    label: "sweep at load",
    from: `export default function (pi: ExtensionAPI) {
	const config = loadConfig();`,
    to: `export default function (pi: ExtensionAPI) {
	sweepAbandonedSessions();

	const config = loadConfig();`,
  },
  {
    label: "shutdown waits for stopping writers",
    from: `	pi.on("session_shutdown", (_event, ctx) => {
		forgetSessionCwd(ctx.sessionManager.getSessionId());
		closeSessionQueries(ctx.sessionManager.getSessionId());
		clearSession("session_shutdown");
	});`,
    to: `	pi.on("session_shutdown", async (_event, ctx) => {
		forgetSessionCwd(ctx.sessionManager.getSessionId());
		closeSessionQueries(ctx.sessionManager.getSessionId());
		clearSession("session_shutdown");
		// ${MARKER}: pi awaits this before process.exit deletes the sessions.
		await settleStoppingWriters();
	});`,
  },
];

const QUERY_PROCESS = [
  {
    label: "query children registry",
    from: `import type { SpawnedProcess, SpawnOptions } from "@anthropic-ai/claude-agent-sdk";`,
    to: `import type { SpawnedProcess, SpawnOptions } from "@anthropic-ai/claude-agent-sdk";

// ${MARKER}
// The Claude Code children of every query this process runs, by pid, until
// each exits: any of them can still append to its session file, so the
// session cleanup (index.ts) defers deleting files while one lives. One
// registry per process (Symbol.for), however many module instances load.
export interface QueryChildren {
	live: Set<number>;
	/** Called after a child starts or exits. */
	changed?: () => void;
}
const QUERY_CHILDREN_KEY = Symbol.for("claude-bridge:queryChildren");
export const queryChildren: QueryChildren = ((globalThis as Record<symbol, unknown>)[QUERY_CHILDREN_KEY] ??= { live: new Set<number>() }) as QueryChildren;

function trackQueryChild(child: ReturnType<typeof spawn>): void {
	const pid = child.pid;
	// No pid: the spawn failed and nothing runs (an "error" event follows).
	if (!pid) return;
	queryChildren.live.add(pid);
	queryChildren.changed?.();
	child.once("exit", () => {
		queryChildren.live.delete(pid);
		queryChildren.changed?.();
	});
}`,
  },
  {
    label: "track each spawned child",
    from: `		child.once("close", () => this.resolveExit());`,
    to: `		child.once("close", () => this.resolveExit());
		trackQueryChild(child);`,
  },
];

const TRANSCRIPT = [
  {
    label: "absent recorded anchor",
    from: `function boundaryIndex(records: readonly RecordEntry[], uuid: string | null): number {`,
    to: `// ${MARKER}
/** The first anchor of \`uuids\` (nulls skipped) that \`records\` lacks, by
 *  findBoundary's test; undefined when every one is present. One pass over
 *  the records, however many anchors. */
export function absentRecordedAnchor(records: readonly RecordEntry[], uuids: readonly (string | null)[]): string | undefined {
	const present = new Set<string>();
	for (const r of records) {
		if (typeof r.uuid === "string") present.add(r.uuid);
		if (r.type === "attachment" && r.attachment?.type === "queued_command" && typeof r.attachment.source_uuid === "string") present.add(r.attachment.source_uuid);
	}
	return uuids.find((uuid): uuid is string => uuid !== null && !present.has(uuid));
}

function boundaryIndex(records: readonly RecordEntry[], uuid: string | null): number {`,
  },
];

const results = [
  patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: INDEX }),
  patchFile(packageDir, "src/query-process.ts", { name: "pi-claude-bridge", marker: MARKER, edits: QUERY_PROCESS }),
  patchFile(packageDir, "src/session-transcript.ts", { name: "pi-claude-bridge", marker: MARKER, edits: TRANSCRIPT }),
];
if (!results.some(Boolean)) {
  console.log("pi-claude-bridge patch: session cleanup already applied");
  process.exit(0);
}
console.log(
  "pi-claude-bridge patch: bridge deletes the Claude Code sessions it created once their writers are gone, " +
    "and imports pi history over a stale transcript (index.ts, query-process.ts, session-transcript.ts)",
);

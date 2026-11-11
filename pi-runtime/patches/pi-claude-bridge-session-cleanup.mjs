// Build-time patch: delete the Claude Code session files the bridge created.
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
// Lifetime
// --------
// Until the bridge cleans up its own sessions. Pinned to the bundled version:
// a version bump must re-check the anchors (session-cleanup.staged.test.mjs
// checks the patched bridge still loads and cleans up).
//
// Failure policy: hard. A missing anchor means the bridge changed shape;
// shipping it unpatched would keep leaking session files. The packaging script
// runs under `set -e`.
//
// Usage: node pi-claude-bridge-session-cleanup.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-session-cleanup.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: session-cleanup";
const EXPECTED_VERSION = "0.12.0";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-session-cleanup.mjs" });

const EDITS = [
  {
    label: "imports",
    from: `import { appendFileSync, mkdirSync, readFileSync, realpathSync, statSync } from "fs";`,
    to: `import { appendFileSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from "fs";
import { getAgentDir } from "@earendil-works/pi-coding-agent";`,
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
// manifest lists them for the next bridge to sweep when this one dies before
// it can.
// One registry per process (Symbol.for), however many module instances load.
interface OwnedSessions {
	paths: Set<string>; // resolved JSONL paths
	manifest: string;
}
const OWNED_SESSIONS_KEY = Symbol.for("claude-bridge:ownedSessions");
const SESSION_FILE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\.jsonl$/i;

function sessionManifestDir(): string {
	return join(getAgentDir(), "claude-bridge-sessions");
}

function ownedSessions(): OwnedSessions | undefined {
	return (globalThis as Record<symbol, unknown>)[OWNED_SESSIONS_KEY] as OwnedSessions | undefined;
}

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
	try {
		mkdirSync(dirname(owned.manifest), { recursive: true });
		// Atomic: a sweep must never read a half-written manifest.
		const tmp = \`\${owned.manifest}.tmp\`;
		writeFileSync(tmp, JSON.stringify({ sessions: [...owned.paths] }));
		renameSync(tmp, owned.manifest);
	} catch (error) {
		debug("WARNING: could not record owned Claude Code sessions:", error);
	}
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
	owned.paths.clear();
	rmSync(owned.manifest, { force: true });
}

function processAlive(pid: number): boolean {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		return (error as NodeJS.ErrnoException).code === "EPERM";
	}
}

/** Delete the sessions of bridge processes that exited without releasing them. */
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
				const { sessions } = JSON.parse(readFileSync(manifest, "utf-8")) as { sessions?: unknown };
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
}`,
  },
  {
    label: "path import",
    from: `import { dirname } from "path";`,
    to: `import { basename, dirname, join } from "path";`,
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
];

if (!patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: EDITS })) {
  console.log("pi-claude-bridge patch: session cleanup already applied");
  process.exit(0);
}
console.log("pi-claude-bridge patch: bridge deletes the Claude Code sessions it created (index.ts)");

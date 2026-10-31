// Build-time patch: rebuild the Claude Code session when Pi's prompt changes.
//
// Why this exists
// ---------------
// pi-claude-bridge resumes one Claude Code session for every turn of a pi
// session and passes Pi's prompt as `--append-system-prompt` each time. But
// Claude Code (2.1.302) stores the system prompt in the session file on the
// session's first query (an `attachment` record with `systemPrompt`) and a
// resume keeps that copy, whatever append it is given. So anything that
// changes Pi's prompt mid-chat never reached Claude: `/explain` turned on in a
// running chat wrote its first page and then stopped updating it, and
// `/orchestrate on` mid-chat likewise.
//
// The bridge now remembers the append each Claude Code session was created
// with (`promptAppend` on its session state). When a query's append differs,
// it marks the session `needsRebuild`, so the turn takes the bridge's existing
// REBUILD path: the file is rewritten from Pi's history (without the stored
// prompt; only `file` attachments are carried) and Claude Code reads the new
// append on that query. A rebuild costs one turn without the prompt cache, so
// it happens only when the prompt actually changed.
//
// Lifetime
// --------
// Until the bridge (or Claude Code) honors a changed append on resume. Pinned
// to the bundled version: a version bump must re-check the anchors
// (prompt-refresh.staged.test.mjs checks the patched bridge marks the rebuild).
//
// Failure policy: hard. A missing anchor means the bridge changed shape;
// shipping it unpatched would silently drop mid-chat prompt changes again. The
// packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-prompt-refresh.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-prompt-refresh.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: prompt-refresh";
const EXPECTED_VERSION = "0.9.2";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-prompt-refresh.mjs" });

const EDITS = [
  {
    label: "SessionState.promptAppend",
    from: `	// REBUILD wipes and rewrites the file to match pi's current history.
	needsRebuild?: boolean;`,
    to: `	// REBUILD wipes and rewrites the file to match pi's current history.
	needsRebuild?: boolean;
	// ${MARKER}: the append Claude Code stored in this session's file on its
	// first query. A resume keeps that copy whatever append it is passed, so a
	// changed prompt forces a REBUILD (markPromptChange). Undefined until a
	// query on this session completed.
	promptAppend?: string;`,
  },
  {
    label: "markPromptChange",
    from: `/**
 * Ensure the shared session has all messages up to (but not including) the last user message.`,
    to: `// ${MARKER}: Claude Code keeps a resumed session's stored system prompt, so
// a query whose append differs from the one its session was created with must
// rebuild the session for Claude to see the change.
function markPromptChange(piSessionId: string | null | undefined, append: string): boolean {
	const state = sessionStateFor(piSessionId);
	if (!state || state.needsRebuild || state.promptAppend === undefined || state.promptAppend === append) return false;
	setSessionStateFor(piSessionId, { ...state, needsRebuild: true });
	debug(\`prompt-refresh: append changed (\${state.promptAppend.length} → \${append.length} chars), marking needsRebuild on session \${state.sessionId.slice(0, 8)}\`);
	return true;
}

/**
 * Ensure the shared session has all messages up to (but not including) the last user message.`,
  },
  {
    label: "provider: mark before sync",
    from: `	const syncResult = syncSharedSession(context.messages, cwd, customToolNameToSdk, cliModel, piSessionId);`,
    to: `	markPromptChange(piSessionId, systemPromptAppend ?? ""); // ${MARKER}
	const syncResult = syncSharedSession(context.messages, cwd, customToolNameToSdk, cliModel, piSessionId);`,
  },
  {
    label: "provider: record the append at query done",
    from: `setSessionStateFor(queryCtx.piSessionId, { ...state, sessionId, cursor, cwd, piSessionId: queryCtx.piSessionId ?? undefined, needsRebuild: queryCtx.missedSteer || state?.needsRebuild });`,
    to: `setSessionStateFor(queryCtx.piSessionId, { ...state, sessionId, cursor, cwd, piSessionId: queryCtx.piSessionId ?? undefined, needsRebuild: queryCtx.missedSteer || state?.needsRebuild,
						// ${MARKER}: a resumed session keeps the append it was created
						// with; a new or rebuilt one (no promptAppend yet) took this one.
						promptAppend: state?.promptAppend ?? (systemPromptAppend ?? "") });`,
  },
  {
    label: "__test.markPromptChange",
    from: `	markRebuildForSession,
	getHistoryRewritten:`,
    to: `	markRebuildForSession,
	markPromptChange, // ${MARKER}
	getHistoryRewritten:`,
  },
];

if (!patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: EDITS })) {
  console.log("pi-claude-bridge patch: prompt refresh already applied");
  process.exit(0);
}
console.log("pi-claude-bridge patch: a changed prompt rebuilds the Claude Code session (index.ts)");

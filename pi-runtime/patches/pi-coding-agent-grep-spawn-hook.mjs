// Build-time patch: let an extension wrap the ripgrep process behind `grep`.
//
// Why this exists
// ---------------
// Cypher's permission extension (extensions/cypher-permissions.ts) replaces
// the built-in file tools with copies whose filesystem access it checks. The
// `grep` tool reads matched lines through its pluggable operations, but the
// search itself is a ripgrep child process Pi spawns directly: which files
// match, and on which line, would still come from files the agent may not
// read (a pattern tried one character at a time reveals a secret). The patch
// adds a `spawnHook` option to `createGrepToolDefinition`, the same shape as
// the bash tool's, so the extension can run ripgrep inside Landstrip.
//
// Only `dist/core/tools/grep.js` is patched: it is what `dist/index.js`
// exports to extensions, and the Runtime's `bin/pi` runs the unbundled
// `dist/cli.js`. The extension also filters result lines by path, so an
// unpatched copy degrades to a policy check instead of an open search.
//
// Lifetime
// --------
// Until Pi's grep accepts a spawn hook. Pinned to the bundled version: a
// version bump must re-check the anchor (permissions.staged.test.mjs runs
// grep through the hook).
//
// Failure policy: hard. A missing anchor means Pi changed shape; shipping it
// unpatched would leave ripgrep outside the sandbox.
//
// Usage: node pi-coding-agent-grep-spawn-hook.mjs <pi-coding-agent-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-coding-agent-grep-spawn-hook.mjs <pi-coding-agent-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: grep-spawn-hook";
const NAME = "pi-coding-agent";
requireVersion(packageDir, { name: NAME, expected: "1.1.0", script: "pi-coding-agent-grep-spawn-hook.mjs" });

const patchedJs = patchFile(packageDir, "dist/core/tools/grep.js", {
  name: NAME,
  marker: MARKER,
  edits: [
    {
      label: "ripgrep spawn",
      from: `                        const child = spawn(rgPath, args, { stdio: ["ignore", "pipe", "pipe"] });`,
      to: `                        // ${MARKER}: an extension may wrap the ripgrep process.
                        const spawnTarget = typeof options?.spawnHook === "function"
                            ? options.spawnHook({ command: rgPath, args })
                            : { command: rgPath, args };
                        const child = spawn(spawnTarget.command, spawnTarget.args, { stdio: ["ignore", "pipe", "pipe"] });`,
    },
  ],
});

const patchedTypes = patchFile(packageDir, "dist/core/tools/grep.d.ts", {
  name: NAME,
  marker: MARKER,
  edits: [
    {
      label: "options type",
      from: `export interface GrepToolOptions {`,
      to: `export interface GrepToolOptions {
    /** ${MARKER}: wrap the ripgrep process (command and arguments). */
    spawnHook?: (target: {
        command: string;
        args: string[];
    }) => {
        command: string;
        args: string[];
    };`,
    },
  ],
});
if (patchedJs || patchedTypes) console.log(`${NAME} patch: grep accepts a ripgrep spawn hook (grep.js, grep.d.ts)`);

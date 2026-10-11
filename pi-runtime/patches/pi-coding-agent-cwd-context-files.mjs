// Build-time patch: load project context files from the working directory only.
//
// Why this exists
// ---------------
// Pi's `loadProjectContextFiles` walks from the session cwd up to `/`, loading
// one AGENTS.md / CLAUDE.md per directory. A chat started in a nested folder —
// a worktree under `~/.cypher/worktrees`, a package inside a monorepo, any
// project under a home directory that has its own AGENTS.md — silently picked
// up every ancestor's instructions, written for other projects or harnesses.
//
// The walk now stops after the cwd itself. Pi's agent-dir context file (the
// isolated `pi-runtime/agent/AGENTS.md`, never `~/.pi`) is a deliberate
// user-level file, not an ancestor, and still loads first. `--no-context-files`
// is unchanged.
//
// pi-claude-bridge forwards exactly this list to Claude Code (its own discovery
// is excluded, see pi-claude-bridge-context-files.mjs), so the Claude provider
// follows the same rule.
//
// Two copies are patched: `dist/core/resource-loader.js`, which the Runtime's
// `bin/pi` (`dist/cli.js`) and the SDK entry run, and the minified copy in
// `dist/bundle/chunks/`, which Pi's own `bin` entry runs.
//
// Lifetime
// --------
// Until Pi offers a setting for the walk. Pinned to the bundled version: a
// version bump must re-check the anchors (context-files.staged.test.mjs checks
// the patched loader).
//
// Failure policy: hard. A missing anchor means Pi changed shape; shipping it
// unpatched would quietly bring the ancestor walk back. The packaging script
// runs under `set -e`.
//
// Usage: node pi-coding-agent-cwd-context-files.mjs <pi-coding-agent-package-dir>

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-coding-agent-cwd-context-files.mjs <pi-coding-agent-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: cwd-context-files";
const EXPECTED_VERSION = "1.0.1";
const NAME = "pi-coding-agent";
requireVersion(packageDir, { name: NAME, expected: EXPECTED_VERSION, script: "pi-coding-agent-cwd-context-files.mjs" });

const UNBUNDLED = "dist/core/resource-loader.js";
const UNBUNDLED_EDITS = [
  {
    label: "ancestor walk",
    from: `        const parentDir = dirname(currentDir);
        if (parentDir === currentDir)
            break;
        currentDir = parentDir;
    }
    contextFiles.push(...ancestorContextFiles);`,
    to: `        // ${MARKER}: the cwd only, never its ancestors.
        break;
    }
    contextFiles.push(...ancestorContextFiles);`,
  },
];

// The bundle's chunk names are content hashes and its import aliases are
// numbered (`dirname13`), so find the one chunk that defines the loader.
const BUNDLE_FUNCTION = "function loadProjectContextFiles(options){";
const BUNDLE_WALK = /let parentDir=(dirname\d*)\(currentDir\);if\(parentDir===currentDir\)break;currentDir=parentDir\}return contextFiles\.push\(\.\.\.ancestorContextFiles\)/;
const chunksDir = join(packageDir, "dist/bundle/chunks");
const chunks = readdirSync(chunksDir).filter(
  (file) => file.endsWith(".js") && readFileSync(join(chunksDir, file), "utf-8").includes(BUNDLE_FUNCTION),
);
if (chunks.length !== 1) {
  console.error(
    `${NAME} patch: expected one bundle chunk defining loadProjectContextFiles, found ${chunks.length}.\n` +
      `${NAME} changed shape — update pi-runtime/patches/ before packaging.`,
  );
  process.exit(1);
}
const bundled = `dist/bundle/chunks/${chunks[0]}`;
const bundledSource = readFileSync(join(packageDir, bundled), "utf-8");
const walk = bundledSource.match(BUNDLE_WALK)?.[0];
if (!walk && !bundledSource.includes(MARKER)) {
  console.error(
    `${NAME} patch: anchor not found (${chunks[0]}: bundled ancestor walk).\n` +
      `${NAME} changed shape — update pi-runtime/patches/ before packaging.`,
  );
  process.exit(1);
}
const BUNDLE_EDITS = [
  {
    label: "bundled ancestor walk",
    from: walk ?? MARKER,
    to: `/* ${MARKER}: the cwd only, never its ancestors. */break}return contextFiles.push(...ancestorContextFiles)`,
  },
];

const patched = [
  patchFile(packageDir, UNBUNDLED, { name: NAME, marker: MARKER, edits: UNBUNDLED_EDITS }),
  patchFile(packageDir, bundled, { name: NAME, marker: MARKER, edits: BUNDLE_EDITS }),
];
if (!patched.some(Boolean)) {
  console.log(`${NAME} patch: cwd-only context files already applied`);
  process.exit(0);
}
console.log(`${NAME} patch: context files load from the cwd only (resource-loader.js, ${chunks[0]})`);

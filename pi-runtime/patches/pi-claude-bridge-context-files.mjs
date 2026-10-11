// Build-time patch: keep Claude Code from loading CLAUDE.local.md.
//
// Why this exists
// ---------------
// Pi owns context files on both Claude routes: pi-claude-bridge passes Claude
// Code `claudeMdExcludes` so it loads none of its own memory files, and the
// provider path forwards Pi's list — the cwd's AGENTS.md / CLAUDE.md only
// (see pi-coding-agent-cwd-context-files.mjs). The bridge's excludes cover
// CLAUDE.md, AGENTS.md and .claude/rules but not Local memory, so Claude Code
// still loaded every CLAUDE.local.md from the cwd up to `/` — exactly the
// ancestor walk Runtime removed from Pi.
//
// Adds `**/CLAUDE.local.md` to the excludes, which both the provider path and
// AskClaude pass. Managed/policy memory stays unexcludable by Claude Code's
// design.
//
// Lifetime
// --------
// Until the bridge excludes Local memory itself. Pinned to the bundled
// version: a version bump must re-check the anchor (context-files.staged.test.mjs
// checks the patched bridge still loads).
//
// Failure policy: hard. A missing anchor means the bridge changed shape;
// shipping it unpatched would let ancestor CLAUDE.local.md files back in. The
// packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-context-files.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-context-files.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: context-files";
const EXPECTED_VERSION = "0.9.2";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-context-files.mjs" });

const EDITS = [
  {
    label: "CLAUDE_MD_EXCLUDES",
    from: `const CLAUDE_MD_EXCLUDES = ["**/CLAUDE.md", "**/AGENTS.md", "**/.claude/rules/**"];`,
    to: `// ${MARKER}: Local memory too, which Claude Code also reads from every ancestor.
const CLAUDE_MD_EXCLUDES = ["**/CLAUDE.md", "**/CLAUDE.local.md", "**/AGENTS.md", "**/.claude/rules/**"];`,
  },
];

if (!patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: EDITS })) {
  console.log("pi-claude-bridge patch: context-file excludes already applied");
  process.exit(0);
}
console.log("pi-claude-bridge patch: Claude Code excludes CLAUDE.local.md (index.ts)");

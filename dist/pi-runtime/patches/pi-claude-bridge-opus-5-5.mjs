// Build-time patch: give pi-claude-bridge a claude-opus-5-5 row.
//
// Why this exists
// ---------------
// pi-claude-bridge deliberately owns no model list. Its src/models.ts says the
// picker "is driven by pi-ai's anthropic catalog: models appear (and disappear)
// with it, no per-model code here", and index.ts builds it from
// `getModels("anthropic")` imported from "@earendil-works/pi-ai/compat" — which
// resolves to getBuiltinModels(), the catalog compiled into pi. That makes the
// agent dir's models.json (pi-ai's supported override) unreachable from the
// bridge, so the runtime bundle is the only place we can add the row.
//
// Claude Code serves the model today: its published catalog lists
// claude-opus-5-5 with min_claude_code_version 2.1.280, and the bridge drives
// the host CLI. pi-ai has no entry for it as of 0.87.0, so the bridge cannot
// offer a model the CLI would happily run.
//
// Lifetime
// --------
// Temporary. The injected row is skipped at runtime as soon as pi-ai's catalog
// carries claude-opus-5-5, so a pi-ai bump cannot produce a duplicate or shadow
// the real entry. This script also warns at build time when that day arrives,
// which is the signal to delete this file and its call in
// scripts/package-pi-runtime.sh.
//
// Failure policy: hard. A missing anchor means pi-claude-bridge changed shape,
// and silently shipping an unpatched bundle would reintroduce the missing model
// with no signal. The packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-opus-5-5.mjs <bridge-package-dir> [pi-package-dir]

import { readFileSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

const [, , bridgeDir, piDir] = process.argv;
if (!bridgeDir) {
  console.error("usage: pi-claude-bridge-opus-5-5.mjs <bridge-package-dir> [pi-package-dir]");
  process.exit(1);
}

const target = join(bridgeDir, "src/models.ts");
if (!existsSync(target)) {
  console.error(`pi-claude-bridge patch: ${target} not found`);
  process.exit(1);
}

const MARKER = "CYPHER-RUNTIME-PATCH: opus-5-5";

// Fields mirror pi-ai's own claude-opus-5 row. buildModels projects only
// id/name/reasoning/input/contextWindow/maxTokens/thinkingLevelMap; the rest
// keeps the shape recognisable next to a real catalog entry. Limits follow
// Claude Code's published catalog for Opus 5.5 (1M in, 128K out).
const DECLARATION = `
// ${MARKER}
// Injected by cypher's runtime packaging because @earendil-works/pi-ai has no
// claude-opus-5-5 row yet, while the Claude Code CLI the bridge drives serves
// the model. Skipped automatically once the real catalog entry appears.
const CYPHER_CATALOG_FALLBACKS = [
	{
		id: "claude-opus-5-5",
		name: "Claude Opus 5.5",
		api: "anthropic-messages",
		provider: "anthropic",
		baseUrl: "https://api.anthropic.com",
		reasoning: true,
		input: ["text", "image"],
		cost: { input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25 },
		contextWindow: 1_000_000,
		maxTokens: 128_000,
		thinkingLevelMap: { off: null, xhigh: "xhigh", max: "max" },
	},
];

`;

const EDITS = [
  {
    label: "buildModels",
    from: `export function buildModels<T extends { id: string; [key: string]: any }>(piAiModels: T[]) {
	return piAiModels`,
    to: `export function buildModels<T extends { id: string; [key: string]: any }>(piAiModels: T[]) {
	// ${MARKER} — a real catalog entry always wins.
	const withFallbacks = [...piAiModels];
	for (const extra of CYPHER_CATALOG_FALLBACKS) {
		if (!withFallbacks.some((m) => m.id === extra.id)) withFallbacks.push(extra as unknown as T);
	}
	return withFallbacks`,
  },
  {
    // The registered contextWindow must match the window the bridge actually
    // requests or pi's status bar and compaction threshold misreport, so the
    // 1M id and the 1M registration have to move together.
    label: "MEASURED_ONE_M",
    from: `const MEASURED_ONE_M = new Set([
	"claude-fable-5",`,
    to: `const MEASURED_ONE_M = new Set([
	"claude-opus-5-5", // ${MARKER} — verified 1M against the host CLI.
	"claude-fable-5",`,
  },
  {
    label: "declaration",
    from: "export function buildModels<T",
    to: `${DECLARATION}export function buildModels<T`,
  },
];

const source = readFileSync(target, "utf-8");
if (source.includes(MARKER)) {
  console.log("pi-claude-bridge patch: already applied");
  process.exit(0);
}

let out = source;
for (const { label, from, to } of EDITS) {
  if (!out.includes(from)) {
    console.error(
      `pi-claude-bridge patch: anchor not found (${label}).\n` +
        "pi-claude-bridge changed shape — update dist/pi-runtime/patches/ before packaging.",
    );
    process.exit(1);
  }
  out = out.replace(from, to);
}

writeFileSync(target, out);
console.log("pi-claude-bridge patch: added claude-opus-5-5 (1M)");

// Advisory only: once pi-ai ships the model the injection goes inert, and this
// patch should be deleted rather than left to rot.
if (piDir) {
  const chunks = join(piDir, "dist/bundle/chunks");
  try {
    const shipped = readdirSync(chunks).some(
      (file) => file.endsWith(".js") && readFileSync(join(chunks, file), "utf-8").includes('"claude-opus-5-5"'),
    );
    if (shipped) {
      console.log(
        "pi-claude-bridge patch: NOTE — pi-ai now ships claude-opus-5-5. " +
          "The injected row is inert; delete dist/pi-runtime/patches/pi-claude-bridge-opus-5-5.mjs " +
          "and its call in scripts/package-pi-runtime.sh.",
      );
    }
  } catch {
    // Layout probing is best-effort; never fail packaging over the advisory.
  }
}

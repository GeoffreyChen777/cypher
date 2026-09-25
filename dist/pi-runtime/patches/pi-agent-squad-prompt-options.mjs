// Build-time patch: let pi-agent-squad's prompt section reach Claude Code.
//
// Why this exists
// ---------------
// pi-agent-squad's `before_agent_start` handler appends a section to the
// system prompt: the orchestrator prompt after `/orchestrate on`, otherwise
// the "automatic delegation disabled" guard. It does so by returning a whole
// new `systemPrompt`, which Pi stores as `forceSystemPrompt`. pi-claude-bridge
// never sends Pi's prompt: it appends only the portable parts of Pi's prompt
// options (context files, skills, custom prompt, `appendSystemPrompt`) to
// Claude Code's own preset. A forced prompt is none of those, so on the bridge
// the section vanished without an error and `/orchestrate` did nothing.
//
// The handler now adds its section to `event.systemPromptOptions
// .appendSystemPrompt` instead. Pi passes one options object to every
// `before_agent_start` handler (documented as mutable) and keeps it for the
// run, and the bridge reads that object again at `agent_start` and every
// `turn_start`, so the section arrives whichever extension loads first. Pi's
// own prompt carries it too, for every other provider. Without prompt options
// (older Pi), or when an earlier handler already forced a prompt that would
// ignore them, it falls back to the upstream return.
//
// Lifetime
// --------
// Until pi-agent-squad appends through the prompt options itself. Pinned to
// the bundled version: a version bump must re-check the anchors (and the
// behaviour — pi-agent-squad-prompt-options.test.mjs loads both extensions).
//
// Failure policy: hard. A missing anchor means pi-agent-squad changed shape;
// silently shipping an unpatched bundle would make `/orchestrate` a no-op on
// the Claude bridge again. The packaging script runs under `set -e`.
//
// Usage: node pi-agent-squad-prompt-options.mjs <pi-agent-squad-package-dir>

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [, , packageDir] = process.argv;
if (!packageDir) {
  console.error("usage: pi-agent-squad-prompt-options.mjs <pi-agent-squad-package-dir>");
  process.exit(1);
}

const MARKER = "CYPHER-RUNTIME-PATCH: prompt-options";
const EXPECTED_VERSION = "0.9.0";

const version = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf-8")).version;
if (version !== EXPECTED_VERSION) {
  console.error(
    `pi-agent-squad patch: expected ${EXPECTED_VERSION}, found ${version}.\n` +
      "Re-check dist/pi-runtime/patches/pi-agent-squad-prompt-options.mjs against the new version.",
  );
  process.exit(1);
}

const EDITS = [
  {
    label: "appendSystemPromptSection",
    from: `].join("\\n");

function orchestratorPromptPath(): string {`,
    to: `].join("\\n");

/**
 * ${MARKER}
 * Add a section through Pi's prompt options rather than replacing the prompt.
 * pi-claude-bridge forwards \`appendSystemPrompt\` to Claude Code but drops a
 * replaced (forced) prompt; Pi shares these options across
 * \`before_agent_start\` handlers and keeps them for the run.
 */
function appendSystemPromptSection(
	event: { systemPrompt: string; systemPromptOptions?: { appendSystemPrompt?: string; forceSystemPrompt?: string } },
	section: string,
): { systemPrompt: string } | undefined {
	const options = event.systemPromptOptions;
	// Older Pi has no options, and an earlier handler's forced prompt ignores them.
	if (!options || options.forceSystemPrompt !== undefined) {
		return { systemPrompt: event.systemPrompt + "\\n\\n" + section };
	}
	options.appendSystemPrompt = options.appendSystemPrompt ? \`\${options.appendSystemPrompt}\\n\\n\${section}\` : section;
	return undefined;
}

function orchestratorPromptPath(): string {`,
  },
  {
    label: "before_agent_start",
    from: `			if (!prompt) return;
			return { systemPrompt: event.systemPrompt + "\\n\\n" + prompt };
		}
		return { systemPrompt: event.systemPrompt + "\\n\\n" + SUBAGENT_USAGE_DISABLED_GUARD };`,
    to: `			if (!prompt) return;
			// ${MARKER}
			return appendSystemPromptSection(event, prompt);
		}
		return appendSystemPromptSection(event, SUBAGENT_USAGE_DISABLED_GUARD);`,
  },
];

const target = join(packageDir, "index.ts");
if (!existsSync(target)) {
  console.error(`pi-agent-squad patch: ${target} not found`);
  process.exit(1);
}
const source = readFileSync(target, "utf-8");
if (source.includes(MARKER)) {
  console.log("pi-agent-squad patch: prompt options already applied");
  process.exit(0);
}
let out = source;
for (const { label, from, to } of EDITS) {
  const at = out.indexOf(from);
  if (at < 0 || out.indexOf(from, at + 1) >= 0) {
    console.error(
      `pi-agent-squad patch: anchor ${at < 0 ? "not found" : "not unique"} (index.ts: ${label}).\n` +
        "pi-agent-squad changed shape — update dist/pi-runtime/patches/ before packaging.",
    );
    process.exit(1);
  }
  out = out.replace(from, to);
}
writeFileSync(target, out);
console.log("pi-agent-squad patch: prompt sections through appendSystemPrompt (index.ts)");

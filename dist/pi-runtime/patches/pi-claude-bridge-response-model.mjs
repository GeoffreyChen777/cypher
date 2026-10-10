// Build-time patch: record the model Claude Code says answered.
//
// Why this exists
// ---------------
// Cypher labels every answer with the model that wrote it — Pi's
// `responseModel` when the provider named a different model than requested,
// else `model`. pi-claude-bridge builds each assistant message itself and only
// ever stamps the requested `model.id`, although every Anthropic
// `message_start` (and the non-streaming fallback's assistant message) carries
// the model that actually served it. A Claude Code fallback to another model
// was invisible.
//
// The bridge now copies that model onto the message as `responseModel` when it
// differs from the requested id, skipping Claude Code's `<synthetic>` stand-ins.
//
// Lifetime
// --------
// Until the bridge records it itself. Pinned to the bundled version: a version
// bump must re-check the anchors (response-model.test.mjs checks the patched
// bridge still loads).
//
// Failure policy: hard. A missing anchor means the bridge changed shape;
// shipping it unpatched would silently drop the label for every Claude answer.
// The packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-response-model.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-response-model.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: response-model";
const EXPECTED_VERSION = "0.9.1";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-response-model.mjs" });

const EDITS = [
  {
    label: "noteResponseModel",
    from: `// --- Usage helpers ---
`,
    to: `// --- Usage helpers ---

/** ${MARKER}
 * The model Claude Code says served this message, when it is not the one
 * requested: Pi's \`responseModel\`, which Cypher shows on the answer. */
function noteResponseModel(output: AssistantMessage, served: unknown, model: Model<any>): void {
	if (typeof served !== "string" || !served || served === "<synthetic>" || served === model.id) return;
	output.responseModel = served;
}
`,
  },
  {
    label: "message_start",
    from: `		c.turnStreamBlockStart = c.turnBlocks.length;
		if (event.message?.usage) recordUsage(c.turnOutput, event.message.usage, model);
		return;`,
    to: `		c.turnStreamBlockStart = c.turnBlocks.length;
		if (event.message?.usage) recordUsage(c.turnOutput, event.message.usage, model);
		// ${MARKER}
		noteResponseModel(c.turnOutput, event.message?.model, model);
		return;`,
  },
  {
    label: "processAssistantMessage",
    from: `	const assistantMsg = (message as any).message;
	if (!assistantMsg?.content) return;
`,
    to: `	const assistantMsg = (message as any).message;
	if (!assistantMsg?.content) return;
	// ${MARKER}
	if (c.turnOutput) noteResponseModel(c.turnOutput, assistantMsg.model, model);
`,
  },
];

if (!patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: EDITS })) {
  console.log("pi-claude-bridge patch: response model already applied");
  process.exit(0);
}
console.log("pi-claude-bridge patch: messages record responseModel (index.ts)");

// Build-time patch: record the model an OpenAI Responses stream says answered.
//
// Why this exists
// ---------------
// An assistant message carries `model` (what Pi requested) and, when the
// provider names a different one, `responseModel`. Cypher labels every answer
// with the model that wrote it, so a relay or gateway quietly serving another
// model shows up in the transcript. pi-ai records `responseModel` for the
// Anthropic Messages and Chat Completions streams, but its shared Responses
// stream (OpenAI Responses, Azure, ChatGPT/Codex) never reads
// `response.model`, so those answers could only ever show the requested model.
//
// The stream now notes `response.model` from `response.created` and from the
// terminal response, the same way Chat Completions treats `chunk.model`: set
// only when it differs from the requested id, and the first value stands.
//
// Lifetime
// --------
// Until pi-ai records it itself. Pinned to the bundled version: a version bump
// must re-check the anchors (and the behaviour — response-model.test.mjs feeds
// the patched stream).
//
// Failure policy: hard. A missing anchor means pi-ai changed shape; shipping
// an unpatched runtime would silently drop the label for every Responses
// model. The packaging script runs under `set -e`.
//
// Usage: node pi-ai-response-model.mjs <@earendil-works/pi-ai-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-ai-response-model.mjs <pi-ai-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: response-model";
const EXPECTED_VERSION = "1.0.1";
requireVersion(packageDir, { name: "pi-ai", expected: EXPECTED_VERSION, script: "pi-ai-response-model.mjs" });

const EDITS = [
  {
    label: "noteResponseModel",
    from: `    const finalizeResponse = (response) => {
        sawTerminalResponseEvent = true;
        backfillReasoningSignatures(response.output ?? []);
        if (response?.id) {
            output.responseId = response.id;
        }`,
    to: `    // ${MARKER}
    // The model the provider says answered, when it is not the one requested
    // (as the Chat Completions stream treats \`chunk.model\`).
    const noteResponseModel = (response) => {
        const served = response?.model;
        if (typeof served === "string" && served.length > 0 && served !== model.id) {
            output.responseModel ||= served;
        }
    };
    const finalizeResponse = (response) => {
        sawTerminalResponseEvent = true;
        backfillReasoningSignatures(response.output ?? []);
        if (response?.id) {
            output.responseId = response.id;
        }
        noteResponseModel(response);`,
  },
  {
    label: "response.created",
    from: `        if (event.type === "response.created") {
            output.responseId = event.response.id;
        }`,
    to: `        if (event.type === "response.created") {
            output.responseId = event.response.id;
            // ${MARKER}
            noteResponseModel(event.response);
        }`,
  },
];

if (!patchFile(packageDir, "dist/api/openai-responses-shared.js", { name: "pi-ai", marker: MARKER, edits: EDITS })) {
  console.log("pi-ai patch: response model already applied");
  process.exit(0);
}
console.log("pi-ai patch: Responses streams record responseModel (openai-responses-shared.js)");

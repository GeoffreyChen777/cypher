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

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [, , packageDir] = process.argv;
if (!packageDir) {
  console.error("usage: pi-ai-response-model.mjs <pi-ai-package-dir>");
  process.exit(1);
}

const MARKER = "CYPHER-RUNTIME-PATCH: response-model";
const EXPECTED_VERSION = "1.0.1";

const version = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf-8")).version;
if (version !== EXPECTED_VERSION) {
  console.error(
    `pi-ai patch: expected ${EXPECTED_VERSION}, found ${version}.\n` +
      "Re-check dist/pi-runtime/patches/pi-ai-response-model.mjs against the new version.",
  );
  process.exit(1);
}

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

const target = join(packageDir, "dist", "api", "openai-responses-shared.js");
if (!existsSync(target)) {
  console.error(`pi-ai patch: ${target} not found`);
  process.exit(1);
}
const source = readFileSync(target, "utf-8");
if (source.includes(MARKER)) {
  console.log("pi-ai patch: response model already applied");
  process.exit(0);
}
let out = source;
for (const { label, from, to } of EDITS) {
  const at = out.indexOf(from);
  if (at < 0 || out.indexOf(from, at + 1) >= 0) {
    console.error(
      `pi-ai patch: anchor ${at < 0 ? "not found" : "not unique"} (openai-responses-shared.js: ${label}).\n` +
        "pi-ai changed shape — update dist/pi-runtime/patches/ before packaging.",
    );
    process.exit(1);
  }
  out = out.replace(from, to);
}
writeFileSync(target, out);
console.log("pi-ai patch: Responses streams record responseModel (openai-responses-shared.js)");

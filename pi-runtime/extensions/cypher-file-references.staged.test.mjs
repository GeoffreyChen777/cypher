// The file-reference instructions in a prompt Pi really builds: they land in
// the addendum, ahead of the user's APPEND_SYSTEM.md text, with and without
// a replacement prompt (a subagent's). No network, provider or session.
//
// Run against a staged runtime:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

import extension, { FILE_REFERENCES } from "./cypher-file-references.ts";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const pi = join(stage, "npm", "node_modules", "@earendil-works", "pi-coding-agent");
const { buildSystemPromptSections, normalizeBuildSystemPromptOptions } = await import(
  pathToFileURL(join(pi, "dist", "core", "system-prompt.js")).href
);

function render(input) {
  const options = normalizeBuildSystemPromptOptions(input);
  let handler;
  extension({ on: (name, h) => name === "before_agent_start" && (handler = h) });
  handler({ systemPromptOptions: options });
  return buildSystemPromptSections(options);
}

test("the addendum carries the block, then the user's text", () => {
  const sections = render({ cwd: "/repo", appendSystemPrompt: "Answer in English." });
  assert.equal(
    sections.addendum,
    `<addendum>\n${FILE_REFERENCES}\n\nAnswer in English.\n</addendum>`,
  );
});

test("a replacement prompt keeps the block", () => {
  const sections = render({ cwd: "/repo", customPrompt: "You review diffs." });
  assert.equal(sections.preamble, "You review diffs.");
  assert.equal(sections.addendum, `<addendum>\n${FILE_REFERENCES}\n</addendum>`);
});

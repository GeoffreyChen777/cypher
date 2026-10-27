import assert from "node:assert/strict";
import { test } from "node:test";

import extension, { GUIDE, SECTION } from "./cypher-artifacts.ts";

function registered() {
  const handlers = new Map();
  extension({ on: (event, handler) => handlers.set(event, handler) });
  return handlers;
}

test("every turn's system prompt carries the artifacts section", () => {
  const handler = registered().get("before_agent_start");
  const event = { systemPromptOptions: { sections: { other: "kept" } } };
  assert.equal(handler(event), undefined, "the prompt is amended, never replaced");
  assert.deepEqual(event.systemPromptOptions.sections, { other: "kept", [SECTION]: GUIDE });
});

test("the section name is one Pi accepts", () => {
  // Pi's SYSTEM_PROMPT_SECTION_NAME.
  assert.match(SECTION, /^[a-z][a-z0-9_-]*$/);
});

test("the guide names the fence the desktop chat renders", () => {
  assert.match(GUIDE, /`html artifact title="Short title"`/);
  assert.match(GUIDE, /`svg artifact title="…"`/);
});

import assert from "node:assert/strict";
import { test } from "node:test";

import extension, { FILE_REFERENCES, withFileReferences } from "./cypher-file-references.ts";

function beforeAgentStart() {
  const handlers = [];
  extension({ on: (name, handler) => name === "before_agent_start" && handlers.push(handler) });
  assert.equal(handlers.length, 1);
  return handlers[0];
}

test("the block leads the appended prompt, the user's text follows", () => {
  const handler = beforeAgentStart();
  const options = { appendSystemPrompt: "Answer in English.\n", sections: {} };
  handler({ systemPromptOptions: options });
  assert.equal(options.appendSystemPrompt, `${FILE_REFERENCES}\n\nAnswer in English.`);
  // Nothing is added as a custom section: those can replay out of order.
  assert.deepEqual(options.sections, {});
});

test("without user text the block stands alone", () => {
  const handler = beforeAgentStart();
  for (const appendSystemPrompt of [undefined, "", "  \n"]) {
    const options = { appendSystemPrompt };
    handler({ systemPromptOptions: options });
    assert.equal(options.appendSystemPrompt, FILE_REFERENCES);
  }
});

test("a second pass adds nothing", () => {
  const once = withFileReferences("Be brief.");
  assert.equal(withFileReferences(once), once);
});

test("the instructions name the shapes the transcript links", () => {
  for (const shape of ["`src/app/main.py`", "`src/app/main.py:42`", "`src/app/main.py:42-58`"]) {
    assert.ok(FILE_REFERENCES.includes(shape), shape);
  }
  assert.match(FILE_REFERENCES, /^<file_references>\n[\s\S]*\n<\/file_references>$/);
});

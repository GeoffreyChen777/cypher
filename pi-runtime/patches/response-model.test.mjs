// Tests for the response-model patches: the patched pi-ai Responses stream
// records the model the provider says answered, and the patched
// pi-claude-bridge still loads. No network, provider or session.
//
// Run against a staged runtime whose pi-ai and bridge are already patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const MARKER = "CYPHER-RUNTIME-PATCH: response-model";

// Keep the bridge off the real agent directory.
const scratch = mkdtempSync(join(tmpdir(), "pi-response-model-"));
process.env.PI_CODING_AGENT_DIR = join(scratch, "agent");
after(() => rmSync(scratch, { recursive: true, force: true }));

const { processResponsesStream } = await import(
  pathToFileURL(join(modules, "@earendil-works/pi-ai", "dist", "api", "openai-responses-shared.js")).href
);

const MODEL = {
  id: "gpt-6-astra",
  provider: "mvp-lab",
  api: "openai-responses",
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
};

/** One Responses stream whose `response.created` and terminal response name
 * `created` and `completed` as the model; the message it leaves behind. */
async function respond({ created, completed }) {
  const response = (model) => ({ id: "resp_1", status: "completed", output: [], ...(model ? { model } : {}) });
  async function* events() {
    yield { type: "response.created", response: response(created) };
    yield { type: "response.completed", response: response(completed) };
  }
  const output = {
    role: "assistant",
    content: [],
    api: MODEL.api,
    provider: MODEL.provider,
    model: MODEL.id,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    stopReason: "stop",
    timestamp: 0,
  };
  await processResponsesStream(events(), output, { push() {} }, MODEL, {});
  return output;
}

test("a Responses stream records the model that answered", async () => {
  const output = await respond({ created: "gpt-5.4-mini", completed: "gpt-5.4-mini" });
  assert.equal(output.responseModel, "gpt-5.4-mini");
  assert.equal(output.model, "gpt-6-astra");
});

test("the terminal response names it when response.created did not", async () => {
  const output = await respond({ created: undefined, completed: "gpt-5.4-mini" });
  assert.equal(output.responseModel, "gpt-5.4-mini");
});

test("the requested model leaves responseModel unset", async () => {
  const output = await respond({ created: "gpt-6-astra", completed: "gpt-6-astra" });
  assert.equal("responseModel" in output, false);
  const unnamed = await respond({ created: undefined, completed: undefined });
  assert.equal("responseModel" in unnamed, false);
});

test("the patched bridge still loads", async () => {
  const entry = join(modules, "pi-claude-bridge", "src", "index.ts");
  const source = readFileSync(entry, "utf-8");
  // The helper and both call sites (streamed and non-streaming messages).
  assert.equal(source.split(MARKER).length - 1, 3);
  const piRequire = createRequire(join(modules, "@earendil-works/pi-coding-agent", "package.json"));
  const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
  const bridge = await createJiti(import.meta.url).import(entry);
  assert.equal(typeof bridge.default, "function");
});

// Tests for the Claude Code web-search patches: the patched pi-claude-bridge
// publishes its WebSearch query, and the patched pi-web-search hands a search
// to it when pi-web-search-claude-bridge has pinned a `claude-bridge/*` model —
// and only then. A stand-in replaces the bridge's query, so no network,
// provider or Claude Code.
//
// CYPHER_LIVE_CLAUDE_SEARCH=1 adds one real search through the bridge's query
// (Claude Code must be signed in; CYPHER_CLAUDE_EXECUTABLE optionally names
// the `claude` to run). It uses the subscription, so packaging never sets it.
//
// Run against a staged runtime whose bridge and pi-web-search are patched:
//   CYPHER_PI_RUNTIME_STAGE=<stage> <stage>/bin/node --test <this file>
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

const stage = process.env.CYPHER_PI_RUNTIME_STAGE;
assert.ok(stage, "CYPHER_PI_RUNTIME_STAGE must point at a staged Pi runtime");
const modules = join(stage, "npm", "node_modules");
const SEARCH_KEY = Symbol.for("claude-bridge:webSearch");

// Keep the bridge and the fallback package off the real agent directory.
const scratch = realpathSync(mkdtempSync(join(tmpdir(), "pi-web-search-")));
const agentDir = join(scratch, "agent");
mkdirSync(agentDir, { recursive: true });
process.env.PI_CODING_AGENT_DIR = agentDir;
delete process.env.PI_WEB_SEARCH_CONFIG;
after(() => rmSync(scratch, { recursive: true, force: true }));

const piRequire = createRequire(join(modules, "@earendil-works/pi-coding-agent", "package.json"));
const { createJiti } = await import(pathToFileURL(piRequire.resolve("jiti")).href);
const jiti = createJiti(import.meta.url);

const BRIDGE = join(modules, "pi-claude-bridge", "src", "index.ts");
const bridge = await jiti.import(BRIDGE);
const bridgeSearch = globalThis[SEARCH_KEY];
const { webSearch } = await jiti.import(join(modules, "pi-web-search", "src", "web_search.ts"));
const fallback = await jiti.import(join(modules, "pi-web-search-claude-bridge", "index.ts"));

const CLAUDE = { id: "claude-haiku-4-5", provider: "claude-bridge", api: "claude-bridge", name: "Claude Haiku 4.5" };
const GPT = { id: "gpt-5.5", provider: "openai-codex", api: "openai-codex-responses", name: "GPT-5.5" };
const registry = {
  find: (provider, id) => [CLAUDE, GPT].find((model) => model.provider === provider && model.id === id),
  getAvailable: () => [CLAUDE, GPT],
};
const context = (model) => ({ model, modelRegistry: registry });

/** Pins `model` the way Cypher's Claude → Manage does, then lets the fallback
 * package route the next web_search for a conversation on `current`. */
function pin(model, current = CLAUDE) {
  writeFileSync(
    join(agentDir, fallback.CONFIG_FILE),
    `${JSON.stringify(model ? { provider: model.provider, model: model.id } : { model: "auto" })}\n`,
  );
  return fallback.applyWebSearchFallback(context(current));
}

/** A stand-in for the bridge's query, recording what it was asked. */
function stub(reply) {
  const calls = [];
  globalThis[SEARCH_KEY] = async (model, prompt, options) => {
    calls.push({ model, prompt });
    for (const query of reply.queries) options.onSearch?.(query);
    return reply;
  };
  return calls;
}

const text = (result) => result.content.map((part) => part.text).join("\n");
async function search(params, current = CLAUDE) {
  const updates = [];
  const result = await webSearch("call-1", params, new AbortController().signal, (update) => updates.push(text(update)), context(current));
  return { result, updates };
}

after(() => {
  globalThis[SEARCH_KEY] = bridgeSearch;
});

test("the patched bridge loads and publishes its web search", () => {
  const source = readFileSync(BRIDGE, "utf-8");
  assert.equal(source.split("CYPHER-RUNTIME-PATCH: web-search").length - 1, 1);
  assert.equal(typeof bridge.default, "function");
  assert.equal(typeof bridgeSearch, "function");
});

const BUN = {
  queries: ["bun latest release", "bun 1.4.3"],
  searches: [
    {
      query: "bun latest release",
      links: [
        { title: "Releases · oven-sh/bun", url: "https://github.com/oven-sh/bun/releases" },
        { title: "Bun", url: "https://bun.com/" },
      ],
    },
    { query: "bun 1.4.3", links: [{ title: "Blog", url: "https://bun.com/blog" }] },
  ],
};

test("a pinned Claude Code model searches through Claude Code", async () => {
  assert.deepEqual(pin(CLAUDE), { provider: "claude-bridge", model: "claude-haiku-4-5" });
  const answer = "Bun 1.4.3 is the latest release ([Releases](https://github.com/oven-sh/bun/releases)), see also [the blog](https://bun.com/blog).\n\n## Sources\n- [Releases](https://github.com/oven-sh/bun/releases)\n- [the blog](https://bun.com/blog)";
  const calls = stub({ ...BUN, text: answer });
  const { result, updates } = await search({ query: "latest Bun release", urls: ["https://bun.com/"] });

  assert.equal(calls.length, 1);
  assert.equal(calls[0].model.provider, "claude-bridge");
  assert.equal(calls[0].model.id, "claude-haiku-4-5");
  assert.equal(calls[0].prompt, "latest Bun release\n\nAlso analyze these URLs:\nhttps://bun.com/");
  assert.ok(updates.some((update) => update.includes(`"bun latest release", "bun 1.4.3"`)), updates.join("\n"));

  // The answer's own Sources section stands; nothing is appended to it.
  assert.equal(text(result), answer);
  // Cited links, in citation order and under the search's own titles.
  assert.deepEqual(result.details.sources, [
    { title: "Releases · oven-sh/bun", url: "https://github.com/oven-sh/bun/releases" },
    { title: "Blog", url: "https://bun.com/blog" },
  ]);
  // The uncited result (a prefix of a cited URL) stays in its search's details.
  assert.deepEqual(result.details.nativeSearchCalls.map((call) => call.urls), [
    ["https://github.com/oven-sh/bun/releases", "https://bun.com/"],
    ["https://bun.com/blog"],
  ]);
  assert.equal(result.details.providerKind, "claude-code");
  assert.equal(result.details.model, "claude-haiku-4-5");
  assert.deepEqual(result.details.searchQueries, ["bun latest release", "bun 1.4.3"]);
  assert.equal(result.details.resultCount, 2);
  assert.equal(result.details.error, undefined);
});

test("an answer without its own Sources section gets one", async () => {
  pin(CLAUDE);
  stub({ ...BUN, text: "Bun 1.4.3 ([Releases](https://github.com/oven-sh/bun/releases))." });
  const { result } = await search({ query: "latest Bun release" });
  assert.equal(
    text(result),
    "Bun 1.4.3 ([Releases](https://github.com/oven-sh/bun/releases)).\n\n## Sources\n1. [Releases · oven-sh/bun](https://github.com/oven-sh/bun/releases)",
  );
});

test("a Claude Code failure is the tool's error", async () => {
  pin(CLAUDE);
  globalThis[SEARCH_KEY] = async () => {
    throw new Error("Claude rate limit (five_hour): out of usage");
  };
  const { result } = await search({ query: "anything" });
  assert.equal(text(result), "Error: Claude rate limit (five_hour): out of usage");
  assert.equal(result.details.error, true);
});

test("only a pinned Claude Code model searches through Claude Code", async () => {
  const calls = stub({ text: "never", queries: [], searches: [] });

  // No fallback: the conversation's own Claude Code model still can't search.
  pin(CLAUDE);
  delete process.env.PI_WEB_SEARCH_CONFIG;
  const own = await search({ query: "anything" });
  assert.equal(own.result.details.error, "unsupported_model");

  // A conversation on another provider keeps its own search.
  pin(CLAUDE, GPT);
  assert.equal(process.env.PI_WEB_SEARCH_CONFIG, undefined);

  // Without the bridge's query the pinned model is unsupported, as before.
  pin(CLAUDE);
  delete globalThis[SEARCH_KEY];
  const unloaded = await search({ query: "anything" });
  assert.equal(unloaded.result.details.error, "unsupported_model");

  assert.equal(calls.length, 0);
});

test("automatic never picks Claude Code", () => {
  const ref = pin(undefined);
  assert.deepEqual(ref, { provider: "openai-codex", model: "gpt-5.5" });
});

test("a real search through Claude Code", { skip: process.env.CYPHER_LIVE_CLAUDE_SEARCH !== "1" }, async () => {
  if (process.env.CYPHER_CLAUDE_EXECUTABLE) {
    writeFileSync(
      join(agentDir, "claude-bridge.json"),
      `${JSON.stringify({ provider: { pathToClaudeCodeExecutable: process.env.CYPHER_CLAUDE_EXECUTABLE } })}\n`,
    );
  }
  globalThis[SEARCH_KEY] = bridgeSearch;
  pin(CLAUDE);
  const { result, updates } = await search({ query: "What is the latest stable release of Node.js?" });
  const output = text(result);
  assert.equal(result.details.error, undefined, output);
  assert.equal(result.details.providerKind, "claude-code");
  assert.ok(result.details.searchQueries.length > 0, output);
  assert.ok(result.details.sources.length > 0, output);
  assert.equal(output.split("## Sources").length - 1, 1, output);
  assert.ok(updates.some((update) => update.startsWith("Searching the web with Claude Code: ")), updates.join("\n"));
  console.log(output);
});

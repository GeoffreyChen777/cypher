// Build-time patch: search through Claude Code when the search model is a
// Claude Code one.
//
// Why this exists
// ---------------
// pi-web-search drives each provider's native search API, and a
// `claude-bridge/*` model has none: it is the Claude Code CLI behind
// pi-claude-bridge. pi-web-search-claude-bridge therefore routed search on a
// Claude model through another provider's model.
//
// The patched bridge (pi-claude-bridge-web-search.mjs) runs Claude Code's own
// WebSearch as a one-off query and publishes it on a Symbol.for registry. When
// the configured search model (PI_WEB_SEARCH_CONFIG, written by
// pi-web-search-claude-bridge) is a `claude-bridge/*` model, web_search now
// hands the request to that query and reports its answer, the links it cites
// as sources and the other results it found, like any native search. Without
// the bridge loaded the model stays unsupported, as before.
//
// Only a configured model is routed: with no config, web_search still uses the
// conversation model and still refuses a Claude Code one, so nothing searches
// on the subscription unless the user picked it.
//
// Lifetime
// --------
// Until pi-web-search can search through Claude Code itself. Pinned to the
// bundled version: a version bump must re-check the anchors
// (web-search.staged.test.mjs drives the patched tool).
//
// Failure policy: hard. A missing anchor means pi-web-search changed shape;
// the packaging script runs under `set -e`.
//
// Usage: node pi-web-search-claude-code.mjs <pi-web-search-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-web-search-claude-code.mjs <pi-web-search-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: claude-code";
const EXPECTED_VERSION = "1.7.0";
requireVersion(packageDir, { name: "pi-web-search", expected: EXPECTED_VERSION, script: "pi-web-search-claude-code.mjs" });

const NAME = "pi-web-search";

const TYPES = [
  {
    label: "ProviderKind",
    from: `export type ProviderKind = "google" | "openai" | "xai" | "anthropic" | "deepseek" | "ollama" | "unsupported";`,
    to: `// ${MARKER}: Claude Code's WebSearch, through pi-claude-bridge.
export type ProviderKind = "google" | "openai" | "xai" | "anthropic" | "deepseek" | "ollama" | "claude-code" | "unsupported";`,
  },
];

const API = [
  {
    label: "imports",
    from: `import type { StreamResult } from "./providers/types.ts";`,
    to: `import type { Source, StreamResult } from "./providers/types.ts";

// ${MARKER}: the patched pi-claude-bridge publishes a one-off
// Claude Code query whose only tool is WebSearch.
type ClaudeCodeWebSearch = (
    model: { id: string },
    prompt: string,
    options: { signal?: AbortSignal; onSearch?: (query: string) => void },
) => Promise<{ text: string; queries: string[]; searches: { query: string; links: Source[] }[] }>;

/** The bridge's search for a \`claude-bridge/*\` model, when the bridge is loaded. */
export function claudeCodeWebSearch(model: Model<Api> | undefined): ClaudeCodeWebSearch | undefined {
    if (model?.provider !== "claude-bridge") return undefined;
    const search = (globalThis as Record<symbol, unknown>)[Symbol.for("claude-bridge:webSearch")];
    return typeof search === "function" ? search as ClaudeCodeWebSearch : undefined;
}

async function callClaudeCodeSearch(
    search: ClaudeCodeWebSearch,
    model: Model<Api>,
    prompt: string,
    onUpdate?: AgentToolUpdateCallback,
    signal?: AbortSignal,
): Promise<StreamResult> {
    const searched: string[] = [];
    const progress = () => onUpdate?.({
        content: [{
            type: "text",
            text: searched.length
                ? \`Searching the web with Claude Code: \${searched.map((query) => \`"\${query}"\`).join(", ")}\`
                : "Searching the web with Claude Code...",
        }],
        details: { streaming: true, searching: true },
    });
    progress();
    const result = await search(model, prompt, {
        signal,
        onSearch: (query) => {
            searched.push(query);
            progress();
        },
    });
    // Sources are the links the answer cites, in the order it cites them, under
    // the search's own titles. The answer lists them itself; what the searches
    // returned besides stays in the details (each call's urls), not the text.
    const titles = new Map(result.searches.flatMap((search) => search.links.map((link) => [link.url, link.title] as const)));
    const sources: Source[] = [];
    for (const [, label, url] of result.text.matchAll(/\\[([^\\]]*)\\]\\((https?:\\/\\/[^)\\s]+)\\)/g)) {
        if (sources.some((source) => source.url === url)) continue;
        sources.push({ title: titles.get(url) || label || url, url });
    }
    return {
        text: result.text,
        sources,
        providerKind: "claude-code",
        nativeSearchUsed: result.searches.length > 0,
        nativeSearchCalls: result.searches.map((search) => ({
            provider: "claude-code",
            status: "completed",
            actionType: "WebSearch",
            queries: [search.query],
            urls: search.links.map((link) => link.url),
        })),
        searchQueries: result.queries,
        searchResults: sources.map((source) => ({
            ...source,
            source: "claude-code.WebSearch",
            type: "web_search_result",
        })),
    };
}`,
  },
  {
    label: "callApiStream dispatch",
    from: `    if (!prompt) {
        throw new Error("No prompt text found in request body");
    }
`,
    to: `    if (!prompt) {
        throw new Error("No prompt text found in request body");
    }

    // ${MARKER}
    const claudeCodeSearch = claudeCodeWebSearch(model);
    if (claudeCodeSearch) {
        return callClaudeCodeSearch(claudeCodeSearch, model, prompt, onUpdate, signal);
    }
`,
  },
];

const UTILS = [
  {
    label: "imports",
    from: `import { getProviderKind } from "./api.ts";`,
    to: `import { claudeCodeWebSearch, getProviderKind } from "./api.ts";`,
  },
  {
    label: "configured model",
    from: `        const model = ctx.modelRegistry.find(config.provider, config.modelId);
        return isSupportedSearchModel(model) ? model : undefined;`,
    to: `        const model = ctx.modelRegistry.find(config.provider, config.modelId);
        // ${MARKER}: a configured Claude Code model searches through it.
        if (claudeCodeWebSearch(model)) return model;
        return isSupportedSearchModel(model) ? model : undefined;`,
  },
];

const results = [
  patchFile(packageDir, "src/providers/types.ts", { name: NAME, marker: MARKER, edits: TYPES }),
  patchFile(packageDir, "src/api.ts", { name: NAME, marker: MARKER, edits: API }),
  patchFile(packageDir, "src/utils.ts", { name: NAME, marker: MARKER, edits: UTILS }),
];
if (!results.some(Boolean)) {
  console.log("pi-web-search patch: Claude Code search already applied");
  process.exit(0);
}
console.log("pi-web-search patch: configured claude-bridge models search through Claude Code (types.ts, api.ts, utils.ts)");

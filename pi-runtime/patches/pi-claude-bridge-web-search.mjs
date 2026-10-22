// Build-time patch: let pi-web-search search through Claude Code's WebSearch.
//
// Why this exists
// ---------------
// The provider path runs Claude Code with `tools: []`, so a Claude model in Pi
// sees only Pi's tools and cannot use Claude Code's own WebSearch, and
// pi-web-search cannot call a `claude-bridge` API either. Search on a Claude
// model therefore went through another provider's model.
//
// The bridge now offers one more one-off Claude Code query beside the compact
// summary: WebSearch is its only tool, nothing from ~/.claude is loaded, the
// session is not persisted, and it runs on the user's Claude subscription. It
// is published on a process-wide Symbol.for registry, so pi-web-search (see
// pi-web-search-claude-code.mjs) finds it however many module instances load.
// The conversation's own Claude Code session is untouched.
//
// Lifetime
// --------
// Until the bridge exposes Claude Code's search itself. Pinned to the bundled
// version: a version bump must re-check the anchor (web-search.staged.test.mjs
// checks the patched bridge still loads and registers the search).
//
// Failure policy: hard. A missing anchor means the bridge changed shape; the
// packaging script runs under `set -e`.
//
// Usage: node pi-claude-bridge-web-search.mjs <pi-claude-bridge-package-dir>

import { packageDirArg, patchFile, requireVersion } from "./lib/apply-anchors.mjs";

const packageDir = packageDirArg("pi-claude-bridge-web-search.mjs <pi-claude-bridge-package-dir>");

const MARKER = "CYPHER-RUNTIME-PATCH: web-search";
const EXPECTED_VERSION = "0.9.2";
requireVersion(packageDir, { name: "pi-claude-bridge", expected: EXPECTED_VERSION, script: "pi-claude-bridge-web-search.mjs" });

const SEARCH = String.raw`// ${MARKER}: Claude Code's own WebSearch as a one-off query, for
// pi-web-search. Only WebSearch is enabled; nothing from ~/.claude is loaded
// and the session is not persisted.
type ClaudeCodeWebSearchLink = { title: string; url: string };
type ClaudeCodeWebSearchResult = {
	text: string;
	/** Every query Claude Code searched for, in order. */
	queries: string[];
	/** Each search that returned, with its result links. */
	searches: { query: string; links: ClaudeCodeWebSearchLink[] }[];
};
type ClaudeCodeWebSearchOptions = {
	signal?: AbortSignal;
	/** Each query Claude Code searches for, as it starts. */
	onSearch?: (query: string) => void;
};

const CLAUDE_CODE_WEB_SEARCH_KEY = Symbol.for("claude-bridge:webSearch");
const WEB_SEARCH_MAX_TURNS = 8;
const WEB_SEARCH_SYSTEM_PROMPT = [
	"You are a web research tool called by another AI agent. Answer its request by searching the web with the WebSearch tool; search again with a different query when the first results do not settle it.",
	"Reply with the findings only, concise and factual, with dates and version numbers where they matter. Say so plainly when the results do not answer the request.",
	"Cite claims inline as markdown links to the pages they came from, and end with a \"## Sources\" section listing each page you cited as a markdown link.",
].join("\n");

async function runClaudeCodeWebSearch(
	model: { id: string },
	prompt: string,
	options: ClaudeCodeWebSearchOptions = {},
): Promise<ClaudeCodeWebSearchResult> {
	const { signal, onSearch } = options;
	if (signal?.aborted) throw new Error("Operation aborted");
	const cwd = process.cwd();
	const claudeExecutable = loadConfig(cwd).provider?.pathToClaudeCodeExecutable;
	const cliModel = claudeCodeModelId(model, longContextSettings);
	debug("web search: spawn model=" + cliModel + " promptLen=" + prompt.length);

	const sdkQuery = query({
		prompt,
		options: {
			cwd,
			env: { ...process.env, ...CC_CHILD_ENV },
			settings: { autoMemoryEnabled: false },
			tools: ["WebSearch"],
			permissionMode: "bypassPermissions",
			strictMcpConfig: true,
			settingSources: [] as SettingSource[],
			skills: [],
			persistSession: false,
			systemPrompt: WEB_SEARCH_SYSTEM_PROMPT,
			model: cliModel,
			maxTurns: WEB_SEARCH_MAX_TURNS,
			...(claudeExecutable ? { pathToClaudeCodeExecutable: claudeExecutable } : {}),
			...makeCliDebugOptions("web-search"),
		},
	});
	let aborted = false;
	const onAbort = () => {
		aborted = true;
		void sdkQuery.interrupt().catch(() => {});
		try { sdkQuery.close(); } catch {}
	};
	signal?.addEventListener("abort", onAbort, { once: true });

	const queries: string[] = [];
	const searches: ClaudeCodeWebSearchResult["searches"] = [];
	let assistantText = "";
	let finalText = "";
	let errorText: string | undefined;
	let rejection: { rateLimitType?: string; resetsAt?: number } | undefined;
	try {
		for await (const message of sdkQuery) {
			if (aborted) break;
			if (message.type === "assistant") {
				for (const block of (message as any).message?.content ?? []) {
					if (block.type === "text" && typeof block.text === "string") assistantText += block.text;
					if (block.type === "tool_use" && block.name === "WebSearch" && typeof block.input?.query === "string") {
						queries.push(block.input.query);
						onSearch?.(block.input.query);
					}
				}
			} else if (message.type === "user") {
				// WebSearch's structured output: its query, and per server search
				// the result links (the rest of results is Claude's own summary).
				const output = (message as any).tool_use_result;
				if (typeof output?.query !== "string" || !Array.isArray(output.results)) continue;
				const links: ClaudeCodeWebSearchLink[] = [];
				for (const result of output.results) {
					for (const link of Array.isArray(result?.content) ? result.content : []) {
						if (typeof link?.url !== "string" || links.some((known) => known.url === link.url)) continue;
						links.push({ title: typeof link.title === "string" && link.title ? link.title : link.url, url: link.url });
					}
				}
				searches.push({ query: output.query, links });
			} else if (message.type === "rate_limit_event") {
				const info = (message as any).rate_limit_info;
				if (info?.status === "rejected") rejection = info;
			} else if (message.type === "result") {
				errorText = resultErrorText(message);
				if (!errorText && message.subtype === "success") finalText = message.result || assistantText;
			}
		}
	} finally {
		signal?.removeEventListener("abort", onAbort);
		try { sdkQuery.close(); } catch {}
	}

	if (aborted) throw new Error("Operation aborted");
	if (errorText) throw new Error(rejection ? describeRateLimitFailure(rejection, errorText) : errorText);
	const text = (finalText || assistantText).trim();
	if (!text) throw new Error("Claude Code web search returned no answer");
	debug("web search: done queries=" + queries.length + " searches=" + searches.length + " textLen=" + text.length);
	return { text, queries, searches };
}

(globalThis as Record<symbol, unknown>)[CLAUDE_CODE_WEB_SEARCH_KEY] = runClaudeCodeWebSearch;

function reinjectPriorCompactionFileOps(`;

const EDITS = [
  {
    label: "runClaudeCodeWebSearch",
    from: "function reinjectPriorCompactionFileOps(",
    to: SEARCH,
  },
];

if (!patchFile(packageDir, "src/index.ts", { name: "pi-claude-bridge", marker: MARKER, edits: EDITS })) {
  console.log("pi-claude-bridge patch: web search already applied");
  process.exit(0);
}
console.log("pi-claude-bridge patch: Claude Code WebSearch for pi-web-search (index.ts)");

// The reviewing model for eval/run.mjs: stubbed (fixed answers) or live (the
// Claude Agent SDK call of ../live.mjs, copied because live.mjs runs on import).
import { mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * What the command acts on, read by the eval's own small parser (not the
 * extension's): the claim an approving stub names, so the code's target check
 * is exercised. Deliberately simple; a wrong claim only makes the code ask.
 */
export function claimTargets(command = "") {
  const out = [];
  const add = (kind, value) => value && !out.some((t) => t.kind === kind && t.value === value) && out.push({ kind, value: String(value) });
  for (const m of command.matchAll(/\b(?:gh pr|glab mr)\s+(?:merge|close|edit|comment|ready|review|reopen)\s+(?:[^\s]*\/pull\/|[^\s]*\/merge_requests\/|[\w.-]+\/[\w.-]+#)?#?(\d+)/g)) add("pr", m[1]);
  for (const m of command.matchAll(/\bgh pr create\b[^|;&]*?--base[= ](\S+)/g)) add("branch", m[1].replace(/["']/g, ""));
  for (const m of command.matchAll(/\bgit\b[^|;&\n]*\bpush\b([^|;&\n]*)/g)) {
    const words = m[1].trim().split(/\s+/).filter((w) => w && !w.startsWith("-") && !/^\d>&?\d?$/.test(w) && !w.includes(">"));
    for (const ref of words.slice(1)) {
      const dest = ref.replace(/^\+/, "").split(":").at(-1).replace(/^refs\/(heads|tags)\//, "");
      if (/^v?\d|release|-v\d/.test(dest)) add("tag", dest);
      else add("branch", dest);
    }
  }
  for (const m of command.matchAll(/\bgit tag\s+(?:-[a-z]\s+)*([^\s-][^\s]*)/g)) add("tag", m[1]);
  for (const m of command.matchAll(/\bgh release create\s+([^\s-][^\s]*)/g)) add("tag", m[1]);
  for (const m of command.matchAll(/\b(?:ssh|scp|rsync)\b[^|;&\n]*?\s([\w.-]+@)?([\w-]+(?:\.[\w-]+)*):?(?=\s|$|:)/g)) if (!/^-/.test(m[2]) && !/^(ssh|scp|rsync)$/.test(m[2])) add("host", m[2]);
  for (const m of command.matchAll(/--repo[= ]([\w.-]+\/[\w.-]+)/g)) add("repo", m[1]);
  for (const m of command.matchAll(/https?:\/\/([\w.-]+)/g)) add("host", m[1]);
  for (const m of command.matchAll(/\b(?:npm|pnpm|yarn|cargo) publish\b/g)) add("package", "this package");
  return out.slice(0, 4);
}

/** The owner word an approving stub quotes: the latest one, in full. */
function latestQuote(item) {
  const words = item.owner?.messages ?? [];
  return words.at(-1)?.text;
}

/**
 * The stubbed reviewing model.
 * - "recorded": the case's `verdict` (recorded, or the adversarial one), else ask-user.
 * - "approve": every case approved, with a quote and target claim (an over-eager model: the code's bar alone).
 * An approval without a quote/target gets the latest owner word and the eval's own claim.
 */
/**
 * With `consent`, an approving stub also names its consent the way the newer judge prompt asks
 * (`consent: {source, quote, assents}`), the strongest fooled model: the source of the owner word
 * holding the quote (or `reply` when only a reply's proposal holds it), and `assents: true`.
 */
function consentFor(item, quote) {
  if (!quote) return undefined;
  const words = item.owner?.messages ?? [];
  const holder = [...words].reverse().find((word) => word.text?.includes(quote)) ?? [...words].reverse().find((word) => word.proposal?.includes(quote));
  const source = holder ? (holder.text?.includes(quote) && holder.source !== "reply" ? holder.source : "reply") : "typed";
  return { source, quote, assents: true };
}

export function stubModel(mode, { consent = false } = {}) {
  return async (_system, _prompt, _signal, item) => {
    let verdict = item.verdict;
    if (mode === "approve" && !item.adversarial) verdict = { decision: "approve", reason: "stub: an over-eager model approves everything" };
    if (!verdict) return '{"decision":"ask-user","reason":"no verdict for this case"}';
    const out = { decision: verdict.decision, reason: verdict.reason ?? "stub" };
    if (verdict.decision !== "ask-user") {
      out.userQuote = verdict.userQuote ?? latestQuote(item);
      const target = verdict.target ?? claimTargets(item.command)[0];
      if (target) out.target = target;
      if (consent) {
        const named = consentFor(item, out.userQuote);
        if (named) out.consent = named;
      }
    }
    return JSON.stringify(out);
  };
}

// What claude-bridge sets for every Claude Code child (cc-child.ts CC_CHILD_ENV), without the parent session's markers (as live.mjs).
const PARENT_SESSION_VARS = ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_PID", "CLAUDE_EFFORT", "TRACEPARENT", "CLAUDE_CODE_INVOKED_SKILLS", "CLAUDE_ENV_FILE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_AGENT_SDK_VERSION", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_SSE_PORT", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN"];

/** The live reviewing model: Claude Code through the Claude Agent SDK, as claude-bridge serves the one-off review. */
export async function liveModel({ sdk = join(homedir(), ".cypher/pi-runtime/current/npm/node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs"), model = "claude-opus-5-5", work, claude, modelFor } = {}) {
  const { query } = await import(sdk);
  const childEnv = { ...process.env };
  for (const name of PARENT_SESSION_VARS) delete childEnv[name];
  Object.assign(childEnv, { ENABLE_CLAUDEAI_MCP_SERVERS: "0", DISABLE_AUTO_COMPACT: "1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1", CLAUDE_CODE_SILENT_TURN_REMINDER: "0" });
  mkdirSync(work, { recursive: true, mode: 0o700 });
  const timings = [];
  const complete = async (system, prompt, signal, item) => {
    const started = Date.now();
    const used = modelFor?.(item) ?? model;
    const run = query({
      prompt,
      options: { cwd: work, env: childEnv, settings: { autoMemoryEnabled: false }, tools: [], strictMcpConfig: true, settingSources: [], skills: [], persistSession: false, systemPrompt: system, model: used, maxTurns: 1, thinking: { type: "disabled" }, ...(claude ? { pathToClaudeCodeExecutable: claude } : {}) },
    });
    const onAbort = () => {
      void run.interrupt?.().catch(() => {});
      try {
        run.close?.();
      } catch {}
    };
    if (signal?.aborted) onAbort();
    else signal?.addEventListener("abort", onAbort, { once: true });
    let assistant = "";
    let final = "";
    let error;
    try {
      for await (const message of run) {
        if (message.type === "assistant") {
          for (const block of message.message?.content ?? []) if (block.type === "text") assistant += block.text;
        } else if (message.type === "result") {
          if (message.subtype === "success" && !message.is_error) final = message.result || assistant;
          else error = message.result || message.subtype;
        }
      }
    } finally {
      signal?.removeEventListener("abort", onAbort);
      timings.push(Date.now() - started);
    }
    if (signal?.aborted) return undefined;
    const text = final || assistant;
    if (error || !text.trim()) throw new Error(`The model stopped with an error: ${error ?? "no text"}`);
    return text;
  };
  return { complete, timings };
}

/**
 * Explicit requests: whether the owner's own words ask for exactly what a
 * dangerous command (publishing, pushing, a pull request) does. Since round 3
 * (2026-11-23) this is evidence, never a gate: `scanConsents` and
 * `scanDataConsent` run the checks on every owner word for the judge's prompt,
 * `checkExplicit` on the consent the judge cites for the audit, and the
 * judge's verdict decides (core.ts). The checks:
 *
 * 1. The quote is the owner's: it occurs, as written (whitespace, case and
 *    full-width punctuation aside), in one of the owner's consents. Only
 *    what the engine passes through `CYPHER_OWNER_WORDS` counts, each
 *    tagged by kind (`OwnerWord.source`): what a person typed in this chat,
 *    a plan's instruction and an approved plan's card from its record, the
 *    owner's answer to an agent's question (with the question; only the
 *    option picked), and a short reply with the agent message it answers.
 *    The model says which consent it relies on; the quote must be in that
 *    one (`consentText`).
 *    Text an agent wrote (a plan's brief, an orchestrator's steering, a
 *    subagent's task, a peer's question) never does, even when it sits in
 *    the conversation as a user message. A bare "continue" or "ok" is too
 *    vague to ask for anything.
 * 2. The target is the command's: read from the line itself, by the tool
 *    table (tools.ts) into entities compared by relation (entities.ts), not
 *    by string (the pull
 *    request `gh pr merge 52` merges, the tag or branch `git push origin x`
 *    publishes, the package `npm publish` publishes, the host `curl` or
 *    `ssh` reaches), it must be what the model named, and what the owner's
 *    message names or plainly implies (a release tag on the default branch
 *    holding the version, for "release a new version"; the pull request
 *    this chat opened, for "open a PR and merge it").
 * 3. Nothing the owner wrote since takes it back or moves on to other work,
 *    and the message itself doesn't say not to ("open a PR, don't merge it").
 *    A later "don't" about something else ("don't forget the changelog")
 *    takes nothing back.
 *
 * A target known only when the line runs (`git push origin $BRANCH`) can't
 * be checked, so only the consent's own words are; a body or title known only
 * when it runs (`--body "$(cat …)"`) can be, since it changes what is said,
 * not where.
 */

import { basename, isAbsolute, join, resolve } from "node:path";

import { homedir } from "node:os";

import { remoteOperand, sshHostIndex } from "./commands.ts";
import { canonical, claimEntity, describeEntity, identifies, namesOf, relation, repoOf } from "./entities.ts";
import type { Entity, EntityContext, EntityKind, PullRequestFact } from "./entities.ts";
import { antecedentsFor, IMPLIED_STEPS } from "./lifecycle.ts";
import type { LedgerEntity } from "./ledger.ts";
import type { GitFacts } from "./risk.ts";
import { UNKNOWN } from "./shell.ts";
import { actionWords, allWords, attributeWords, carriesOnOnly, codeNounWords, listWords, nounWords, uncoveredScript } from "./lexicon.ts";
import type { AttributeKey } from "./lexicon.ts";
import { readTool, remoteEffect } from "./tools.ts";
import { refsWritten } from "./effects.ts";
import { viewOf } from "./packs.ts";
import type { ToolRead } from "./tools.ts";

/** The variable the harness sets when it spawns Pi (`crates/harness/src/pi/spawn.rs`). */
export const OWNER_WORDS_ENV = "CYPHER_OWNER_WORDS";

/** How many of the owner's messages count, newest last. */
const MAX_MESSAGES = 10;
/** How much of one message counts. */
const MAX_MESSAGE_CHARS = 4000;
/** How much of a reply's proposal or an answer's question counts (their latest part). */
const MAX_CONTEXT_CHARS = 2000;

/** Where an owner word comes from: every kind is a consent the engine vouches for. */
export type ConsentSource = "typed" | "plan" | "answer" | "card" | "reply";
const SOURCES: ReadonlySet<string> = new Set<ConsentSource>(["typed", "plan", "answer", "card", "reply"]);

/**
 * One consent in the owner's own words, as the engine passed it
 * (`crates/engine/src/host/doc_host/owner_words.rs`):
 * - `typed`: a message they typed in this chat;
 * - `plan`: the instruction a plan carries out, from its record;
 * - `card`: the card of the approved plan the chat belongs to (title,
 *   summary, steps), from its record;
 * - `answer`: their answer to a question an agent asked (`text`: the
 *   option(s) they picked or what they wrote; `question`: the question as
 *   shown, options last, after "Options: ");
 * - `reply`: a short message right after an agent message (`proposal`: that
 *   message, its latest part). Whether it agrees is the reviewing model's call.
 */
export interface OwnerWord {
  text: string;
  source: ConsentSource;
  question?: string;
  proposal?: string;
  /** Epoch millis, when known. */
  at?: number;
}

/** What the engine knows the owner wrote: the only words that can ask for a danger. */
export interface OwnerWords {
  /** Oldest first. */
  messages: OwnerWord[];
  /** Some of the chat's user messages were written by an agent (a brief, a steer, a subagent's task). */
  agentWritten: boolean;
}

/**
 * The owner's words as the harness passed them; undefined when it passed
 * none (an older Cypher, or no Cypher at all). A word of a kind this
 * extension doesn't know is left out: it can't be checked.
 */
export function parseOwnerWords(raw: string | undefined): OwnerWords | undefined {
  if (!raw) return undefined;
  try {
    const value = JSON.parse(raw) as { messages?: unknown; agentWritten?: unknown };
    if (!Array.isArray(value.messages)) return undefined;
    const messages: OwnerWord[] = [];
    for (const item of value.messages as { text?: unknown; source?: unknown; question?: unknown; proposal?: unknown; at?: unknown }[]) {
      if (typeof item?.text !== "string" || !item.text.trim()) continue;
      const source = item.source === undefined ? "typed" : item.source;
      if (typeof source !== "string" || !SOURCES.has(source)) continue;
      const word: OwnerWord = { text: item.text.slice(0, MAX_MESSAGE_CHARS), source: source as ConsentSource };
      if (typeof item.question === "string" && item.question.trim()) word.question = item.question.slice(-MAX_CONTEXT_CHARS);
      if (typeof item.proposal === "string" && item.proposal.trim()) word.proposal = item.proposal.slice(-MAX_CONTEXT_CHARS);
      if (typeof item.at === "number" && Number.isFinite(item.at)) word.at = item.at;
      // A reply answers a proposal; an answer, a question. Without it, it is what was typed.
      if (word.source === "reply" && !word.proposal) word.source = "typed";
      if (word.source === "answer" && !word.question) continue;
      messages.push(word);
    }
    return { messages: messages.slice(-MAX_MESSAGES), agentWritten: value.agentWritten === true };
  } catch {
    return undefined;
  }
}

/** The options a question offered (after its last "Options: "). */
function optionsOf(question: string): string[] {
  const at = question.lastIndexOf("\nOptions: ");
  return at < 0 ? [] : question.slice(at + "\nOptions: ".length).split(" | ").map((option) => option.trim()).filter(Boolean);
}

/**
 * The text a consent says, for checking a quote and the target against:
 * a reply's proposal with the reply; an answer's question with the choice,
 * the options not picked left out (so a quote of one of them fails).
 */
export function consentText(word: OwnerWord): string {
  if (word.source === "reply") return `${word.proposal ?? ""}\n${ownText(word.text)}`;
  if (word.source === "answer") {
    const question = word.question ?? "";
    const at = question.lastIndexOf("\nOptions: ");
    const asked = at < 0 ? question : question.slice(0, at);
    const picked = new Set(word.text.split(/;\s*/).map(normalize));
    const kept = optionsOf(question).filter((option) => picked.has(normalize(option)));
    return `${asked}\n${kept.join("\n")}\n${word.text}`;
  }
  return ownText(word.text);
}

/**
 * Quoted material in the owner's message (round 5 step 2): a fenced block, a `>` quote, a pasted shell
 * prompt line (`$ git push --force …`, `% …`, `PS> …`), a CI or tool log. What it holds is what someone
 * else wrote or ran, not what the owner asks for.
 */
// A fence runs to its closing fence (or the end of the text: `$(?![\s\S])`, since `m` makes `$` end a line too).
const QUOTED = /```[\s\S]*?(?:```|$(?![\s\S]))|~~~[\s\S]*?(?:~~~|$(?![\s\S]))|^[ \t]*(?:>|\$ |% |# |PS[^>\n]*> |❯ |➜ ).*$/gm;

/** Words with which the owner's own text asks for what it quotes (`run this:`, `do it`), besides the actions' own. */
const RUN_QUOTED = /\b(run|execute|exec|do (?:it|this|that|these)|go ahead|apply|paste|type)\b|执行|运行|跑一下|照做/i;

/**
 * The part of a message that is the owner's own words: quoted material only counts when the words around
 * it ask for it (no question among them, an action word or "run this"); otherwise it is left out, so a
 * pasted log's `git push --force` is no request (rt5-100), while "run this: ```git push```" still is.
 */
/** Words that make a sentence conditional (the languages the lexicon has): "if", "once", "unless" (iteration 2/4). */
export const CONDITION_WORDS = /\b(if|unless|once|in case|provided|falls|wenn|sofern|si|sauf si|à condition|se|a menos que|caso|если|в случае)\b|如果|假如|要是|若是|万一|もし|場合|만약|경우/iu;

/**
 * A condition on what the owner asks, for lifting a Deny (iteration 4): a conditional conjunction that opens a clause
 * ("if the tests pass, …", "…, once CI is green"). Words that are also ordinary ("once" as "one time", French "se") only
 * at a clause's start.
 */
export const LIFT_CONDITION = /(?:^|[\s,;:—–(])(?:if|unless|provided(?: that)?|in case|as soon as|wenn|falls|sofern|sauf si|à condition)\s|(?:^|[,;:.!?—–]\s*)(?:once|si|se|caso|a menos que)\s|如果|假如|要是|若是|万一|もし|만약|если/iu;

/** Verbs that keep what they name ("leave X alone", "keep X", "spare X", 保留, 别动): naming to keep (iteration 4). */
export const KEEP_WORDS = /\b(leave\b[^.;,]*\balone|leave\s+(?:it|them|that|this)\b|keep|spare|preserve|retain|protect|hands off|don'?t touch|do not touch)\b|别动|不要动|保留|留着|放着/iu;

/**
 * The owner's own speech in one word (iteration 4): their text without what they quote — code fences and quoted lines
 * (`ownText`), inline quoted speech of several words, and a forwarded or pasted message (from its header on: "----------
 * Forwarded message", "Begin forwarded message:", a "From:" header line). Someone else's words are never consent.
 */
export function ownSpeech(text: string, { attributedOnly = false }: { attributedOnly?: boolean } = {}): string {
  const unforwarded = text.replace(/(?:^|\n)[ \t>]*(?:-{3,}\s*(?:Forwarded message|Original Message)\s*-{0,}|Begin forwarded message:|From:\s+\S+@\S+)[\s\S]*$/i, "\n");
  const quoted = /"([^"\n]*\s[^"\n]*)"|“([^”\n]*\s[^”\n]*)”|「([^」\n]*)」|«([^»\n]*\s[^»\n]*)»/gu;
  // Only speech someone is said to have said (`you said "…"`, `he wrote: "…"`) when a quoted command may be the owner's
  // own request to run it (a Deny's lift: `ok, run "git push --force origin main"`).
  if (attributedOnly) return ownText(unforwarded).replace(new RegExp(`\\b(?:said|says|say|saying|wrote|writes|asked|told|claimed|claims|replied|meant|quote)\\b[^"“「«\\n]{0,24}(?:${quoted.source})`, "giu"), " ");
  return ownText(unforwarded).replace(quoted, " ");
}

export function ownText(text: string): string {
  const outside = text.replace(QUOTED, " ");
  if (outside === text) return text;
  const asks = !QUESTION.test(outside) && (RUN_QUOTED.test(outside) || ACTIONS.some((action) => actionWords(action).test(outside)));
  return asks ? text : outside;
}

/**
 * Where a quote is in one owner word, as which consent: a reply's proposal
 * (the reply agreeing to it), or else the reply's own words, which are then
 * what the owner typed and checked as such; an answer's question and the
 * option picked; any other word's text.
 */
export function citedIn(word: OwnerWord, wanted: string, quote = wanted): { source: ConsentSource; said: string } | undefined {
  if (word.source === "reply") {
    if (word.proposal && normalize(word.proposal).includes(wanted) && bareAssent(word.text, quote)) return { source: "reply", said: consentText(word) };
    // The reply's own words: typed, unless the part holding the quote is a question (a reply asking about the proposal asks for nothing).
    if (!normalize(word.text).includes(wanted)) return undefined;
    const asking = word.text.split(/(?<=[?？¿؟;՞፧⸮⁇⁈⁉꘏.!。！\n])/u).some((part) => normalize(part).includes(wanted) && QUESTION.test(part));
    return asking ? undefined : { source: "typed", said: word.text };
  }
  const said = consentText(word);
  return normalize(said).includes(wanted) ? { source: word.source, said } : undefined;
}

/** A question mark in any script. */
export const QUESTION = /[?？¿؟;՞፧⸮⁇⁈⁉꘏]/u;
/** Things a reply may name of its own: numbers, `#12`, `v1.2`, `a/b`, `x-y`, `a_b`. */
const OWN_NAME = /[\p{L}\p{N}_]*(?:[#!]\d+|\d[\p{L}\p{N}._/-]*|[\p{L}\p{N}]+[/_-][\p{L}\p{N}._/-]+)/gu;

/**
 * Whether a reply may bind to the proposal it answers: nothing in it could
 * change what it agrees to — no question mark of any script, no negation,
 * wait, preference or deferral, no action verb the quoted proposal doesn't
 * have, no name (a number, a `#12`, a `fix-x`) the quote doesn't hold. This
 * only narrows: whether the reply agrees at all is the reviewing model's
 * call on the text (`assents`), never a list of yes-words. Anything else is
 * the owner's own words, checked as typed.
 */
export function bareAssent(reply: string, quote: string): boolean {
  const text = reply.normalize("NFKC");
  if (QUESTION.test(text)) return false;
  if (listWords("negate").test(text) || listWords("retract").test(text)) return false;
  const rest = text;
  const quoted = normalize(quote);
  for (const name of rest.match(OWN_NAME) ?? []) if (!quoted.includes(normalize(name))) return false;
  for (const action of ACTIONS) if (actionWords(action, true).test(rest) && !actionWords(action).test(quote)) return false;
  // Saying more than an assent's size, it must cite what it agrees to: an action of the quote ("swift-delta 删了",
  // "Release a new version."). Longer words that cite none of it ("show me the logs", "thanks, looks good") may be
  // about anything, whatever the model reads into them.
  if (replySize(text) > BARE_REPLY && !citesAction(text, quote)) return false;
  return true;
}

/** The most a reply may say and bind without citing the proposal's action: two words, or four CJK characters. */
const BARE_REPLY = 4;

/** How much a reply says, its filler words left out: a word of a spaced script counts 2, a CJK character 1. */
export function replySize(text: string): number {
  const rest = text.normalize("NFKC").replace(FILLER, " ");
  const words = rest.match(/[\p{Script=Latin}\p{Script=Cyrillic}\p{Script=Greek}\p{N}_'’-]+/gu) ?? [];
  const cjk = rest.match(/[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/gu) ?? [];
  const other = rest.replace(/[\p{Script=Latin}\p{Script=Cyrillic}\p{Script=Greek}\p{N}_'’\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}\s\p{P}\p{S}-]+/gu, " ").trim();
  return words.length * 2 + cjk.length + (other ? other.split(/\s+/).length * 2 : 0);
}

/** The reply says an action (or the pull-request noun) its quoted proposal says. */
function citesAction(reply: string, quote: string): boolean {
  const asks = (regex: RegExp) => regex.test(reply) && regex.test(quote);
  return ACTIONS.some((action) => asks(actionWords(action))) || asks(nounWords("pr")) || asks(nounWords("tag"));
}

const ACTIONS = ["create", "delete", "merge", "close", "comment", "review", "edit", "ready", "reopen", "push", "publish", "release", "unpublish", "install", "stop", "erase", "apply", "rename", "write", "archive", "reach"];

/** How the dialog names each kind of consent. */
const SOURCE_NAMES: Readonly<Record<ConsentSource, string>> = {
  typed: "messages",
  plan: "plan's instruction",
  card: "approved plan's card",
  answer: "answers to the agent's questions",
  reply: "replies to the agent's proposals",
};

// --- Text ------------------------------------------------------------------------

/** Text as the quote check compares it: compatibility forms (full-width), lower case, no whitespace, punctuation or symbols. */
export function normalize(text: string): string {
  return text.normalize("NFKC").toLowerCase().replace(/[\s\p{P}\p{S}]+/gu, "");
}

/** How much a normalized quote says: a CJK character counts twice. */
function weight(normalized: string): number {
  let total = 0;
  for (const char of normalized) total += /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u.test(char) ? 2 : 1;
  return total;
}

/** The least a quote must say: "pr 给main" passes, "继续" and "ok" don't. */
const MIN_QUOTE_WEIGHT = 6;

/** Words that say to go on without saying what (normalized). */
const VAGUE = new Set(
  [
    "continue", "continue please", "please continue", "go ahead", "go on", "go", "ok", "okay", "yes", "yep", "sure", "proceed", "do it", "carry on",
    "ship it", "lgtm", "fine", "approved", "approve", "sounds good", "looks good", "keep going", "next", "try again", "all set", "push it",
    "push this", "push that", "merge it", "release it", "publish it", "do the release", "go for it", "just do it",
    "继续", "继续吧", "好", "好的", "可以", "可以了", "行", "没问题", "同意", "批准", "开始", "搞吧", "发布吧", "发布", "推送", "推送吧", "推吧",
    "推送先", "先推送", "合并", "合并吧", "发吧", "上吧", "再试试", "再试一次", "好了", "好了继续", "push 这个", "推这个", "推送这个",
    "合并这个", "发布这个",
  ].map(normalize),
);

// The multilingual words (lexicon.ts): what says to wait, not to, or only to go on.
const RETRACTION = listWords("retract");
const RETRACTIONS = listWords("retract", true);
const NEGATION = listWords("negate", true);
const FILLER = listWords("filler", true);
const REMINDER = listWords("remind", true);

/** The most a clause may say besides its "wait" and still be a bare one. */
const BARE_WEIGHT = 4;

/** A message's sentences: a "don't" belongs to the sentence it stands in. */
function sentences(text: string): string[] {
  return text
    .split(/[.!?](?=\s|$)|[;。！？；\n]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}

/** The words that ask for what a target's command does, in any language of the table, for telling what a "don't" is about. */
function verbOf(target: Target): RegExp {
  if (target.kind === "branch") return target.action === "delete" ? SAYS.delete : SAYS.push;
  if (target.kind === "tag") return target.action === "delete" ? SAYS.delete : anyOf(SAYS.release, SAYS.push);
  if (target.kind === "package") return target.action === "install" ? SAYS.install : target.action === "publish" ? SAYS.publish : actionWords(target.action);
  return actionWords(target.action);
}

/** Whether a sentence is about what a target's command does: it names the target, or its action, or its kind. */
function concerns(text: string, target: Target): boolean {
  if (mentions(text, target.value) || verbOf(target).test(text)) return true;
  switch (target.kind) {
    case "pr":
      return SAYS.pr.test(text) || SAYS.merge.test(text);
    case "tag":
      return SAYS.release.test(text) || nounWords("version").test(text);
    case "package":
      return SAYS.publish.test(text) || SAYS.install.test(text) || nounWords("package").test(text);
    default:
      return nounWords(target.kind).test(text);
  }
}

/**
 * Whether a later message takes a request back: a sentence with a "wait",
 * "don't" or "instead" that is about what the command does, or that says
 * nothing else ("wait", "先别", "don't do that yet"). A "don't" about
 * something else ("don't rename any branch", for a merge) takes nothing back.
 */
function retracts(text: string, targets: readonly Target[]): boolean {
  return sentences(text.normalize("NFKC").replace(REMINDER, " ")).some((sentence) => {
    if (!RETRACTION.test(sentence)) return false;
    if (targets.some((target) => concerns(sentence, target))) return true;
    const rest = sentence.replace(RETRACTIONS, " ").replace(FILLER, " ");
    return weight(normalize(rest)) <= BARE_WEIGHT;
  });
}

/** Scripts written without spaces between words. */
const UNSPACED = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;

/** The few words next to a "don't", within its clause: after it, or before it. */
function near(text: string, after: boolean): string {
  if (UNSPACED.test(text)) return after ? text.slice(0, 8) : text.slice(-8);
  const words = text.trim().split(/\s+/);
  return (after ? words.slice(0, 4) : words.slice(-4)).join(" ");
}

/**
 * Whether a message says not to do what a target's command does: a "don't"
 * right next to its verb, in any language of the table. Before the verb
 * ("open a PR, don't merge it", "先别合并"); or after it where the language
 * puts it there ("マージしないで", "병합하지 마"), or where it ends its
 * clause ("merge den PR noch nicht").
 */
function forbids(text: string, target: Target): boolean {
  const verb = verbOf(target);
  const haystack = text.normalize("NFKC").replace(REMINDER, " ");
  const clause = /[,.;!?，。；！？、\n]/;
  for (const match of haystack.matchAll(NEGATION)) {
    const after = haystack.slice(match.index + match[0].length, match.index + match[0].length + 48).split(clause)[0];
    if (verb.test(near(after, true))) return true;
    const before = haystack.slice(Math.max(0, match.index - 48), match.index).split(clause).at(-1) ?? "";
    const ends = weight(normalize(after.replace(FILLER, " "))) <= BARE_WEIGHT;
    if ((UNSPACED.test(match[0]) || ends) && verb.test(near(before, false))) return true;
  }
  return false;
}

/** A later message that says it carries on, anywhere in it ("好了 继续发布"). */
const CONTINUES = /continue|go on|carry on|继续|接着|weiter|continu|sigue|prosegu|продолж|続け|계속/i;

// --- Targets ---------------------------------------------------------------------

/** A target's kind: an entity's (entities.ts), a release reading as the tag it publishes. */
export type TargetKind = Exclude<EntityKind, "release">;

/** What one dangerous command acts on, read from the line. */
export interface Target {
  kind: TargetKind;
  /** The same as an entity, for relations (entities.ts). */
  entity: Entity;
  /** The pull request's number (or branch), the branch, the tag, `name@version`, the host. */
  value: string;
  /** What it does there: `merge`, `create`, `push`, `release`, `publish`, `install`, `reach`, `send`. */
  action: string;
  /** The repository (`owner/name`) a `gh` command names with `-R` or a URL. */
  repo?: string;
  /** The remote a push goes to. */
  remote?: string;
  /** A new pull request's base and head. */
  base?: string;
  head?: string;
  /** The commit a tag marks, when the line says (undefined: known only when it runs). */
  commit?: string;
  /** A package's name and version. */
  name?: string;
  version?: string;
  /** Where the command runs (for git facts). */
  dir?: string;
  /** What a command run on a host changes there (tools.ts `remoteEffect`): its action, and the program that does it. */
  remote_action?: string;
  remote_says?: string;
  /** Whether a name is a branch of the repository at `dir` (for reading which branches a consent names). */
  isBranch?: (name: string) => boolean;
}

/** A line's targets, or why one can't be read before it runs. */
/** What a dangerous line acts on; or why some of it can't be read, with the targets that could (`known`). */
export type LineTargets =
  | { targets: Target[]; attributes?: AttributeKey[] }
  | { unknown: string; known?: Target[]; actions?: string[]; text?: string; attributes?: AttributeKey[]; unread?: UnreadCommand[] };

/** One dangerous command whose target can't be read: its own words, and the action its table row says (if any). */
export interface UnreadCommand {
  text: string;
  actions: string[];
  /** Some of its words are known only when it runs (`$DIR/x`, `xargs …`): what it acts on may come from the rest of the line. */
  open?: boolean;
}

/**
 * Irreversible attributes a command's flags give its action (data): a consent to the action is not a consent
 * to these; the owner's words must name each (lexicon.ts `attributeWords`). `sub`: the subcommand words.
 */
export const ATTRIBUTE_FLAGS: readonly { programs: readonly string[]; sub: readonly string[]; flags: RegExp; attribute: AttributeKey }[] = [
  { programs: ["git"], sub: ["push"], flags: /^(-f|--force|--force-with-lease(=.*)?|--force-if-includes|--mirror|-[A-Za-z]*f[A-Za-z]*|\+.+)$/, attribute: "force" },
  { programs: ["gh"], sub: ["pr", "merge"], flags: /^--admin$/, attribute: "bypass" },
  { programs: ["glab"], sub: ["mr", "merge"], flags: /^(--skip-ci|--when-pipeline-succeeds=false)$/, attribute: "bypass" },
  { programs: ["npm", "pnpm", "yarn"], sub: ["publish"], flags: /^(-f|--force)$/, attribute: "force" },
  { programs: ["npm", "pnpm"], sub: ["unpublish"], flags: /^(-f|--force)$/, attribute: "force" },
  { programs: ["cargo"], sub: ["publish"], flags: /^--allow-dirty$/, attribute: "force" },
  { programs: ["kubectl"], sub: [], flags: /^(--force|--grace-period=0)$/, attribute: "force" },
  { programs: ["helm"], sub: [], flags: /^--force$/, attribute: "force" },
  { programs: ["docker", "podman"], sub: ["push"], flags: /^(-f|--force)$/, attribute: "force" },
];

/** The irreversible attributes one command's words carry (ATTRIBUTE_FLAGS). */
export function attributesOf(words: readonly string[], cwd: string | undefined): AttributeKey[] {
  const program = basename(words[0] ?? "");
  const out = new Set<AttributeKey>();
  for (const row of ATTRIBUTE_FLAGS) {
    if (!row.programs.includes(program)) continue;
    let rest: readonly string[];
    if (program === "git") {
      const parts = gitParts(words, cwd);
      if (parts.sub !== row.sub[0]) continue;
      rest = parts.rest;
    } else {
      const positionals = words.slice(1).filter((word) => !word.startsWith("-"));
      if (!row.sub.every((word, i) => positionals[i] === word)) continue;
      rest = words.slice(1);
    }
    if (rest.some((word) => word !== UNKNOWN && row.flags.test(word))) out.add(row.attribute);
  }
  return [...out];
}

/** What reading targets needs to know about the machine. */
export interface TargetFacts {
  git(dir: string): GitFacts | undefined;
  /** A tag of that name exists in the repository at `dir`. */
  tagExists(dir: string, name: string): boolean;
  /** A file's text, or undefined. */
  readFile(path: string): string | undefined;
}

/** One command of a line, as core.ts reads it. */
export interface TargetCommand {
  words: readonly string[];
  cwd: string | undefined;
  /** The classifier found it dangerous. */
  danger: boolean;
  /** Files its dangerous redirections write (canonical): targets of their own. */
  writes?: readonly string[];
}

const VERSION = /(?<![\w.])v?(\d+\.\d+(?:\.\d+)*)(?![\d])/g;

function versionOf(name: string): string | undefined {
  const match = /(\d+\.\d+(?:\.\d+)*)/.exec(name);
  return match?.[1];
}

/** git's global options, and the directory `-C` moves to. */
function gitParts(words: readonly string[], cwd: string | undefined): { sub?: string; rest: string[]; dir: string | undefined } {
  let dir = cwd;
  let i = 1;
  for (; i < words.length; i++) {
    const word = words[i];
    if (word === "-C") {
      const next = words[++i];
      if (next === undefined || next === UNKNOWN) dir = undefined;
      else dir = isAbsolute(next) ? next : dir === undefined ? undefined : resolve(dir, next);
    } else if (word === "-c" || word === "--git-dir" || word === "--work-tree" || word === "--namespace") i++;
    else if (!word.startsWith("-")) break;
  }
  return { sub: words[i], rest: words.slice(i + 1), dir };
}

/** `owner/name` from a GitHub URL or `-R` value (`host/owner/name` keeps the last two). */
export function repoName(text: string): string | undefined {
  const url = /github\.com[/:]([\w.-]+)\/([\w.-]+?)(?:\.git)?(?:[/#?]|$)/i.exec(text);
  if (url) return `${url[1]}/${url[2]}`.toLowerCase();
  const parts = text.replace(/\.git$/, "").split("/").filter(Boolean);
  return parts.length >= 2 ? `${parts.at(-2)}/${parts.at(-1)}`.toLowerCase() : undefined;
}

function packageOf(dir: string, program: string, facts: TargetFacts): { name: string; version: string } | undefined {
  if (program === "cargo") {
    const text = facts.readFile(join(dir, "Cargo.toml"));
    const section = text?.split(/^\[/m).find((part) => part.startsWith("package]"));
    const name = section && /^name\s*=\s*"([^"]+)"/m.exec(section)?.[1];
    const version = section && /^version\s*=\s*"([^"]+)"/m.exec(section)?.[1];
    return name && version ? { name, version } : undefined;
  }
  try {
    const json = JSON.parse(facts.readFile(join(dir, "package.json")) ?? "") as { name?: unknown; version?: unknown };
    return typeof json.name === "string" && typeof json.version === "string" ? { name: json.name, version: json.version } : undefined;
  } catch {
    return undefined;
  }
}

/** git subcommands that move HEAD: a tag of `HEAD` after one is known only when the line runs. */
const MOVES_HEAD = /^(commit|merge|reset|checkout|switch|pull|rebase|cherry-pick|am|revert|stash)$/;

/**
 * What a line's dangerous commands act on. Every one must be read before the
 * line runs; a line with a danger whose target this can't read gives why.
 */
export function lineTargets(commands: readonly TargetCommand[], facts: TargetFacts): LineTargets {
  const read = readLineTargets(commands, facts);
  const attributes = [...new Set(commands.filter((command) => command.danger).flatMap((command) => attributesOf(command.words, command.cwd)))];
  const extra = attributes.length ? { attributes } : {};
  return read.unknown === undefined ? { targets: read.targets, ...extra } : { unknown: read.unknown, known: read.targets, unread: read.unread, ...extra };
}

/** Every target the line's dangerous commands name, and the first reason one can't be read. */
function readLineTargets(commands: readonly TargetCommand[], facts: TargetFacts): { targets: Target[]; unknown?: string; unread: UnreadCommand[] } {
  const targets: Target[] = [];
  let unknown: string | undefined;
  /** Every dangerous command whose target can't be read, each with its own words: each needs its own consent. */
  const unread: UnreadCommand[] = [];
  let at: TargetCommand | undefined;
  const cannot = (why: { unknown: string }) => {
    unknown ??= why.unknown;
    if (!at || unread.some((item) => item.text === at!.words.join(" "))) return;
    const program = basename(at.words[0] ?? "");
    const parts = program === "git" ? gitParts(at.words, at.cwd) : undefined;
    const tool = parts ? (parts.sub === "push" ? undefined : parts.sub === undefined ? undefined : readTool(["git", parts.sub, ...parts.rest])) : readTool(at.words);
    const actions = parts?.sub === "push" ? ["push"] : tool && !("unknown" in tool) ? [tool.action] : [];
    // Its input may be what it acts on: words known only when it runs, `xargs`, or a shell reading its script from the line.
    const open = at.words.includes(UNKNOWN) || at.words.some((word) => /^(xargs|parallel|sh|bash|zsh|dash|ksh|fish|eval|python3?|node|perl|ruby|osascript)$/.test(basename(word)));
    unread.push({ text: at.words.filter((word) => word !== UNKNOWN).join(" "), actions, ...(open ? { open: true } : {}) });
  };
  /** Tags the line creates, with the commit (undefined: known only when it runs). */
  const created = new Map<string, string | undefined>();
  /** Branches the line creates (`git switch -c x`). */
  const branches = new Set<string>();
  let headMoved = false;
  for (const command of commands) {
    at = command;
    const words = command.words;
    const program = basename(words[0] ?? "");
    for (const path of command.writes ?? []) {
      if (path === UNKNOWN) cannot({ unknown: "it writes a file known only when it runs" });
      else targets.push(targetOf({ kind: "path", name: path }, "write", command.cwd));
    }
    if (program === "git") {
      const { sub, rest, dir } = gitParts(words, command.cwd);
      const made = (sub === "switch" || sub === "checkout") && rest.findIndex((word) => /^-[cCbB]$|^--create$|^--force-create$/.test(word));
      if (typeof made === "number" && made >= 0 && rest[made + 1] && rest[made + 1] !== UNKNOWN) branches.add(rest[made + 1]);
      if (sub === "tag") {
        const positionals: string[] = [];
        let listing = false;
        for (let i = 0; i < rest.length; i++) {
          const word = rest[i];
          if (word === "-m" || word === "-F" || word === "-u" || word === "--message" || word === "--file" || word === "--local-user") i++;
          else if (/^(-d|--delete|-l|--list|-v|--verify|-n\d*|--contains|--points-at|--merged|--no-merged|--sort.*|--format.*)$/.test(word)) listing = true;
          else if (word !== UNKNOWN && word.startsWith("-")) continue;
          else positionals.push(word);
        }
        if (!listing && positionals[0] !== undefined) {
          const commit = positionals[1] ?? "HEAD";
          created.set(positionals[0], commit === UNKNOWN || (headMoved && /^(HEAD|@)/.test(commit)) ? undefined : commit);
        }
        continue;
      }
      if (sub !== undefined && MOVES_HEAD.test(sub)) headMoved = true;
      if (!command.danger) continue;
      // A local branch it deletes, renames or resets, by any spelling (effects.ts `GIT_REF_WRITERS`).
      const rewritten = sub === undefined ? undefined : refsWritten(viewOf(["git", sub, ...rest]));
      if (rewritten) {
        for (const name of rewritten.refs) {
          if (name === UNKNOWN) cannot({ unknown: `it ${rewritten.verb} a branch known only when it runs` });
          else targets.push({ kind: "branch", value: name, action: "delete", dir, entity: { kind: "branch", name } });
        }
        continue;
      }
      if (sub !== "push") {
        // Any other git command by the table (`git worktree remove x`), where `-C` took it.
        const read = sub === undefined ? undefined : readTool(["git", sub, ...rest]);
        if (read === undefined) {
          cannot({ unknown: `Cypher can't tell what \`git ${sub ?? ""}\` acts on` });
          continue;
        }
        if ("unknown" in read) {
          cannot(read);
          continue;
        }
        const found = toolTargets(read, ["git", sub!, ...rest], dir, facts);
        if ("unknown" in found) cannot(found);
        else targets.push(...found.targets);
        continue;
      }
      const read = pushTargets(rest, dir, created, branches, facts);
      if ("unknown" in read) cannot(read);
      else targets.push(...read.targets);
      continue;
    }
    if (!command.danger) continue;
    const read = readTool(words);
    if (read === undefined) {
      cannot({ unknown: `Cypher can't tell from your words what \`${program}\` acts on` });
      continue;
    }
    if ("unknown" in read) {
      cannot(read);
      continue;
    }
    const found = toolTargets(read, words, command.cwd, facts);
    if ("unknown" in found) cannot(found);
    else targets.push(...found.targets);
  }
  for (const target of targets) {
    const dir = target.dir;
    if (dir !== undefined) target.isBranch ??= (name) => facts.git(dir)?.isBranch(name) ?? false;
  }
  return { targets, unread, ...(unknown !== undefined ? { unknown } : {}) };
}

/** A target from an entity the table read, with what the command does to it. */
function targetOf(entity: Entity, action: string, dir: string | undefined): Target {
  const kind: TargetKind = entity.kind === "release" ? "tag" : entity.kind;
  const base = { entity, action, ...(dir !== undefined ? { dir } : {}), ...(entity.repo ? { repo: repoName(entity.repo) ?? entity.repo.toLowerCase() } : {}) };
  switch (entity.kind) {
    case "pr":
      return entity.name
        ? { kind, value: entity.name, ...base }
        : { kind, value: `${entity.head ?? "this branch"} → ${entity.base}`, ...base, base: entity.base, head: entity.head };
    case "package":
      return { kind, value: entity.version ? `${entity.name}@${entity.version}` : entity.name, ...base, name: entity.name.replace(/(?<=.)@[^/]*$/, ""), ...(entity.version ? { version: entity.version } : {}) };
    case "host":
      return { kind, value: entity.name.toLowerCase(), ...base };
    default:
      return { kind, value: entity.name, ...base };
  }
}

/** The targets of one command the table covers: its entities, or what its reader reads. */
function toolTargets(read: ToolRead, words: readonly string[], cwd: string | undefined, facts: TargetFacts): LineTargets {
  const { row, program, action, args } = read;
  switch (row.reader) {
    case "manifest": {
      const where = args.positionals[1] && program !== "cargo" && args.positionals[1] !== UNKNOWN ? args.positionals[1] : undefined;
      const dir = where ? (isAbsolute(where) ? where : cwd && resolve(cwd, where)) : cwd;
      const pkg = dir ? packageOf(dir, program, facts) : undefined;
      if (!pkg) return { unknown: "Cypher can't read which package it publishes" };
      const entity: Entity = { kind: "package", name: pkg.name, version: pkg.version };
      return { targets: [{ kind: "package", value: `${pkg.name}@${pkg.version}`, action, entity, name: pkg.name, version: pkg.version, dir }] };
    }
    case "ssh": {
      const at = sshHostIndex(words);
      const host = at === undefined ? undefined : words[at];
      if (!host || host === UNKNOWN) return { unknown: "the host is known only when it runs" };
      const target = targetOf({ kind: "host", name: host.replace(/^[^@]*@/, "").toLowerCase() }, action, cwd);
      const effect = remoteEffect(words.slice(at! + 1));
      if (effect) Object.assign(target, { remote_action: effect.action, remote_says: effect.says });
      return { targets: [target] };
    }
    case "copy": {
      const hosts = new Set(words.slice(1).map((word) => remoteOperand(word)?.host).filter((host): host is string => host !== undefined));
      if (hosts.size === 0) return { unknown: "Cypher can't tell which host it reaches" };
      return { targets: [...hosts].map((host) => targetOf({ kind: "host", name: host.toLowerCase() }, action, cwd)) };
    }
    case "urls": {
      const hosts = new Set<string>();
      for (const word of words.slice(1)) {
        const url = /^(?:[a-z][\w+.-]*:\/\/)(?:[^@/]*@)?([^/:?#]+)/i.exec(word);
        if (url) hosts.add(url[1].toLowerCase());
      }
      if (hosts.size === 0) return { unknown: "Cypher can't tell which server it reaches" };
      return { targets: [...hosts].map((host) => targetOf({ kind: "host", name: host }, action, cwd)) };
    }
    case "refspecs":
      return { unknown: `Cypher can't tell what \`${program} ${row.sub.join(" ")}\` acts on` };
  }
  const targets: Target[] = [];
  for (const entity of read.entities) {
    if (entity.kind === "pr" && !entity.name) {
      // A new pull request: from the branch checked out into the default one, unless the line says.
      const git = cwd === undefined ? undefined : facts.git(cwd);
      entity.base ??= git?.defaults[0];
      entity.head ??= git?.current;
      if (entity.base === undefined) return { unknown: "the pull request's base is known only when it runs" };
    }
    if (entity.kind === "pr" && entity.name) {
      const url = /\/(?:pull|-\/merge_requests|merge_requests)\/(\d+)/.exec(entity.name);
      if (url) {
        entity.repo ??= repoOf(entity.name) || repoName(entity.name);
        entity.name = url[1];
      } else entity.name = entity.name.replace(/^[#!]/, "");
    }
    if (entity.repo !== undefined) entity.repo = repoName(entity.repo) ?? entity.repo.toLowerCase();
    const target = targetOf(entity, action, cwd);
    if (entity.kind === "release") {
      // The commit a release tags: `--target`, else the tag itself.
      const commit = args.options.get("--target");
      target.commit = commit && commit !== UNKNOWN ? commit : entity.name;
    }
    targets.push(target);
  }
  return { targets };
}

function pushTargets(
  rest: readonly string[],
  dir: string | undefined,
  created: Map<string, string | undefined>,
  branches: ReadonlySet<string>,
  facts: TargetFacts,
): LineTargets {
  const positionals: string[] = [];
  let remote: string | undefined;
  let deletes = false;
  for (let i = 0; i < rest.length; i++) {
    const word = rest[i];
    if (word === UNKNOWN) return { unknown: "it pushes something known only when it runs" };
    if (word === "-o" || word === "--push-option" || word === "--receive-pack" || word === "--exec") {
      i++;
      continue;
    }
    if (word === "-d" || word === "--delete") deletes = true;
    if (word.startsWith("--repo=")) remote = word.slice("--repo=".length);
    else if (word.startsWith("-")) continue;
    else positionals.push(word);
  }
  remote ??= positionals.shift();
  if (remote === undefined) return { unknown: "it pushes to the branch's upstream, which the line doesn't name" };
  if (positionals.length === 0) return { unknown: "it pushes what the remote's settings say, which the line doesn't name" };
  const git = dir === undefined ? undefined : facts.git(dir);
  const targets: Target[] = [];
  // `git push origin --delete x`: the refs it names are the remote's, and
  // whether they still exist here says nothing.
  if (deletes) {
    for (const refspec of positionals) {
      const name = refspec.replace(/^refs\/(heads|tags)\//, "");
      const tagged = refspec.startsWith("refs/tags/") || (dir !== undefined && facts.tagExists(dir, name));
      targets.push({ kind: tagged ? "tag" : "branch", value: name, action: "delete", remote, dir, entity: { kind: tagged ? "tag" : "branch", name, ...(tagged ? {} : { remote }) } });
    }
    return { targets };
  }
  for (const refspec of positionals) {
    const [source, dest = source] = refspec.replace(/^\+/, "").split(":");
    const name = dest.replace(/^refs\/(heads|tags)\//, "");
    const tagged = dest.startsWith("refs/tags/") || source.startsWith("refs/tags/");
    if (tagged || created.has(name)) {
      targets.push({ kind: "tag", value: name, action: "push", remote, commit: created.has(name) ? created.get(name) : name, dir, entity: { kind: "tag", name } });
      continue;
    }
    const branch = name === "HEAD" ? git?.current : name;
    if (branch && ((git && (git.isBranch(branch) || refspec.includes(":"))) || branches.has(branch))) {
      targets.push({ kind: "branch", value: branch, action: "push", remote, dir, entity: { kind: "branch", name: branch, remote } });
      continue;
    }
    if (dir !== undefined && facts.tagExists(dir, name)) {
      targets.push({ kind: "tag", value: name, action: "push", remote, commit: name, dir, entity: { kind: "tag", name } });
      continue;
    }
    return { unknown: `Cypher can't tell whether \`${name}\` is a branch or a tag` };
  }
  return { targets };
}

export function describeTarget(target: Target): string {
  switch (target.kind) {
    case "pr":
      return target.action === "create"
        ? `a new pull request from ${target.head ?? "this branch"} into ${target.base}${target.repo ? ` of ${target.repo}` : ""}`
        : `pull request #${target.value} (${target.action})${target.repo ? ` of ${target.repo}` : ""}`;
    case "branch":
      return `branch ${target.value} on ${target.remote}${target.action === "delete" ? " (deleted)" : ""}`;
    case "tag":
      return `tag ${target.value}${target.commit && target.commit !== target.value ? ` at ${target.commit}` : ""}${target.remote ? ` on ${target.remote}` : ""}${target.action === "release" ? " (a release)" : ""}`;
    case "package":
      return target.action === "install" ? `package ${target.value} (installed for the whole machine)` : `package ${target.value}`;
    case "host":
      return `host ${target.value}`;
    default:
      return `${describeEntity(target.entity)}${target.action ? ` (${target.action})` : ""}`;
  }
}

// --- The check -------------------------------------------------------------------

/** What the reviewing model claimed besides its decision. */
export interface ExplicitClaim {
  quote?: string;
  /** Which kind of consent the quote is from, when the model said. */
  source?: string;
  targets?: { kind: string; value: string }[];
}

/** Facts the check needs beyond the line. */
export interface ExplicitFacts {
  /** The home directory, for `~` in paths (default: this user's). */
  home?: string;
  /** The `owner/name` of the repository `origin` points at, in `dir`. */
  originRepo(dir: string): string | undefined;
  /** A tag's commit is on the remote's default branch, and its files hold `version`. Undefined: can't tell. */
  release(dir: string, commit: string, version: string): { onDefault?: boolean; hasVersion?: boolean };
  /** The pull requests whose head is `branch`, in the repository at `dir` (lifecycle.ts follow-ups). */
  pullRequestsWithHead?(dir: string | undefined, branch: string): readonly PullRequestFact[];
  /** The branch checked out in a worktree. */
  worktreeBranch?(path: string): string | undefined;
  /** The branch checked out in the repository at `dir` (lifecycle.ts `IMPLIED_STEPS`). */
  currentBranch?(dir: string): string | undefined;
  /** This task's own branches and pull requests (core.ts `ownEntities`): what a consent naming no target may mean. */
  own?(): { branches: readonly string[]; prs: readonly PullRequestFact[] };
}

/**
 * The one entity of this task a consent that names an action but no target
 * binds to: the task's own branch, or its own pull request, when exactly one
 * fits (several or none: undefined, and the consent must name it). Default
 * branches are never a task's own.
 */
function ownTarget(kind: "branch" | "pr", message: string, opened: readonly PullRequestFact[], facts: ExplicitFacts): string | undefined {
  const own = facts.own?.() ?? { branches: [], prs: [] };
  const prs = [...own.prs, ...opened];
  let found: string | undefined;
  let names: string[];
  if (kind === "branch") {
    const branches = new Set([...own.branches, ...opened.flatMap((pr) => (pr.head ? [pr.head] : []))].filter((name) => !BRANCHY.test(name)));
    if (branches.size !== 1) return undefined;
    found = [...branches][0];
    names = [found, ...prs.filter((pr) => pr.head === found).map((pr) => pr.number)];
  } else {
    const numbers = new Set(prs.map((pr) => pr.number));
    if (numbers.size !== 1) return undefined;
    found = [...numbers][0];
    names = [found, ...prs.filter((pr) => pr.number === found).flatMap((pr) => (pr.head ? [pr.head] : []))];
  }
  // Words that name something else ("merge other-branch", "0.3.5", "#13") leave it to the names they say.
  if (namesOther(message, names)) return undefined;
  // Merging, a default branch may only be where it goes ("merge into main"), never what is merged ("merge main into it").
  if (kind === "pr" && namedAttributes(message, {}).branches.some((branch) => BRANCHY.test(branch.name) && branch.role !== "base")) return undefined;
  return found;
}

/** The message holds an entity-shaped name (OWN_NAME: `#12`, `x-y`, `a/b`, `v1.2`, a number) that is none of `own`, no default branch and no word of the table. */
function namesOther(message: string, own: readonly string[]): boolean {
  const mine = new Set(own.map((name) => name.toLowerCase()));
  for (const raw of message.normalize("NFKC").match(OWN_NAME) ?? []) {
    const name = raw.replace(/^[#!]/, "").replace(/^origin\//, "").toLowerCase();
    if (mine.has(name) || BRANCHY.test(name) || ordinaryWord(name)) continue;
    return true;
  }
  return false;
}

type Entry = { type?: string; message?: { role?: string; content?: unknown; toolCallId?: unknown; isError?: unknown } };

/** Pull requests this chat opened: a `gh pr create` whose result names exactly one. */
export function openedPullRequests(entries: readonly Entry[]): PullRequestFact[] {
  const creates = new Map<string, { head?: string; base?: string }>();
  const opened: PullRequestFact[] = [];
  for (const entry of entries) {
    const message = entry.type === "message" ? entry.message : undefined;
    if (!message) continue;
    if (message.role === "assistant" && Array.isArray(message.content)) {
      for (const part of message.content as { type?: string; id?: unknown; arguments?: { command?: unknown } }[]) {
        if (part?.type === "toolCall" && typeof part.id === "string" && typeof part.arguments?.command === "string" && /\b(?:gh\s+pr|glab\s+mr)\s+create\b/.test(part.arguments.command)) {
          // The branches, when the line says them (`-H x -B main`, glab's `-s x -b main`).
          const line = part.arguments.command;
          const glab = /\bglab\s+mr\s+create\b/.test(line);
          const head = (glab ? /(?:^|\s)(?:-s|--source-branch)(?:=|\s+)([\w./-]+)/ : /(?:^|\s)(?:-H|--head)(?:=|\s+)([\w./-]+)/).exec(line)?.[1];
          const base = (glab ? /(?:^|\s)(?:-b|--target-branch)(?:=|\s+)([\w./-]+)/ : /(?:^|\s)(?:-B|--base)(?:=|\s+)([\w./-]+)/).exec(line)?.[1];
          creates.set(part.id, { ...(head ? { head } : {}), ...(base ? { base } : {}) });
        }
      }
    } else if (message.role === "toolResult" && typeof message.toolCallId === "string" && creates.has(message.toolCallId) && message.isError !== true) {
      const text = Array.isArray(message.content)
        ? (message.content as { type?: string; text?: unknown }[]).map((part) => (part?.type === "text" && typeof part.text === "string" ? part.text : "")).join("\n")
        : typeof message.content === "string"
          ? message.content
          : "";
      const found = new Map<string, string | undefined>();
      for (const match of text.matchAll(/(?:github|gitlab)\.com\/([\w.-]+\/[\w.-]+)\/(?:pull|-\/merge_requests)\/(\d+)/g)) found.set(match[2], match[1].toLowerCase());
      if (found.size === 1) {
        const [[number, repo]] = [...found];
        opened.push({ number, ...(repo ? { repo } : {}), ...creates.get(message.toolCallId) });
      }
    }
  }
  return opened;
}

function userTexts(entries: readonly Entry[]): string[] {
  const out: string[] = [];
  for (const entry of entries) {
    if (entry.type !== "message" || entry.message?.role !== "user") continue;
    const content = entry.message.content;
    const text = typeof content === "string" ? content : Array.isArray(content) ? (content as { type?: string; text?: unknown }[]).map((part) => (part?.type === "text" && typeof part.text === "string" ? part.text : "")).join("\n") : "";
    if (text.trim()) out.push(text);
  }
  return out;
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** `value` appears in `text` as a name of its own, not inside a longer one. */
export function mentions(text: string, value: string): boolean {
  if (!value) return false;
  const haystack = text.normalize("NFKC");
  return new RegExp(`(?<![A-Za-z0-9_./-])${escapeRegex(value.normalize("NFKC"))}(?![A-Za-z0-9_/-]|\\.[A-Za-z0-9])`, "i").test(haystack);
}

/**
 * Whether the owner's words name `value` as a ref (a branch or tag): some mention of it that no code noun stands
 * next to ("update the `hotfix` function" names a function, not the branch hotfix), unless a ref noun does too.
 */
export function refMention(text: string, value: string): boolean {
  if (!value || !mentions(text, value)) return false;
  const haystack = text.normalize("NFKC");
  const code = codeNounWords();
  const ref = anyOf(nounWords("branch"), nounWords("tag"), nounWords("version"));
  // A ref role right before the name: the push verb whose object it is, or a base/head marker ("push x", "into x", "from x").
  const role = anyOf(SAYS.push, listWords("baseBefore"), listWords("headBefore"), ref);
  const codeBefore = new RegExp(`(?:${code.source})[\\s\\p{P}\\p{S}]*$`, "iu");
  const codeAfter = new RegExp(`^[\\s\\p{P}\\p{S}]*(?:${code.source})`, "iu");
  const refBefore = new RegExp(`(?:${role.source})[\\s\\p{P}\\p{S}]*$`, "iu");
  const refAfter = new RegExp(`^[\\s\\p{P}\\p{S}]*(?:${ref.source})`, "iu");
  const pattern = new RegExp(`(?<![A-Za-z0-9_./-])${escapeRegex(value.normalize("NFKC"))}(?![A-Za-z0-9_/-]|\\.[A-Za-z0-9])`, "giu");
  for (const match of haystack.matchAll(pattern)) {
    const before = haystack.slice(Math.max(0, match.index - 24), match.index).replace(/(?:\b(?:the|a|an|der|die|das|le|la|el|il)\s*)$/iu, "");
    const after = haystack.slice(match.index + match[0].length, match.index + match[0].length + 24);
    // `hotfix()` or `cart.hotfix` is code by its own spelling.
    const spelledCode = /^\(/.test(after) || /\.$/.test(before);
    const nextToCode = spelledCode || codeBefore.test(before) || codeAfter.test(after);
    if (!nextToCode || refBefore.test(before) || refAfter.test(after)) return true;
  }
  return false;
}

/** Pull request numbers a message names: `#52`, `PR 52`, `pull/52`. */
function namedPullRequests(text: string): Set<string> {
  const out = new Set<string>();
  const normalized = text.normalize("NFKC");
  for (const match of normalized.matchAll(/(?:#|(?<![\w])!|\bpr\s*#?|\bmr\s*[#!]?|pull\s*request\s*#?|merge\s*request\s*[#!]?|\/pull\/|\/merge_requests\/|合并请求\s*#?)\s*(\d+)\b/gi)) out.add(match[1]);
  for (const match of normalized.matchAll(/\b(\d+)\s*号?\s*(?:pr|pull request)\b/gi)) out.add(match[1]);
  return out;
}

function namedVersions(text: string): Set<string> {
  return new Set([...text.normalize("NFKC").matchAll(VERSION)].map((match) => match[1]));
}

/** The table's words (lexicon.ts) for what the checks below ask a consent to say. */
const SAYS = {
  get pr() { return nounWords("pr"); },
  get merge() { return actionWords("merge"); },
  get push() { return actionWords("push"); },
  get release() { return anyOf(actionWords("release"), nounWords("tag")); },
  get version() { return anyOf(nounWords("version"), actionWords("release")); },
  get publish() { return actionWords("publish"); },
  get install() { return actionWords("install"); },
  get delete() { return actionWords("delete"); },
};

/** One regex matching what any of these does. */
function anyOf(...regexes: RegExp[]): RegExp {
  return new RegExp(regexes.map((regex) => `(?:${regex.source})`).join("|"), "iu");
}

/** The message names the entity, or one that holds it (entities.ts `namesOf`). */
function namesEntity(message: string, entity: Entity, context: EntityContext): boolean {
  return namesOf(entity, context).some((name) => mentions(message, name));
}

/**
 * Why `message` (the owner's request) doesn't cover `target`; undefined
 * when it does: it asks for the action and names the target (or plainly
 * implies it), and nothing it names of the target's attributes' kinds
 * (base, head, repository, commit) is something else.
 */
function uncovered(target: Target, message: string, opened: readonly PullRequestFact[], facts: ExplicitFacts, context: EntityContext): string | undefined {
  const why = uncoveredByKind(target, message, opened, facts, context);
  if (why) return why;
  const originRaw = target.dir ? facts.originRepo(target.dir) : undefined;
  const origin = originRaw ? repoOf(originRaw) || undefined : context.currentRepo;
  return attributeClash(target, message, { ...context, ...(origin ? { currentRepo: origin } : {}) }, facts);
}

function uncoveredByKind(target: Target, message: string, opened: readonly PullRequestFact[], facts: ExplicitFacts, context: EntityContext): string | undefined {
  const originRaw = target.dir ? facts.originRepo(target.dir) : undefined;
  const origin = originRaw ? repoOf(originRaw) || undefined : (context.currentRepo ?? undefined);
  if (target.repo && origin && repoOf(target.repo) !== origin && !mentions(message, target.repo) && !mentions(message, target.repo.split("/")[1] ?? "")) {
    return `it reaches ${target.repo}, a repository your message doesn't name`;
  }

  if (target.remote && target.remote !== "origin" && !mentions(message, target.remote)) return `it pushes to ${target.remote}, a remote your message doesn't name`;
  switch (target.kind) {
    case "pr": {
      if (target.action === "create") {
        if (!SAYS.pr.test(message)) return "your message doesn't ask for a pull request";
        if (target.base && !mentions(message, target.base) && !["main", "master"].includes(target.base)) return `it targets ${target.base}, a branch your message doesn't name`;
        return undefined;
      }
      if (!actionWords(target.action).test(message)) return `your message doesn't ask to ${target.action} a pull request`;
      const named = namedPullRequests(message);
      if (named.size) return named.has(target.value) ? undefined : `your message names pull request ${[...named].map((n) => `#${n}`).join(", ")}, not #${target.value}`;
      // Merging a pull request merges its head branch: "merge fix-x into main" names the pull request from fix-x.
      const pr = canonical(target.entity, context);
      if (target.action === "merge" && pr.head && namesEntity(message, { kind: "branch", name: pr.head, ...(pr.repo ? { repo: pr.repo } : {}) }, context) && (!pr.base || ["main", "master"].includes(pr.base) || mentions(message, pr.base))) return undefined;
      // No pull request named: the task's own one, when it has exactly one.
      if (ownTarget("pr", message, opened, facts) === target.value && (!target.repo || !origin || repoOf(target.repo) === origin)) return undefined;
      if (!SAYS.pr.test(message)) return `your message doesn't say which pull request (#${target.value})`;
      const ours = opened.filter((pr) => pr.number === target.value && (!pr.repo || !origin || repoOf(pr.repo) === origin) && (!target.repo || !pr.repo || repoOf(pr.repo) === repoOf(target.repo)));
      return ours.length ? undefined : `your message doesn't name pull request #${target.value}, and this chat didn't open it`;
    }
    case "branch":
      if (target.action === "delete") {
        if (!SAYS.delete.test(message)) return "your message doesn't ask to delete a branch";
        if (refMention(message, target.value)) return undefined;
        return ownTarget("branch", message, opened, facts) === target.value ? undefined : `it deletes ${target.value}, a branch your message doesn't name`;
      }
      // The task's own branch (exactly one): "push" or a pull request asked for (its head must be pushed) names it.
      const own = ownTarget("branch", message, opened, facts) === target.value;
      if (!SAYS.push.test(message) && !(own && SAYS.pr.test(message))) return "your message doesn't ask to push";
      return refMention(message, target.value) || own ? undefined : `it pushes to ${target.value}, a branch your message doesn't name${mentions(message, target.value) ? " (it names a piece of code of that name)" : ""}`;
    case "tag": {
      if (!SAYS.release.test(message) && !SAYS.push.test(message)) return "your message doesn't ask for a release or a tag";
      if (refMention(message, target.value)) return undefined;
      const version = versionOf(target.value);
      const named = namedVersions(message);
      if (named.size) return version && named.has(version) ? undefined : `your message names version ${[...named].join(", ")}, not ${target.value}`;
      if (!SAYS.release.test(message) || !SAYS.version.test(message)) return `your message doesn't name ${target.value} or ask for a new version`;
      if (!version) return `${target.value} carries no version to match a release against`;
      if (!target.dir || !target.commit) return `the commit ${target.value} marks is known only when it runs`;
      const release = facts.release(target.dir, target.commit, version);
      if (release.onDefault !== true) return `${target.value} doesn't mark a commit on the default branch`;
      if (release.hasVersion !== true) return `the commit ${target.value} marks doesn't hold version ${version}`;
      return undefined;
    }
    case "package": {
      if (target.action === "install") {
        if (!SAYS.install.test(message)) return "your message doesn't ask to install anything";
        return target.name && mentions(message, target.name) ? undefined : `it installs ${target.value}, which your message doesn't name`;
      }
      if (!SAYS.publish.test(message)) return "your message doesn't ask to publish";
      if (target.name && mentions(message, target.name)) return undefined;
      if (target.version && namedVersions(message).has(target.version)) return undefined;
      return `your message names neither ${target.name} nor version ${target.version}`;
    }
    case "host":
      // By its name, or one that holds it (`web` for `web.example.com`, `gamma` for `gamma-cluster`).
      if (!namesEntity(message, target.entity, context)) return `it reaches ${target.value}, a host your message doesn't name`;
      // A command run there that changes something (tools.ts `REMOTE_EFFECTS`): naming the host is no consent to it.
      if (target.remote_action && !actionWords(target.remote_action).test(message)) return `it runs a command on ${target.value} that would ${target.remote_action} (${target.remote_says ?? ""}), which your message doesn't ask for`;
      return undefined;
    default: {
      if (!actionWords(target.action).test(message)) return `your message doesn't ask to ${target.action} ${describeEntity(target.entity)}`;
      const here = target.dir ? { ...context, cwd: target.dir } : context;
      return namesEntity(message, target.entity, here) ? undefined : `it acts on ${describeEntity(target.entity)}, which your message doesn't name`;
    }
  }
}

const IN_REPO_KINDS: ReadonlySet<string> = new Set(["pr", "branch", "tag", "release"]);

// --- Attributes a consent names ---------------------------------------------------

/** Articles between a marker and the name it marks ("into the main branch"). */
const ARTICLES = new Set(["the", "a", "an", "der", "die", "das", "den", "dem", "le", "la", "les", "l", "el", "los", "las", "il", "lo", "gli", "o", "os", "as", "het", "een"]);
/** Remote names, never branches by themselves. */
const REMOTE_NAMES = /^(origin|upstream|fork)$/i;

/** A word of the language table (lexicon.ts), not a name: "PR", "remote", "merge". */
function ordinaryWord(word: string): boolean {
  return LEXICON_WORD.test(word);
}
const LEXICON_WORD = new RegExp(`^(?:${allWords().source})$`, "iu");

/**
 * A name a role marker marks ("into develop", "from feat-a") that reads as a
 * ref, not prose: ref-shaped (no spaces, no capitals), no word of the table
 * and no part that is one ("chart-side"), and either a `/`, `_` or digit, a
 * conventional branch name, or a `-` that ends its phrase (not an
 * adjective before a noun: "read-only mode").
 */
function markedRef(name: string, after: string): boolean {
  if (!/^[a-z0-9][a-z0-9._/-]*$/.test(name) || ordinaryWord(name) || REMOTE_NAMES.test(name)) return false;
  // A count or a measure ("up to 2x", "to 30s", "to 4k"), not a ref.
  if (QUANTITY.test(name)) return false;
  if (/[/_\d]/.test(name) || BRANCHY.test(name)) return true;
  if (!name.includes("-") || name.split("-").some((part) => part.length >= 3 && ordinaryWord(part))) return false;
  const next = after.trim().split(/\s+/)[0] ?? "";
  const ends = next === "" || /^[,.;:!?，。；！？、)]/.test(after.trimStart()) || listWords("baseBefore").test(next) || listWords("headBefore").test(next) || ordinaryWord(next.replace(/[^\p{L}\p{N}-]+$/u, ""));
  return ends;
}

/** A number with a short unit: `2x`, `30s`, `10ms`, `4k`, `3rd`. */
const QUANTITY = /^\d+(?:\.\d+)?[a-z]{0,3}$/i;

/** Branch names that are branches whatever the repository has. */
const BRANCHY = /^(main|master|develop|development|dev|trunk|staging)$/i;
const VERSIONISH = /^v?\d+(?:\.\d+)*$/;
const COMMITISH = /^(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}$/i;

export interface NamedAttributes {
  /** Branches, with the role a marker gives them (`into main`: base; `from fix`: head). */
  branches: { name: string; role?: "base" | "head" }[];
  repos: string[];
  commits: string[];
}

/**
 * The branches, repositories and commits a consent names, read with the
 * table's role markers (lexicon.ts: `into`/`给`/`に`/`에` before or after a
 * branch, `from`/`从`/`から`/`에서`): a name counts as a branch when the
 * repository has it, a branch noun stands next to it, or a role marker
 * marks it and it has a `/`; ordinary words never (an adjective like
 * "read-only" is no branch). A repository is a URL's, an `owner/name#N`'s, or an
 * `owner/name` of the current repository's owner or next to a repository noun.
 */
export function namedAttributes(text: string, context: EntityContext, isBranch: (name: string) => boolean = () => false, options: { explicitRepo?: boolean } = {}): NamedAttributes {
  const out: NamedAttributes = { branches: [], repos: [], commits: [] };
  const haystack = text.normalize("NFKC");
  // Branches this chat's pull requests name.
  const chatRefs = new Set((context.prs ?? []).flatMap((pr) => [pr.head, pr.base]).filter((name): name is string => !!name));
  // Code formatting: `…` spans.
  const spans = [...haystack.matchAll(/`[^`\n]+`/g)].map((m) => [m.index, m.index + m[0].length] as const);
  const inCode = (at: number) => spans.some(([start, end]) => at > start && at < end);
  const owner = context.currentRepo?.split("/")[0];
  for (const match of haystack.matchAll(/(?:https?:\/\/)?(?:www\.)?(?:github\.com|gitlab\.com)[/:]([\w.-]+\/[\w.-]+?)(?:\.git)?(?=[/#?\s]|$)/gi)) out.repos.push(repoOf(match[1]));
  for (const match of haystack.matchAll(/(?<![\w./-])([\w.-]+\/[\w.-]+)[#!]\d+/g)) out.repos.push(repoOf(match[1]));
  const clause = /[,.;!?，。；！？、\n](?:\s|$)|[，。；！？、\n]/;
  for (const match of haystack.matchAll(/(?<![\w./:#@-])([A-Za-z0-9][\w./-]*[\w])(?![\w/:@])/g)) {
    const name = match[1];
    const at = match.index;
    if (/[/:]/.test(haystack[at - 1] ?? "") || haystack.slice(Math.max(0, at - 12), at).match(/(?:github|gitlab)\.com[/:]?[\w.-]*\/?$/i)) continue;
    if (COMMITISH.test(name)) {
      out.commits.push(name.toLowerCase());
      continue;
    }
    if (VERSIONISH.test(name) || /^\d+$/.test(name)) continue;
    const before = haystack.slice(Math.max(0, at - 40), at).split(clause).at(-1) ?? "";
    const after = haystack.slice(at + name.length, at + name.length + 40).split(clause)[0] ?? "";
    const words = before.trim().split(/\s+/).filter(Boolean);
    const prior = words.filter((word) => !ARTICLES.has(word.toLowerCase().replace(/['’]$/, "")));
    const lastWord = prior.at(-1) ?? "";
    const rawLast = words.at(-1) ?? "";
    const nextWord = after.trim().split(/\s+/)[0] ?? "";
    const repoNoun = nounWords("repo");
    const repoBase = context.currentRepo?.split("/")[1];
    const role = roleOf(before, after, rawLast) ?? roleOf(before, after, lastWord);
    const coded = inCode(at);
    const ref = name.replace(/^origin\//, "");
    // An entity, not a word: a branch the repository or this chat has, or one in code formatting; a role-marked `x/y`.
    const exists = ((isBranch(name) || isBranch(ref)) && !ordinaryWord(ref) && !REMOTE_NAMES.test(ref)) || chatRefs.has(ref);
    // With a repository on the line, any `x/y` that is no branch may be one: a name of another must conflict (safe side).
    if (/^[\w.-]+\/[\w.-]+$/.test(name) && !exists && (role === undefined || options.explicitRepo === true)) {
      const repoish = coded || options.explicitRepo === true || (owner && name.split("/")[0].toLowerCase() === owner) || (repoBase && name.split("/")[1].toLowerCase() === repoBase) || repoNoun.test(lastWord) || repoNoun.test(nextWord) || repoNoun.test(before.slice(-6));
      if (repoish) {
        out.repos.push(repoOf(name));
        continue;
      }
    }
    if (exists || coded || (role !== undefined && markedRef(ref, after))) out.branches.push({ name: ref, ...(role ? { role } : {}) });
  }
  return out;
}

/** The role a marker right before (`into`) or right after (`に`) a name gives it. */
function roleOf(before: string, after: string, lastWord: string): "base" | "head" | undefined {
  const at = (regex: RegExp, text: string) => regex.test(text);
  const tail = before.slice(-6);
  const head = after.slice(0, 6);
  const whole = (key: "baseBefore" | "headBefore") => new RegExp(`^(?:${listWords(key).source})$`, "iu");
  // A word that marks a head in one language and a base in another ("do": Polish "into", Portuguese "from the") marks neither.
  const isHead = lastWord !== "" && at(whole("headBefore"), lastWord);
  const isBase = lastWord !== "" && at(whole("baseBefore"), lastWord);
  if (isHead !== isBase) return isHead ? "head" : "base";
  // Scripts without spaces: the marker touches the name (`从fix`, `给main`, `mainに`, `main에`).
  const touching = (key: "baseBefore" | "headBefore" | "baseAfter" | "headAfter", text: string, end: boolean) =>
    UNSPACED.test(text) && new RegExp(end ? `(?:${listWords(key).source})\\s*$` : `^\\s*(?:${listWords(key).source})`, "iu").test(text);
  if (touching("headAfter", head, false)) return "head";
  if (touching("baseAfter", head, false)) return "base";
  if (touching("headBefore", tail, true)) return "head";
  if (touching("baseBefore", tail, true)) return "base";
  return undefined;
}

/**
 * Why a target's attributes conflict with what the consent names: a
 * branch the consent marks as the base (or head) that isn't the pull
 * request's; branches it names, none of them the target's own, when it
 * doesn't name the target itself; a repository it names that isn't the
 * target's; a commit it names that isn't the one a tag marks.
 */
function attributeClash(target: Target, message: string, context: EntityContext, facts?: ExplicitFacts): string | undefined {
  const named = namedAttributes(message, context, target.isBranch, { explicitRepo: target.repo !== undefined });
  const entity = canonical(target.entity, context);
  // Repositories: the current one named in any form never conflicts with itself; another named against another acted on does.
  const repo = target.repo ? repoOf(target.repo) : entity.repo ?? context.currentRepo;
  const others = named.repos.filter((name) => !(context.currentRepo && repoOf(name) === context.currentRepo && (repo === undefined || repo === context.currentRepo)));
  if (others.length && repo !== undefined && IN_REPO_KINDS.has(entity.kind)) {
    const related = others.some((name) => ["same", "contains", "contained"].includes(relation({ kind: "repo", name }, { kind: "repo", name: repo }, context)));
    if (!related) return `your words name repository ${others.join(", ")}, but it acts on ${repo}`;
  }
  // Commits a tag marks.
  if (entity.kind === "tag" || entity.kind === "release") {
    const commit = target.commit?.toLowerCase();
    if (named.commits.length && commit && COMMITISH.test(commit) && !named.commits.some((sha) => sha.startsWith(commit) || commit.startsWith(sha))) {
      return `your words name commit ${named.commits.join(", ")}, but ${target.value} marks ${commit}`;
    }
    // A branch the consent names ("from main"): the tag's commit must be on it, by git's facts; unknown asks.
    for (const branch of named.branches) {
      const onIt = target.dir && target.commit && /^(main|master)$/i.test(branch.name) ? facts?.release(target.dir, target.commit, versionOf(target.value) ?? "").onDefault : undefined;
      if (onIt !== true) return `your words name ${branch.name}, and Cypher can't confirm the commit ${target.value} marks is on it`;
    }
    return undefined;
  }
  // Branches.
  const own: { head?: string; base?: string; self?: string } =
    entity.kind === "pr" ? { head: entity.head, base: entity.base } : entity.kind === "branch" ? { self: entity.name } : {};
  if (entity.kind !== "pr" && entity.kind !== "branch") return undefined;
  const same = (a: string | undefined, b: string) => a !== undefined && a.replace(/^origin\//, "").toLowerCase() === b.toLowerCase();
  // Asked for a pull request, a branch marked as where it goes ("pr 给main") is the pull request's base, not the branch a push sends.
  const prBase = entity.kind === "branch" && SAYS.pr.test(message);
  for (const branch of named.branches) {
    if (!branch.role || (prBase && branch.role === "base")) continue;
    const want = entity.kind === "pr" ? (branch.role === "base" ? own.base : own.head) : own.self;
    // An attribute not known is checked by the target's own identity (its number), not here.
    if (want !== undefined && !same(want, branch.name)) {
      const what = entity.kind === "pr" ? `the pull request's ${branch.role} is ${want}` : `it pushes ${own.self}`;
      return `your words name ${branch.name} as the ${branch.role === "base" ? "branch it goes into" : "branch it comes from"}, but ${what}`;
    }
  }
  const plain = named.branches.filter((branch) => !branch.role);
  const identified = entity.kind === "pr" ? entity.name !== "" && namedPullRequests(message).has(entity.name) : plain.some((branch) => same(own.self, branch.name));
  if (plain.length && !identified && !plain.some((branch) => [own.head, own.base, own.self].some((mine) => same(mine, branch.name)))) {
    return `your words name branch ${plain.map((branch) => branch.name).join(", ")}, not ${[own.head, own.base, own.self].filter(Boolean).join(" or ") || "the one it acts on"}`;
  }
  return undefined;
}

/** The model's target names one the command really acts on: the same entity, one holding it or held by it (entities.ts). */
function sameTarget(claim: { kind: string; value: string }, target: Target, origin: string | undefined, context: EntityContext): boolean {
  const kind = claim.kind.trim().toLowerCase();
  const value = claim.value.trim().toLowerCase();
  if (!value) return false;
  // A repository: the one the command reaches, or the one it runs in.
  if ((kind === "repo" || kind === "repository") && [target.repo, origin].some((repo) => repo !== undefined && repoOf(value) === repoOf(repo))) return true;
  // A package by its version alone.
  if (target.kind === "package" && /^(package|crate|npm)$/.test(kind) && [target.value, target.name, target.version].some((name) => name !== undefined && value === name.toLowerCase())) return true;
  const entity = claimEntity(claim);
  if (entity === undefined) return false;
  // A claim that leaves the repository unsaid names the thing in the command's: the consent check compares repositories.
  if (IN_REPO_KINDS.has(entity.kind) && entity.repo === undefined && !/\/pull\/|#\d|!\d/.test(claim.value) && target.entity.repo) entity.repo = target.entity.repo;
  if (identifies(entity, target.entity, context)) return true;
  // "the PR main" or "the PR fix-x": a pull request by one of its branches.
  return entity.kind === "pr" && !entity.name && entity.base === undefined && entity.head !== undefined && identifies({ kind: "branch", name: entity.head }, target.entity, context);
}

export interface ExplicitResult {
  ok: boolean;
  /** Why it isn't, for the dialog and the audit. */
  reason?: string;
  quote?: string;
  /** Where the quote was: which consent (`typed`, `plan`, `card`, `answer`, `reply`). */
  quoteSource?: ConsentSource;
  targets: string[];
  /** Which checks passed: `quote` (the owner's words), `target` (the command's and the owner's), `current` (not taken back since). */
  passed: ("quote" | "target" | "current")[];
  /** Targets the consent covers as a standard follow-up of what it asks for (lifecycle.ts), with why. */
  followUps?: string[];
  /**
   * The line's target (or part of it) can't be read: what passed is the
   * consent alone (its kind, quote, not taken back since) and the targets
   * that could be read. It may run once, never remembered.
   */
  once?: boolean;
  /** Why the target can't be read. */
  unknown?: string;
}

/**
 * Whether the owner's own words ask for this line: the model's quote is
 * theirs, names what the line acts on, and nothing they wrote since takes
 * it back.
 */
export function checkExplicit(claim: ExplicitClaim, read: LineTargets, owner: OwnerWords | undefined, entries: readonly Entry[], facts: ExplicitFacts, options: { wordsOnly?: boolean } = {}): ExplicitResult {
  // A line whose target can't be read (all or part of it): only the consent can be checked (see below), and the targets that could be read.
  const unknown = "unknown" in read ? read.unknown : undefined;
  const targets = "targets" in read ? read.targets : (read.known ?? []);
  const result = (passed: ExplicitResult["passed"], reason?: string, extra: Partial<ExplicitResult> = {}): ExplicitResult => ({
    ok: reason === undefined,
    ...(reason ? { reason } : {}),
    ...(claim.quote ? { quote: claim.quote } : {}),
    targets: targets.map(describeTarget),
    passed,
    ...extra,
  });
  if (!owner) return result([], "Cypher didn't pass your own words to this check (an older Cypher, or a run Cypher didn't start)");
  const quote = claim.quote?.trim();
  if (!quote) return result([], "the reviewing model quoted none of your words");
  const wanted = normalize(quote);
  if (VAGUE.has(wanted)) return result([], `"${quote}" doesn't say what to do`);
  if (weight(wanted) < MIN_QUOTE_WEIGHT) return result([], `"${quote}" is too short to ask for this`);
  // The consent the model relies on: the latest owner word of its kind holding the quote.
  // `reply-unconfirmed`: the model cited a reply without saying it assents (judge.ts).
  const assentClaimed = claim.source === "reply";
  const claimedSource = claim.source === "reply-unconfirmed" ? "reply" : claim.source;
  const kind = claimedSource && SOURCES.has(claimedSource) ? (claimedSource as ConsentSource) : undefined;
  // Without a target to check, the consent must be cited exactly: its kind too.
  if (unknown !== undefined && kind === undefined) return result([], `${unknown}, and the reviewing model cited no consent of yours by its kind`);
  // A reply consents to its proposal: a quote found only in the reply's own words is those words, checked as typed.
  let at = -1;
  let found: { source: ConsentSource; said: string } | undefined;
  owner.messages.forEach((message, index) => {
    const cited = citedIn(message, wanted, quote);
    // A model citing a reply whose quote is the reply's own words: those are typed words, checked as such.
    if (cited && (kind === undefined || cited.source === kind || (kind === "reply" && message.source === "reply"))) {
      at = index;
      found = cited;
    }
  });
  if (at < 0 || !found) {
    const unagreed = owner.messages.findLast((message) => message.source === "reply" && message.proposal && normalize(message.proposal).includes(wanted));
    if (unagreed) return result([], `"${quote}" is the agent's proposal, and your reply ("${unagreed.text.trim().slice(0, 80)}") doesn't plainly agree to it`);
    const other = kind !== undefined && owner.messages.some((message) => citedIn(message, wanted, quote) !== undefined);
    if (other) return result([], `"${quote}" isn't in your ${SOURCE_NAMES[kind]}`);
    const elsewhere = userTexts(entries).some((text) => normalize(text).includes(wanted));
    return result([], elsewhere ? `"${quote}" is in the conversation but not in your own words (an agent's brief or steering, or a translation)` : `"${quote}" isn't in your messages`);
  }
  const message = owner.messages[at];
  const said = found.said;
  if (found.source === "reply" && !assentClaimed) return result([], `the reviewing model didn't say your reply ("${message.text.trim().slice(0, 80)}") agrees to the agent's proposal`, { quoteSource: "reply" });
  const source = { quoteSource: found.source };
  // A "don't" can only be read in a language the table has: the owner's words, and everything they wrote since.
  for (const text of [message.text, ...(found.source === "typed" ? [] : [said]), ...owner.messages.slice(at + 1).map((later) => later.text)]) {
    const script = uncoveredScript(text);
    if (script) return result(["quote"], `Cypher can't read whether your words in ${script} script say not to or to wait (no word table for that language), so it asks`, source);
  }
  // The model's target must be one the line acts on.
  const origin = targets.find((target) => target.dir)?.dir;
  const originRaw = origin ? facts.originRepo(origin) : undefined;
  // The checkout's repository, as entities compare it (case-insensitive `owner/name`, from any URL form).
  const originRepo = originRaw ? repoOf(originRaw) || undefined : undefined;
  const opened = openedPullRequests(entries);
  const context: EntityContext = { ...(originRepo ? { currentRepo: originRepo } : {}), prs: opened, ...(origin ? { cwd: origin } : {}), home: facts.home ?? homedir() };
  const claimed = claim.targets ?? [];
  // A target that can't be read can't be compared with the model's: then only the known ones are checked, below.
  if (unknown === undefined) {
    if (claimed.length === 0) return result(["quote"], "the reviewing model named no target", source);
    const strange = claimed.find((item) => !targets.some((target) => sameTarget(item, target, originRepo, context)));
    if (strange) return result(["quote"], `the reviewing model named ${strange.kind} ${strange.value}, but the command acts on ${targets.map(describeTarget).join(", ")}`, source);
  }
  // What a "wait" is about can't be told against a target nobody can read: any counts.
  const takesBack = (text: string) => (unknown !== undefined ? retractsAny(text) : retracts(text, targets));
  // A reply or an answer that itself says to wait consents to nothing, whatever it answers.
  if ((message.source === "reply" || message.source === "answer") && takesBack(message.text)) {
    return result(["quote"], `your answer was "${message.text.trim().slice(0, 140)}"`, source);
  }
  // An irreversible attribute of the action (force, a protection bypass: ATTRIBUTE_FLAGS) must itself be named:
  // a consent to push or merge is no consent to force it or bypass the branch's protection.
  for (const attribute of read.attributes ?? []) {
    if (!attributeWords(attribute).test(said)) return result(["quote"], `the command ${ATTRIBUTE_SAYS[attribute]}, which your words don't ask for`, source);
  }
  // Every target must be what the owner's consent asks for, or a standard follow-up of it (lifecycle.ts).
  const followUps: string[] = [];
  for (const target of targets) {
    const why = uncovered(target, said, opened, facts, context);
    const follow = why ? (consentedFollowUp(target, said, opened, facts, context) ?? impliedStep(target, targets, said, opened, facts, context)) : undefined;
    if (why && !follow) return result(["quote"], why, source);
    if (follow) followUps.push(follow);
    // "open a PR, don't merge it" asks for no merge, though it says the word.
    if (forbids(said, target)) return result(["quote"], `your message says not to (${describeTarget(target)})`, source);
  }
  // Nothing since takes it back or moves on.
  for (const later of owner.messages.slice(at + 1)) {
    const text = later.text.trim();
    const short = text.length > 140 ? `${text.slice(0, 140)}…` : text;
    if (takesBack(text)) return result(["quote", "target"], `you wrote later: "${short}"`, source);
    // A reply or an answer about the same thing carries on with it.
    const about = later.source === "reply" ? later.proposal : later.source === "answer" ? later.question : undefined;
    const carriesOn =
      (text.length <= 120 && (carriesOnOnly(text) || CONTINUES.test(text))) ||
      targets.some((target) => mentions(text, target.value) || (about !== undefined && concerns(about, target)));
    if (!carriesOn) return result(["quote", "target"], `you've written since ("${short}"), so that request may be about earlier work`, source);
  }
  // The target can't be read: the checked consent lets it run once, never remembered, when its own words name
  // what the model says the command acts on and ask for that kind of action (lexicon.ts); a bare "yes" never does.
  if (unknown !== undefined) {
    // The code's own scan (`scanConsents`, `wordsOnly`): no model claims a target, so what can be checked is that the
    // words stand and ask for the line's kind of action.
    const actions = "actions" in read && read.actions?.length ? read.actions : ["delete", "stop", "push", "publish", "release", "merge", "install", "apply", "write", "erase", "reach"];
    if (options.wordsOnly) {
      if (!actions.some((action) => actionWords(action).test(quote))) return result(["quote", "current"], `${unknown}, and your words "${quote}" don't ask to ${actions.join(" or ")}`, source);
      return result(["quote", "current"], undefined, { ...source, once: true, unknown, ...(followUps.length ? { followUps } : {}) });
    }
    if (claimed.length === 0) return result(["quote", "current"], `${unknown}, and the reviewing model named no target`, source);
    const unnamed = claimed.find((item) => {
      const entity = claimEntity(item);
      return entity === undefined || !(namesEntity(quote, entity, context) || mentions(quote, item.value));
    });
    if (unnamed) return result(["quote", "current"], `${unknown}, and your words "${quote}" don't name ${unnamed.kind} ${unnamed.value}`, source);
    // The claimed target must be in the command's own text: a name the line never says is the model's guess.
    const text = "text" in read ? read.text : undefined;
    const token = (value: string) => value !== "" && new RegExp(`(?<![A-Za-z0-9_.-])${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![A-Za-z0-9_-])`, "i").test(text ?? "");
    const absent = claimed.find((item) => text === undefined || !token(item.value.trim()));
    if (absent) return result(["quote", "current"], `${unknown}, and the command never names ${absent.kind} ${absent.value}`, source);
    if (!actions.some((action) => actionWords(action).test(quote))) return result(["quote", "current"], `${unknown}, and your words "${quote}" don't ask to ${actions.join(" or ")}`, source);
    // Every unreadable command needs its own: a claimed target in its own words, and its own action asked for
    // (`gh release create v2 && npm publish`: the release's consent brings none for the publish).
    for (const command of "unread" in read ? (read.unread ?? []) : []) {
      const own = (value: string) => new RegExp(`(?<![A-Za-z0-9_.-])${value.trim().replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![A-Za-z0-9_-])`, "i").test(command.text);
      // A claimed target's names: itself, or a new pull request's head and base ("x → main"), whose head the push publishes.
      const names = (value: string) => [value, ...value.split(/\s*(?:→|->)\s*|\s+of\s+/)].map((name) => name.trim()).filter((name) => name !== "");
      if (!command.open && !claimed.some((item) => names(item.value).some(own))) return result(["quote", "current"], `${unknown}, and nothing your words name is what \`${command.text.slice(0, 60)}\` acts on`, source);
      if (command.actions.length && !command.actions.some((action) => actionWords(action).test(quote))) return result(["quote", "current"], `${unknown}, and your words "${quote}" don't ask to ${command.actions.join(" or ")} (\`${command.text.slice(0, 60)}\`)`, source);
    }
    return result(["quote", "current"], undefined, { ...source, once: true, unknown, ...(followUps.length ? { followUps } : {}) });
  }
  return result(["quote", "target", "current"], undefined, { ...source, ...(followUps.length ? { followUps } : {}) });
}

/**
 * The names a consent to read the installed app's data may say for `path`: the data root's own name
 * (`.cypher` → "cypher", the app) and every segment below it, with a file's stem (`engine.sqlite`,
 * `engine`). Taken from the path, never from a word list; a glob segment names nothing.
 */
export function dataNames(paths: readonly string[]): string[] {
  const names = new Set<string>();
  for (const path of paths) {
    const segments = path.split("/").filter(Boolean);
    const root = segments.findIndex((segment) => /^\.cypher/.test(segment));
    if (root < 0) continue;
    for (const segment of segments.slice(root)) {
      if (/[*?[\]{}]/.test(segment)) continue;
      const name = segment.replace(/^\./, "");
      for (const candidate of [name, name.split(".")[0]]) if (candidate.length >= 3) names.add(candidate);
    }
  }
  return [...names];
}

/**
 * Whether the owner's words ask for reading the installed app's data (core.ts): the model's consent cited
 * by its kind, its quote found in the latest owner word of that kind (a reply only when the model said it
 * assents), the quote naming that data or the app (`dataNames`), no "don't" in the quote's sentence,
 * and nothing written since taking it back or moving on. It runs once, never remembered.
 */
export function checkDataConsent(claim: ExplicitClaim | undefined, owner: OwnerWords | undefined, paths: readonly string[]): { ok: true; quote: string } | { ok: false; reason: string } {
  if (!owner) return { ok: false, reason: "Cypher didn't pass your own words to this check" };
  const quote = claim?.quote?.trim();
  if (!quote) return { ok: false, reason: "the reviewing model cited none of your words asking for it" };
  const wanted = normalize(quote);
  if (VAGUE.has(wanted) || weight(wanted) < MIN_QUOTE_WEIGHT) return { ok: false, reason: `the words it cited ("${quote}") are too short to ask for it` };
  const claimedSource = claim?.source === "reply-unconfirmed" ? "reply" : claim?.source;
  const kind = claimedSource && SOURCES.has(claimedSource) ? (claimedSource as ConsentSource) : undefined;
  if (kind === undefined) return { ok: false, reason: "the reviewing model cited no consent of yours by its kind" };
  let at = -1;
  let found: { source: ConsentSource; said: string } | undefined;
  owner.messages.forEach((message, index) => {
    const cited = citedIn(message, wanted, quote);
    if (cited && (cited.source === kind || (kind === "reply" && message.source === "reply"))) {
      at = index;
      found = cited;
    }
  });
  if (at < 0 || !found) return { ok: false, reason: `"${quote}" is not in your ${SOURCE_NAMES[kind]}` };
  const message = owner.messages[at];
  if (found.source === "reply" && claim?.source !== "reply") return { ok: false, reason: `the reviewing model didn't say your reply ("${message.text.trim().slice(0, 80)}") agrees` };
  for (const text of [message.text, ...owner.messages.slice(at + 1).map((later) => later.text)]) {
    const script = uncoveredScript(text);
    if (script) return { ok: false, reason: `Cypher can't read whether your words in ${script} script say not to (no word table for that language)` };
  }
  const names = dataNames(paths);
  const named = names.filter((name) => mentions(quote, name));
  if (named.length === 0) return { ok: false, reason: `your words "${quote}" don't name the app or the data it reads (${names.join(", ") || "none readable"})` };
  // A "don't" in the sentence the quote stands in, or a reply or answer that itself says to wait.
  const own = sentences(found.said.normalize("NFKC").replace(REMINDER, " ")).filter((sentence) => wanted.includes(normalize(sentence)) || normalize(sentence).includes(wanted));
  if (own.some((sentence) => RETRACTION.test(sentence)) || ((message.source === "reply" || message.source === "answer") && retractsAny(message.text))) {
    return { ok: false, reason: `your words say not to or to wait ("${message.text.trim().slice(0, 140)}")` };
  }
  for (const later of owner.messages.slice(at + 1)) {
    const text = later.text.trim();
    const short = text.length > 140 ? `${text.slice(0, 140)}…` : text;
    if (retractsAny(text)) return { ok: false, reason: `you wrote later: "${short}"` };
    const carriesOn = (text.length <= 120 && (carriesOnOnly(text) || CONTINUES.test(text))) || names.some((name) => mentions(text, name));
    if (!carriesOn) return { ok: false, reason: `you've written since ("${short}"), so that request may be about earlier work` };
  }
  return { ok: true, quote };
}

/** What each irreversible attribute does, for the reason. */
const ATTRIBUTE_SAYS: Readonly<Record<AttributeKey, string>> = {
  force: "forces it (overwrites what is there)",
  bypass: "bypasses a protection (branch protection, required checks or review)",
};

/** What an attribute does, in words (for the judge's evidence). */
export function describeAttribute(attribute: AttributeKey): string {
  return ATTRIBUTE_SAYS[attribute];
}

/** Whether a message says to wait or not do something, whatever about. */
function retractsAny(text: string): boolean {
  return sentences(text.normalize("NFKC").replace(REMINDER, " ")).some((sentence) => RETRACTION.test(sentence));
}

/**
 * A target the consent doesn't name, that is a standard follow-up of an
 * action it does ask for (lifecycle.ts): "merge #52" covers deleting #52's
 * head branch, here and on the remote, and the worktree it is checked out
 * in. The antecedent is checked against the consent exactly as a target
 * would be. Returns why it is covered, or undefined.
 */
function consentedFollowUp(target: Target, said: string, opened: readonly PullRequestFact[], facts: ExplicitFacts, context: EntityContext): string | undefined {
  const known = new Map<string, PullRequestFact>();
  const prsWithHead = (branch: string): LedgerEntity[] => {
    for (const pr of [...opened.filter((item) => item.head === branch), ...(facts.pullRequestsWithHead?.(target.dir, branch) ?? [])]) known.set(pr.number, pr);
    return [...known.values()].filter((pr) => pr.head === branch).map((pr) => ({ kind: "pr", name: pr.number, head: pr.head, ...(pr.repo ? { repo: pr.repo } : {}) }));
  };
  const acted: LedgerEntity | undefined =
    target.kind === "branch" ? { kind: "branch", name: target.value, ...(target.remote ? { remote: target.remote } : {}) } : target.entity.kind === "worktree" ? { kind: "worktree", name: target.entity.name } : undefined;
  if (!acted) return undefined;
  // Another remote or repository than the consent's stays the owner's to name.
  if (target.remote && target.remote !== "origin" && !mentions(said, target.remote)) return undefined;
  const originRaw = target.dir ? facts.originRepo(target.dir) : undefined;
  const origin = originRaw ? repoOf(originRaw) : undefined;
  if (target.repo && origin && repoOf(target.repo) !== origin && !mentions(said, target.repo)) return undefined;
  for (const { rule, antecedent } of antecedentsFor({ action: target.action, entity: acted }, { prsWithHead, worktreeBranch: facts.worktreeBranch })) {
    const pr = known.get(antecedent.entity.name);
    const entity: Entity = { kind: "pr", name: antecedent.entity.name, ...(pr?.head ? { head: pr.head } : {}), ...(pr?.base ? { base: pr.base } : {}), ...(pr?.repo ? { repo: pr.repo } : {}) };
    const before: Target = { kind: "pr", value: entity.name, action: antecedent.action, entity, ...(pr?.repo ? { repo: pr.repo } : {}), ...(target.dir ? { dir: target.dir } : {}) };
    if (uncovered(before, said, opened, facts, context) === undefined && !forbids(said, before)) {
      return `${describeTarget(target)}: ${rule.why} (${describeTarget(before)})`;
    }
  }
  return undefined;
}

/**
 * A target the consent doesn't name that is the ordinary means of another target of the same line the
 * consent does cover (lifecycle.ts `IMPLIED_STEPS`): "tag v1.2.0 and push the tag" covers pushing the
 * branch checked out there, which the tag marks, in the same line. Returns why, or undefined.
 */
function impliedStep(target: Target, line: readonly Target[], said: string, opened: readonly PullRequestFact[], facts: ExplicitFacts, context: EntityContext): string | undefined {
  for (const row of IMPLIED_STEPS) {
    if (!row.then.kinds.includes(target.kind) || !row.then.actions.includes(target.action)) continue;
    // Another remote than origin, or another repository, stays the owner's to name.
    if (target.remote && target.remote !== "origin" && !mentions(said, target.remote)) continue;
    if (row.checkedOut && target.kind === "branch") {
      const current = target.dir ? facts.currentBranch?.(target.dir) : undefined;
      if (current === undefined || target.value !== current) continue;
    }
    for (const before of line) {
      if (before === target || !row.after.kinds.includes(before.kind) || !row.after.actions.includes(before.action)) continue;
      if ((before.remote ?? "origin") !== (target.remote ?? "origin") || (before.repo && target.repo && before.repo !== target.repo)) continue;
      if (uncovered(before, said, opened, facts, context) === undefined && !forbids(said, before) && !forbids(said, target)) return `${describeTarget(target)}: ${row.why} (${describeTarget(before)})`;
    }
  }
  return undefined;
}

// --- Evidence for the reviewing model (round 3) ------------------------------------------------------

/**
 * What the code finds in one owner word for a dangerous line (`scanConsents`): the same checks a cited
 * consent gets (`checkExplicit`: the words ask for the action, name or imply every target, carry the
 * attributes it adds, and nothing written since takes them back), run on the word itself instead of a
 * model's quote. Evidence for the judge, never a gate.
 */
export interface ConsentFinding {
  /** 1-based, as the judge's prompt numbers the owner's words. */
  index: number;
  source: ConsentSource;
  /** The words checked (the word, or the sentence of it that passed). */
  quote: string;
  /** Every check passed: the words ask for this action on every target the line names, and still stand. */
  ok: boolean;
  /** Which checks passed: `quote` (the words say something), `target` (they name what the line acts on), `current` (not taken back since). */
  passed: ExplicitResult["passed"];
  /** Why not, as `checkExplicit` says it. */
  reason?: string;
  /** The line's target (or part of it) can't be read: only the consent itself and the readable targets could be checked. */
  unread?: boolean;
  followUps?: string[];
}

/** The candidates a word is checked as: its whole text, then each sentence (a "don't" elsewhere in it stays its own). */
function quotesOf(text: string): string[] {
  const whole = text.trim();
  const parts = sentences(whole).filter((sentence) => sentence !== whole);
  return [whole, ...parts].filter((quote) => quote !== "");
}

/** The checked words a word offers: a reply its proposal (as the reply's assent) and its own text (as typed); any other word its text. */
function candidatesOf(word: OwnerWord): { source: ConsentSource; quote: string }[] {
  if (word.source === "reply") return [...quotesOf(word.proposal ?? "").map((quote) => ({ source: "reply" as const, quote })), ...quotesOf(word.text).map((quote) => ({ source: "typed" as const, quote }))];
  return quotesOf(word.text).map((quote) => ({ source: word.source, quote }));
}

/** How far a result got: passed checks, an ok counting most. */
function progress(result: ExplicitResult): number {
  return (result.ok ? 10 : 0) + result.passed.length;
}

/**
 * Every owner word checked as a consent for the line (latest last), with the line's own targets as the
 * claim. A reply is checked as assenting to its proposal (whether it does is the judge's call, and the
 * finding says it is a reply).
 */
export function scanConsents(read: LineTargets, owner: OwnerWords | undefined, entries: readonly Entry[], facts: ExplicitFacts): ConsentFinding[] {
  if (!owner) return [];
  const targets = "targets" in read ? read.targets : (read.known ?? []);
  const claimed = targets.map((target) => ({ kind: target.kind, value: target.value }));
  const unread = "unknown" in read;
  const findings: ConsentFinding[] = [];
  owner.messages.forEach((word, at) => {
    let best: { result: ExplicitResult; quote: string; source: ConsentSource } | undefined;
    for (const candidate of candidatesOf(word)) {
      // Only this word: a later identical word would otherwise be the one found.
      const only: OwnerWords = { ...owner, messages: owner.messages.map((message, index) => (index === at ? message : index < at ? { ...message, text: "", proposal: "", question: "" } : message)) };
      const result = checkExplicit({ quote: candidate.quote, source: candidate.source, targets: claimed }, read, only, entries, facts, { wordsOnly: unread });
      if (!best || progress(result) > progress(best.result)) best = { result, quote: candidate.quote, source: candidate.source };
      if (result.ok) break;
    }
    if (!best) return;
    const { result } = best;
    findings.push({
      index: at + 1,
      source: best.source === "typed" && word.source === "reply" ? "typed" : word.source,
      quote: best.quote.length > 240 ? `${best.quote.slice(0, 240)}…` : best.quote,
      ok: result.ok,
      passed: result.passed,
      ...(result.reason ? { reason: result.reason } : {}),
      ...(unread ? { unread: true } : {}),
      ...(result.followUps?.length ? { followUps: result.followUps } : {}),
    });
  });
  return findings;
}

/** What the code finds about a read of the installed app's data (`scanDataConsent`): an owner word asking for it, or why none does. */
export interface DataFinding {
  ok: boolean;
  index?: number;
  source?: ConsentSource;
  quote?: string;
  reason?: string;
  /** The names a consent may say for the data (`dataNames`). */
  names: string[];
}

/**
 * The latest owner word that asks for reading the installed app's data, checked as `checkDataConsent`
 * checks a cited one (its words name the app or the data, no "don't" in that sentence, not taken back or
 * moved on from since). Evidence for the judge, never a gate.
 */
export function scanDataConsent(owner: OwnerWords | undefined, paths: readonly string[]): DataFinding {
  const names = dataNames(paths);
  if (!owner) return { ok: false, reason: "Cypher didn't pass the owner's words", names };
  let closest: DataFinding | undefined;
  for (let at = owner.messages.length - 1; at >= 0; at--) {
    const word = owner.messages[at];
    for (const candidate of candidatesOf(word)) {
      const only: OwnerWords = { ...owner, messages: owner.messages.map((message, index) => (index < at ? { ...message, text: "", proposal: "", question: "" } : message)) };
      const result = checkDataConsent({ quote: candidate.quote, source: candidate.source }, only, paths);
      if (result.ok) return { ok: true, index: at + 1, source: word.source, quote: candidate.quote.length > 240 ? `${candidate.quote.slice(0, 240)}…` : candidate.quote, names };
      closest ??= { ok: false, index: at + 1, source: word.source, reason: result.reason, names };
    }
  }
  return closest ?? { ok: false, reason: "the owner has written nothing", names };
}

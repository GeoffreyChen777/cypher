/**
 * Cypher Pi translation extension.
 *
 * Translation is deliberately implemented as an extension rather than in the
 * harness bridge: it therefore applies to every Pi entry point (TUI, RPC and
 * SDK sessions) while keeping the main model's tool/thinking transcript intact.
 *
 * Each message is translated against a short reference block of earlier turns.
 * A word with several readings — 接口 as interface or endpoint, commit as git or
 * promise — is settled by what was already being discussed, and quoting both
 * languages of a turn keeps the second mention of a term rendered the way the
 * first one was. The block is text only: user messages and the model's final
 * answers, never tool calls, tool output or thinking.
 *
 * An answer's translation is STREAMED, and that is a correctness property
 * before it is a cosmetic one. Translating the final answer is the one step
 * that runs AFTER the visible answer has finished streaming, so for as long as
 * it takes, the session's event stream is otherwise completely silent — and
 * Cypher parks a turn whose stream falls silent (after only 20s, once that turn
 * has been parked and resumed once) and DROPS everything that arrives after the
 * park. A translation published in one piece at the end therefore loses that
 * race whenever it is slower than the window, and is thrown away whole with no
 * sign of it. Publishing frames as the translation arrives keeps the stream
 * audible, which is what makes the feature reliable; rendering progressively is
 * the part you can see.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { AgentMessage, AssistantMessage } from "@earendil-works/pi-agent-core";
import type { Api, Model, TextContent } from "@earendil-works/pi-ai";
import type {
  ExtensionAPI,
  ExtensionContext,
  InputEvent,
  MessageEndEvent,
  MessageEndEventResult,
} from "@earendil-works/pi-coding-agent";

type OutputMode = "replace" | "append";

interface TranslationSettings {
  sourceLanguage: string;
  targetLanguage: string;
  translationModel: string;
  enabledModels: string[];
  outputMode: OutputMode;
  translateUserMessages: boolean;
  translateFinalResponses: boolean;
}

const DEFAULTS: TranslationSettings = {
  sourceLanguage: "auto",
  targetLanguage: "English",
  translationModel: "",
  enabledModels: [],
  outputMode: "append",
  translateUserMessages: true,
  translateFinalResponses: true,
};

const SETTINGS_FILE = "translation.json";
const MAX_TRANSLATION_CHARS = 24_000;

/** Status key of the side-band translation channel Cypher's harness consumes. */
const TRANSLATION_STATUS_KEY = "cypher.translation.v1";
/** A translated prompt, reported so Cypher can keep it beside the prompt the
 *  transcript shows as typed (`INPUT_TRANSLATION_STATUS_KEY` in
 *  `crates/harness/src/pi/mod.rs`; `{version:1, source, text}`). */
const INPUT_TRANSLATION_STATUS_KEY = "cypher.translation.input.v1";
/** Minimum gap between two published frames. Cypher commits the chat doc on a
 *  120ms tick, so a faster cadence buys no visible smoothness and costs one
 *  whole-text snapshot per frame. */
const FRAME_MS = 150;
/** A frame is re-published at least this often even when the translation model
 *  has produced nothing new, so the turn's stream is never quiet for long
 *  enough to be parked. Sized well inside the 20s window a parked-and-resumed
 *  turn gets, which is the tightest one Cypher applies. */
const KEEPALIVE_MS = 4_000;
/** The whole translation, including connect and time-to-first-token. The
 *  keepalive above is deliberately good at holding a turn open, so something
 *  has to be willing to give up: without this, a provider that accepts the
 *  request and then never answers would keep the turn alive forever. */
const TRANSLATION_TIMEOUT_MS = 120_000;
/** The offline detector is a local unix-socket round trip, so this is generous.
 *  It exists because Pi holds its `message_end` event until this handler
 *  returns: a hung engine socket would otherwise stall the turn with no way
 *  out. Timing out yields no local decision, which sends the message to the
 *  translation model — the safe direction. */
const DETECT_TIMEOUT_MS = 2_000;

/** How many earlier exchanges the reference block may quote. Word sense
 *  saturates almost immediately — one or two turns fix the domain, and
 *  everything after that is paid for on every message for nothing. */
const CONTEXT_EXCHANGES = 2;
/** Per quoted turn. The referent of an ambiguous word lives in the opening
 *  sentences, not in the tail of a long answer. */
const CONTEXT_CHARS_PER_TURN = 250;
/** Sized so a single exchange — four clipped quotes with their labels, ~1.1k —
 *  always fits whole. The budget therefore only ever drops OLDER turns, and the
 *  newest one can never be squeezed out by its own length. */
const CONTEXT_MAX_CHARS = 1_600;
/** A message this long carries its own referents, so context would cost the
 *  most exactly where it buys the least. */
const CONTEXT_SKIP_SOURCE_CHARS = 800;
const HISTORY_LIMIT = CONTEXT_EXCHANGES + 1;

type Direction = "input" | "output";

function settings(): TranslationSettings {
  const agentDir = process.env.PI_CODING_AGENT_DIR;
  if (!agentDir) return DEFAULTS;
  try {
    const parsed = JSON.parse(readFileSync(join(agentDir, SETTINGS_FILE), "utf8"));
    return {
      ...DEFAULTS,
      ...parsed,
      // Migrate settings written by the first version of this extension.
      translationModel: parsed.translationModel ?? parsed.model ?? "",
      enabledModels: Array.isArray(parsed.enabledModels) ? parsed.enabledModels : [],
      outputMode: parsed.outputMode === "replace" ? "replace" : "append",
    };
  } catch {
    return DEFAULTS;
  }
}

interface EngineClient {
  call(method: string, params?: Record<string, unknown>): Promise<any>;
}

interface LanguageDetection {
  language?: string;
  confidence?: number;
  reliable?: boolean;
}

let detectorPromise: Promise<EngineClient | undefined> | undefined;

/** `promise`, or `undefined` if it has not settled within `ms` — a rejection
 *  reads the same as a timeout, because every caller here treats both as "no
 *  answer". Nothing on the translation path may hang: Pi withholds its
 *  `message_end` event until the handler returns, and a turn whose stream goes
 *  quiet is parked with everything after it dropped. */
function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T | undefined> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(undefined), ms);
    // A pending translation must never be the reason the process stays alive.
    timer.unref?.();
    const settle = (value: T | undefined) => {
      clearTimeout(timer);
      resolve(value);
    };
    promise.then(settle, () => settle(undefined));
  });
}

async function detectLanguage(value: string): Promise<LanguageDetection | undefined> {
  const modulePath = process.env.CYPHER_ENGINE_CLIENT_MODULE;
  const socketPath = process.env.CYPHER_ENGINE_SOCKET;
  if (!modulePath || !socketPath) return undefined;
  const pending = (detectorPromise ??= import(modulePath)
    .then((module) => module.connectEngine({ socketPath }))
    .catch(() => undefined));
  const client = await withTimeout(pending, DETECT_TIMEOUT_MS);
  if (!client) {
    // A failed connect must not poison the process: the engine restarts
    // independently of this pi child, so let the next call reconnect.
    if (detectorPromise === pending) detectorPromise = undefined;
    return undefined;
  }
  const detection = await withTimeout(
    client.call("DetectPiLanguage", { text: value }),
    DETECT_TIMEOUT_MS,
  );
  // A call that failed or timed out leaves a client that cannot be trusted:
  // drop it so detection resumes after the engine comes back instead of
  // silently staying off. A successful detection is always an object.
  if (!detection && detectorPromise === pending) detectorPromise = undefined;
  return detection;
}

/** Names and codes accepted for a configured language, mapped to ISO 639-3 —
 *  the code space the offline detector reports.
 *
 *  These are exactly the languages the detector is built with (`LANGUAGES` in
 *  `cypher_engine::pi_translation`) and exactly the ones the settings card
 *  offers. A name outside this table is not rejected, it simply carries no
 *  local decision, which keeps its messages going to the translation model. */
const LANGUAGE_ALIASES: Record<string, string> = {
  english: "eng", en: "eng", eng: "eng",
  chinese: "cmn", mandarin: "cmn", zh: "cmn", zho: "cmn", cmn: "cmn",
};

/** `undefined` means "no local decision": either `auto`, or a language this
 *  table does not know. An unknown name must never gate translation off — its
 *  raw form can never equal an ISO 639-3 code, so comparing it would silently
 *  disable translation entirely. */
export function languageCode(value: string): string | undefined {
  const normalized = value.trim().toLowerCase();
  if (!normalized || normalized === "auto") return undefined;
  return LANGUAGE_ALIASES[normalized];
}

/** The configured text, or `undefined` for `auto`/blank — what the prompt can
 *  actually name. */
function configuredName(value: string): string | undefined {
  const trimmed = value.trim();
  if (!trimmed || trimmed.toLowerCase() === "auto") return undefined;
  return trimmed;
}

/** An English name for a detected ISO 639-3 code, so an `auto` source can still
 *  be named in the prompt. */
export function languageName(code: string): string {
  try {
    return new Intl.DisplayNames(["en"], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/** One translation direction. Codes gate the offline pre-filter; names are what
 *  the prompt can say. */
export interface LanguagePair {
  fromCode?: string;
  fromName?: string;
  toCode?: string;
  toName?: string;
}

/** User's language → working language. */
function inputPair(config: TranslationSettings): LanguagePair {
  return {
    fromCode: languageCode(config.sourceLanguage),
    fromName: configuredName(config.sourceLanguage),
    toCode: languageCode(config.targetLanguage),
    toName: configuredName(config.targetLanguage),
  };
}

/** Working language → back to the user's language. The answer has to come back
 *  in the language the user writes in; translating it *into* the working
 *  language again would be a no-op whenever the request was translated on the
 *  way in, which is why append mode could never show two languages. With an
 *  `auto` source the return language is the one detected on the user's own
 *  message. */
function outputPair(
  config: TranslationSettings,
  detectedUserLanguage: string | undefined,
): LanguagePair {
  const toCode = languageCode(config.sourceLanguage) ?? detectedUserLanguage;
  return {
    fromCode: languageCode(config.targetLanguage),
    fromName: configuredName(config.targetLanguage),
    toCode,
    toName: configuredName(config.sourceLanguage) ?? (toCode ? languageName(toCode) : undefined),
  };
}

/** The pure half of the offline pre-filter, split out so the gating rules are
 *  testable without an engine socket. Only a *reliable* detection may skip the
 *  paid request; anything uncertain falls through to the translation model. */
export function translationDecision(
  detected: LanguageDetection | undefined,
  pair: LanguagePair,
): boolean {
  if (!pair.toName) return false;
  // A named origin the alias table does not know is one the detector was not
  // built with, and a detector asked about a language it does not know answers
  // with the nearest one it does: French comes back as confident English. Its
  // verdict cannot be allowed to skip anything, so such a message always goes
  // to the translation model. This is what keeps a language saved before the
  // supported set was trimmed working instead of silently going untranslated.
  if (pair.fromName && !pair.fromCode) return true;
  if (!detected?.reliable || typeof detected.language !== "string") return true;
  const language = detected.language;
  // Already in the destination language: nothing to do.
  if (pair.toCode && language === pair.toCode) return false;
  // An explicit origin language only translates text in that language.
  if (pair.fromCode && language !== pair.fromCode) return false;
  return true;
}

function sessionModelId(ctx: ExtensionContext): string | undefined {
  const model = ctx.model;
  return model ? `${model.provider}/${model.id}` : undefined;
}

function enabledForSession(ctx: ExtensionContext, config: TranslationSettings): boolean {
  const id = sessionModelId(ctx);
  return Boolean(id && config.enabledModels.includes(id));
}

function configuredModel(ctx: ExtensionContext, id: string) {
  const slash = id.indexOf("/");
  if (slash <= 0 || slash === id.length - 1) return undefined;
  return ctx.modelRegistry.find(id.slice(0, slash), id.slice(slash + 1));
}

/** Warn once per distinct failure: this runs on every eligible message, and a
 *  notification per message would bury the session in warnings. */
let warnedModel: string | undefined;
let warnedError: string | undefined;

/** The language the user actually writes in, learned from their own messages so
 *  an `auto` source still knows where to translate answers back to. */
let lastUserLanguage: string | undefined;

/** One side of an earlier turn, in both languages once it has been translated.
 *  Keeping both is what makes the block a translation memory rather than mere
 *  topic: the pair shows which word was already chosen for which term, so the
 *  next turn reuses it instead of alternating synonyms. */
interface TurnText {
  original: string;
  translated?: string;
}

/** A user message and the answer it drew. Text only — tool calls, tool output
 *  and thinking are deliberately never recorded: they are the bulk of a coding
 *  transcript's tokens, they carry no word sense, and the translation model is
 *  frequently a different provider than the coding model. */
interface Exchange {
  user?: TurnText;
  assistant?: TurnText;
}

let historySession: string | undefined;
let history: Exchange[] = [];

function sessionId(ctx: ExtensionContext): string | undefined {
  try {
    return ctx.sessionManager?.getSessionId();
  } catch {
    return undefined;
  }
}

/** Scoped to one session: a switch, a fork or a new session must not inherit
 *  the previous branch's words. */
function historyFor(ctx: ExtensionContext): Exchange[] {
  const id = sessionId(ctx);
  if (id !== historySession) {
    historySession = id;
    history = [];
  }
  return history;
}

function remember(entries: Exchange[], exchange: Exchange): void {
  entries.push(exchange);
  while (entries.length > HISTORY_LIMIT) entries.shift();
}

function recordUserTurn(ctx: ExtensionContext, original: string, translated?: string): void {
  remember(historyFor(ctx), { user: { original, translated } });
}

function recordAssistantTurn(ctx: ExtensionContext, original: string, translated?: string): void {
  const entries = historyFor(ctx);
  const last = entries[entries.length - 1];
  const turn: TurnText = { original, translated };
  if (last && !last.assistant) last.assistant = turn;
  else remember(entries, { assistant: turn });
}

/** Collapsed to one line per turn: the block is reference, not layout, and a
 *  multi-line quote would read like a second payload to translate. */
function clip(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length <= CONTEXT_CHARS_PER_TURN
    ? flat
    : `${flat.slice(0, CONTEXT_CHARS_PER_TURN).trimEnd()}…`;
}

function quote(role: string, language: string | undefined, text: string | undefined): string[] {
  const body = text?.trim();
  if (!body) return [];
  return [`${role}${language ? ` (${language})` : ""}: ${clip(body)}`];
}

/** The recent conversation as plain text, newest last.
 *
 *  Which language sits on which side flips with the direction, and getting it
 *  wrong is the difference between fixing terminology and scrambling it: on the
 *  way in the pair runs user language → working language, on the way out it runs
 *  back, so the same stored turn has to be labelled from the current pair. */
export function referenceBlock(
  exchanges: readonly Exchange[],
  direction: Direction,
  pair: LanguagePair,
): string | undefined {
  const userName = direction === "input" ? pair.fromName : pair.toName;
  const workingName = direction === "input" ? pair.toName : pair.fromName;
  const blocks: string[] = [];
  let budget = CONTEXT_MAX_CHARS;
  for (const exchange of exchanges.slice(-CONTEXT_EXCHANGES).reverse()) {
    const lines = [
      ...quote("User", userName, exchange.user?.original),
      ...quote("User", workingName, exchange.user?.translated),
      ...quote("Assistant", workingName, exchange.assistant?.original),
      ...quote("Assistant", userName, exchange.assistant?.translated),
    ];
    if (!lines.length) continue;
    const block = lines.join("\n");
    // Whole turns only: half an exchange can strand a term away from its
    // translation, which is the pairing the block exists to show.
    if (block.length > budget) break;
    budget -= block.length;
    blocks.unshift(block);
  }
  return blocks.length ? blocks.join("\n") : undefined;
}

/** The instructions. Separate from the payload on purpose: the message sent to
 *  the model holds the text to translate and nothing else, so there is no
 *  second body for it to translate, answer, or splice into its output. */
export function translationSystemPrompt(
  direction: Direction,
  pair: LanguagePair,
  reference?: string,
): string {
  const subject = direction === "input" ? "a user's request" : "an assistant's answer";
  const lines = [
    "You are a precise translation engine.",
    pair.fromName
      ? `The next message is ${subject} written in ${pair.fromName}. Translate it into ${pair.toName}.`
      : `The next message is ${subject}. Translate it into ${pair.toName}.`,
    "Output the translation itself and nothing else: no preamble, no explanation, no commentary, no notes, no label, no surrounding quotation marks and no code fence around the answer.",
    "Preserve Markdown, code fences, inline code, URLs, file paths, identifiers and formatting exactly as they appear.",
    `If the message is already in ${pair.toName}, output it unchanged.`,
  ];
  if (reference) {
    lines.push(
      "",
      "Earlier turns of the conversation follow, for reference only. Use them to settle words that have several meanings and to stay consistent with terminology already chosen. Never translate them, never answer them and never mention them.",
      reference,
    );
  }
  return lines.join("\n");
}

/** The prompt forbids a wrapping fence, but a model that adds one anyway would
 *  otherwise publish ``` lines straight into the transcript. Only a fence that
 *  wraps the WHOLE answer is stripped, and only when the source is not itself a
 *  code block — a message that is one must translate to one. */
export function unwrapTranslation(text: string, source: string): string {
  const trimmed = text.trim();
  if (source.trimStart().startsWith("```")) return trimmed;
  const fenced = /^```[^\n]*\n([\s\S]*?)\n?```$/.exec(trimmed);
  return fenced ? fenced[1].trim() : trimmed;
}

/** The mid-stream form of [`unwrapTranslation`]. A wrapping fence has no
 *  closing line yet while the answer is still arriving, so the strict pattern
 *  cannot match and the opener would otherwise be published as literal
 *  backticks and then taken back at the end.
 *
 *  Everything is held back until the opener's line is complete: `""` means
 *  "nothing publishable yet", not "empty translation". The source is checked
 *  first for the same reason as in the strict form — a message that IS a code
 *  block must translate to one, fence included. */
export function unwrapPartialTranslation(text: string, source: string): string {
  const trimmed = text.trimStart();
  if (source.trimStart().startsWith("```")) return trimmed;
  if (!trimmed.startsWith("`")) return trimmed;
  // A leading run of backticks may still be growing into a fence opener, so
  // hold it back until it is long enough to tell apart from inline code.
  if (!trimmed.startsWith("```")) return trimmed.length < 3 ? "" : trimmed;
  const newline = trimmed.indexOf("\n");
  return newline < 0 ? "" : trimmed.slice(newline + 1).trimStart();
}

/** What the message's text should render as, given a translation of it.
 *
 *  The extension renders rather than the harness, and that is what makes a
 *  frame idempotent: every frame is the WHOLE replacement, so re-sending one,
 *  or landing a later one first, converges instead of appending a second copy
 *  of the answer. */
export function renderTranslation(
  original: string,
  translated: string,
  mode: OutputMode,
): string {
  return mode === "append" ? `${original}\n\n---\n\n${translated}` : translated;
}

/** Paces the frames a streaming translation publishes.
 *
 *  Two jobs, and the second is the one that matters. Frames are throttled to
 *  Cypher's own doc-commit cadence, so a fast model does not cost a whole-text
 *  snapshot per token. And a frame is re-sent on a keepalive even when nothing
 *  changed, because the engine parks a turn whose stream falls silent and drops
 *  everything that arrives after the park — a keepalive frame changes no text
 *  and exists purely to prove the turn is still working.
 *
 *  The clock is injectable so the pacing rules can be tested without waiting
 *  for wall time. */
export class FramePump {
  private latest: string | undefined;
  private sentText: string | undefined;
  /** Before the first frame, not at time zero: the opening frame must go out
   *  the moment there is anything to say, whatever the clock reads. */
  private sentAt = Number.NEGATIVE_INFINITY;
  private readonly send: (text: string) => void;
  private readonly now: () => number;
  private readonly frameMs: number;
  private readonly keepaliveMs: number;

  constructor(
    send: (text: string) => void,
    now: () => number = Date.now,
    frameMs: number = FRAME_MS,
    keepaliveMs: number = KEEPALIVE_MS,
  ) {
    this.send = send;
    this.now = now;
    this.frameMs = frameMs;
    this.keepaliveMs = keepaliveMs;
  }

  /** A new rendering is available; published at the next frame boundary. */
  offer(text: string): void {
    this.latest = text;
    this.tick();
  }

  /** Called on a timer: publishes a pending change once the throttle window
   *  has passed, and re-publishes an unchanged frame on the keepalive. */
  tick(): void {
    if (this.latest === undefined) return;
    const since = this.now() - this.sentAt;
    const due = this.latest === this.sentText ? this.keepaliveMs : this.frameMs;
    if (since >= due) this.flush();
  }

  /** Publish now, throttle or not — the definitive last frame of a translation,
   *  which has to land whatever the pacing rules would have said. */
  flush(text?: string): void {
    if (text !== undefined) this.latest = text;
    if (this.latest === undefined) return;
    this.send(this.latest);
    this.sentText = this.latest;
    this.sentAt = this.now();
  }
}

/** One frame of the side-band channel Cypher's harness folds into the rendered
 *  transcript, replacing the message's text with `text`. Pi's own message
 *  history is deliberately left in the working language. */
function publishTranslation(ctx: ExtensionContext, text: string): void {
  ctx.ui.setStatus(TRANSLATION_STATUS_KEY, JSON.stringify({ version: 1, text }));
}

/** Only Cypher's transcript has a prompt entry to keep the pair beside; in the
 *  TUI a status is footer text, and this one would print the whole prompt. */
function publishInputTranslation(ctx: ExtensionContext, source: string, text: string): void {
  if (!(process.env.CYPHER_ENGINE_SOCKET && process.env.CYPHER_CHAT_ID)) return;
  ctx.ui.setStatus(
    INPUT_TRANSLATION_STATUS_KEY,
    JSON.stringify(inputTranslationStatus(source, text)),
  );
}

export function inputTranslationStatus(source: string, text: string) {
  return { version: 1, source: source.trim(), text };
}

/** Translation is a mechanical rewrite, so reasoning models should not think.
 *
 *  Requesting an effort level is the wrong lever and actively harmful: the
 *  OpenAI adapters already fall back to this model's own
 *  `thinkingLevelMap.off` when no effort is asked for, so naming a level turns
 *  thinking back *on*. Worse, the adapter resolves a level as
 *  `thinkingLevelMap[level] ?? level`, and `??` does not catch the `null` that
 *  marks a level as unsupported — so an unsupported level is forwarded
 *  verbatim and the provider rejects the whole request (`level "minimal" not
 *  supported`). Anthropic is the one family that has to be told explicitly. */
export function noThinkingOptions(model: Pick<Model<Api>, "api" | "reasoning">): Record<string, unknown> {
  if (!model.reasoning) return {};
  return model.api === "anthropic-messages" ? { thinkingEnabled: false } : {};
}

function textOf(content: unknown): string {
  return Array.isArray(content)
    ? content
        .filter((part): part is TextContent => part?.type === "text")
        .map((part) => part.text)
        .join("")
    : typeof content === "string"
      ? content
      : "";
}

function responseText(message: AssistantMessage): string {
  return textOf(message.content);
}

function hasToolCall(message: AssistantMessage): boolean {
  return message.content.some((part) => part?.type === "toolCall");
}

/** Translate `value`, feeding `onPartial` the translation so far as it
 *  arrives. Resolves with the finished translation, or `undefined` when there
 *  is nothing to apply — no model, a failed request, or an answer that came
 *  back unchanged. */
async function translate(
  value: string,
  direction: Direction,
  pair: LanguagePair,
  ctx: ExtensionContext,
  config: TranslationSettings,
  onPartial?: (translated: string) => void,
  onIdle?: () => void,
): Promise<string | undefined> {
  const source = value.trim();
  if (!source || source.length > MAX_TRANSLATION_CHARS || !pair.toName) return undefined;
  const configured = config.translationModel.trim();
  const model = configuredModel(ctx, configured);
  if (!model) {
    if (warnedModel !== configured) {
      warnedModel = configured;
      ctx.ui.notify(`Translation model "${configured}" is not available.`, "warning");
    }
    return undefined;
  }
  warnedModel = undefined;

  const reference =
    source.length > CONTEXT_SKIP_SOURCE_CHARS
      ? undefined
      : referenceBlock(historyFor(ctx), direction, pair);

  // Time-to-first-token is dead air on the session's stream, and so is a model
  // that pauses mid-answer, so the idle tick runs for the whole request rather
  // than only between deltas.
  const idle = onIdle ? setInterval(onIdle, FRAME_MS) : undefined;
  idle?.unref?.();
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), TRANSLATION_TIMEOUT_MS);
  deadline.unref?.();
  try {
    const stream = ctx.modelRegistry.stream(
      model,
      {
        systemPrompt: translationSystemPrompt(direction, pair, reference),
        messages: [{ role: "user", content: source, timestamp: Date.now() }],
      },
      { ...noThinkingOptions(model), signal: controller.signal },
    );
    for await (const event of stream) {
      if (event.type !== "text_delta" && event.type !== "text_end") continue;
      // `partial` is the live response-so-far, which is exactly the shape a
      // frame wants: the whole translation up to here, not this delta.
      const partial = unwrapPartialTranslation(textOf(event.partial.content), source);
      if (partial) onPartial?.(partial);
    }
    const result = await stream.result();
    // The stream resolves with a failed message instead of throwing, so an API
    // rejection would otherwise disable translation with no sign of it.
    if (result.stopReason === "error") {
      const detail = result.errorMessage ?? "the translation request failed";
      if (warnedError !== detail) {
        warnedError = detail;
        ctx.ui.notify(`Translation failed: ${detail}`, "warning");
      }
      return undefined;
    }
    warnedError = undefined;
    // No JSON envelope to unpack: the model answers with the translation and
    // nothing else. The skip signal that `shouldTranslate` used to carry is
    // the instruction to echo a message that is already in the destination
    // language, which lands here as an unchanged answer.
    const translation = unwrapTranslation(textOf(result.content), source);
    return translation && translation !== source ? translation : undefined;
  } catch (error) {
    // A timeout arrives as an abort. Whatever was streamed is deliberately NOT
    // kept: a half-translated answer reads like a whole one, and acting on the
    // half that arrived is worse than reading the original.
    const detail = controller.signal.aborted
      ? `it did not finish within ${Math.round(TRANSLATION_TIMEOUT_MS / 1_000)}s`
      : error instanceof Error
        ? error.message
        : String(error);
    ctx.ui.notify(`Translation skipped: ${detail}`, "warning");
    return undefined;
  } finally {
    clearTimeout(deadline);
    if (idle) clearInterval(idle);
  }
}

/**
 * The session entry recording a rewritten prompt. Pi keeps only the
 * translation as the user message, but Cypher's transcript shows what the user
 * typed, and Session Fork / Restart match the two by text — this record is the
 * one place that pairing survives. Read by `TRANSLATION_INPUT_ENTRY` in
 * `crates/harness/src/pi/fork.rs`; the name and fields are a contract.
 */
export const TRANSLATION_INPUT_ENTRY = "cypher-translation-input";

export function translationInputRecord(original: string, translated: string) {
  return { original, translated };
}

/** Opens the user's request after Cypher's reference blocks
 *  (`REQUEST_MARKER` in `crates/proto/src/agent_prompt.rs`). */
const REQUEST_MARKER = "\n\nUser request:\n";
const COMMENTS_LEAD = "Conversation annotations (JSON):";
/** A quote's alignment input (`ALIGN_KEY` in `crates/proto/src/agent_prompt.rs`):
 *  it holds the translated text the user selected from, so it is resolved and
 *  removed here and never reaches the agent. */
const ALIGN_KEY = "cypherAlign";
/** Alignment is one short request; a stalled one must not hold the prompt. */
const ALIGN_TIMEOUT_MS = 30_000;
/** The model has to reproduce the passage to mark it, so the cost of the call
 *  grows with it. Above this only the back-translation is asked for. */
const ALIGN_MAX_PASSAGE_CHARS = 2_000;
/** Candidate span markers — the first pair that occurs in none of the texts
 *  is used, so a marker can never be confused with the text itself. */
const MARKER_PAIRS: Array<[string, string]> = [
  ["⟦", "⟧"],
  ["⟪", "⟫"],
  ["〖", "〗"],
  ["⦃", "⦄"],
];
/** Marks the selected occurrence in `passage` when the quoted words occur in
 *  it more than once. */
const OCCURRENCE_MARKERS: [string, string] = ["⟦", "⟧"];

type Json = Record<string, unknown>;

/** One reference block of a Cypher prompt: its whole line, lead and JSON. */
export interface EnvelopeBlock {
  line: string;
  lead: string;
  json: Json;
}

export interface Envelope {
  blocks: EnvelopeBlock[];
  request: string;
}

/**
 * Split a prompt Cypher wrapped around the user's request — pending comments,
 * referenced sessions, a Side Chat's first send — into its reference blocks
 * and the request. The layout is the contract in
 * `crates/proto/src/agent_prompt.rs`: one-line blocks, each a lead sentence
 * (no `{`) and a JSON object, separated by blank lines, then the request.
 * JSON escapes line breaks, so the first request marker is the real one.
 *
 * `undefined` for anything else — an ordinary prompt, or text that only looks
 * similar — which is then translated whole, as before.
 */
export function splitEnvelope(text: string): Envelope | undefined {
  const at = text.indexOf(REQUEST_MARKER);
  if (at <= 0) return undefined;
  const blocks: EnvelopeBlock[] = [];
  for (const line of text.slice(0, at).split("\n\n")) {
    if (line.includes("\n")) return undefined;
    const brace = line.indexOf("{");
    if (brace <= 0 || line[brace - 1] !== " ") return undefined;
    let json: unknown;
    try {
      json = JSON.parse(line.slice(brace));
    } catch {
      return undefined;
    }
    if (!json || typeof json !== "object" || Array.isArray(json)) return undefined;
    blocks.push({ line, lead: line.slice(0, brace - 1), json: json as Json });
  }
  return { blocks, request: text.slice(at + REQUEST_MARKER.length) };
}

function commentsOf(block: EnvelopeBlock): unknown[] | undefined {
  const comments = block.json.comments;
  return block.lead.startsWith(COMMENTS_LEAD) && Array.isArray(comments) ? comments : undefined;
}

function isJson(value: unknown): value is Json {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

/** The translated text a quote was selected from, and the original passage it
 *  was translated from (`QuoteAlign` in `crates/proto/src/agent_prompt.rs`). */
export interface QuoteAlign {
  passage: string;
  before: string;
  selected: string;
  after: string;
}

function asAlign(value: unknown): QuoteAlign | undefined {
  if (!isJson(value)) return undefined;
  const { passage, before, selected, after } = value;
  return [passage, before, selected, after].every((field) => typeof field === "string")
    ? (value as unknown as QuoteAlign)
    : undefined;
}

/** A quote to align: a comment's `quotedText` or a side chat's `selectedText`. */
export interface AlignSite {
  key: string;
  align: QuoteAlign;
}

function siteKey(block: number, entry?: number): string {
  return entry === undefined ? `${block}` : `${block}:${entry}`;
}

export function alignSites(envelope: Envelope): AlignSite[] {
  const sites: AlignSite[] = [];
  envelope.blocks.forEach((block, b) => {
    const comments = commentsOf(block);
    if (comments) {
      comments.forEach((entry, e) => {
        const align = isJson(entry) ? asAlign(entry[ALIGN_KEY]) : undefined;
        if (align) sites.push({ key: siteKey(b, e), align });
      });
      return;
    }
    const align = asAlign(block.json[ALIGN_KEY]);
    if (align) sites.push({ key: siteKey(b), align });
  });
  return sites;
}

/** What a translated quote resolved to:
 *  - `exact`: its words in the original, verified present in the passage —
 *    `passage` marks the occurrence when the words occur more than once;
 *  - `approximate`: a back-translation of the selection, when the exact words
 *    could not be recovered;
 *  - `passage`: nothing could be asked, so the original passage stands. */
export type Resolution =
  | { kind: "exact"; text: string; passage: string }
  | { kind: "approximate"; text: string }
  | { kind: "passage" };

/** `passage`, with the occurrence at `start..end` marked when the same words
 *  occur in it more than once — otherwise the quote alone could not say which
 *  one the user meant. */
export function markOccurrence(passage: string, start: number, end: number): string {
  const text = passage.slice(start, end);
  const first = passage.indexOf(text);
  if (first === start && passage.indexOf(text, first + 1) < 0) return passage;
  const [open, close] = OCCURRENCE_MARKERS;
  if (passage.includes(open) || passage.includes(close)) return passage;
  return `${passage.slice(0, start)}${open}${text}${close}${passage.slice(end)}`;
}

/** No request needed when the selection is itself in the passage exactly once:
 *  code, paths, identifiers and numbers survive translation untouched. */
export function literalResolution(align: QuoteAlign): Resolution | undefined {
  const selected = align.selected.trim();
  if (!selected) return undefined;
  const first = align.passage.indexOf(selected);
  if (first < 0 || align.passage.indexOf(selected, first + 1) >= 0) return undefined;
  return { kind: "exact", text: selected, passage: align.passage };
}

export interface AlignmentRequest {
  systemPrompt: string;
  message: string;
  /** The span markers the reply must use; `undefined` when only a
   *  back-translation is asked for. */
  markers?: [string, string];
}

/** The alignment request: the original passage, and its translation with the
 *  user's selection marked. The reply marks the matching words in the
 *  original (when `markers` is set) and translates the selection back. */
export function alignmentRequest(
  align: QuoteAlign,
  originalLanguage: string,
  shownLanguage: string,
): AlignmentRequest {
  const texts = [align.passage, align.before, align.selected, align.after];
  const pair = MARKER_PAIRS.find(([open, close]) =>
    texts.every((text) => !text.includes(open) && !text.includes(close)),
  );
  const markers =
    pair && align.passage.length <= ALIGN_MAX_PASSAGE_CHARS ? pair : undefined;
  const [open, close] = pair ?? ["", ""];
  const translation = pair
    ? `${align.before}${open}${align.selected}${close}${align.after}`
    : `${align.before}${align.selected}${align.after}`;
  const lines = [
    "You align a selection in a translated text with the original text it was translated from.",
    `The next message is a JSON object: "original" is a passage written in ${originalLanguage}; "translation" is its translation into ${shownLanguage}; "selection" is the part of the translation the user selected${pair ? `, also enclosed in ${open} and ${close} inside "translation"` : ""}.`,
    `Reply with one JSON object and nothing else — no code fence, no commentary: ${markers ? '{"marked": "…", "backTranslation": "…"}' : '{"backTranslation": "…"}'}`,
  ];
  if (markers) {
    lines.push(
      `- marked: "original" reproduced exactly, character for character, with ${open} inserted immediately before and ${close} immediately after the shortest continuous stretch of it that the selection translates. Change nothing else.`,
    );
  }
  lines.push(`- backTranslation: the selection translated into ${originalLanguage}.`);
  return {
    systemPrompt: lines.join("\n"),
    message: JSON.stringify({
      original: align.passage,
      translation,
      selection: align.selected,
    }),
    markers,
  };
}

/** Read an alignment reply. The span is only trusted when removing the two
 *  markers gives back the passage exactly (outer whitespace aside) — the
 *  words are then the passage's own, at a known position. */
export function parseAlignment(
  reply: string,
  align: QuoteAlign,
  markers: [string, string] | undefined,
): { span?: { start: number; end: number }; backTranslation?: string } {
  const open = reply.indexOf("{");
  const close = reply.lastIndexOf("}");
  if (open < 0 || close <= open) return {};
  let json: unknown;
  try {
    json = JSON.parse(reply.slice(open, close + 1));
  } catch {
    return {};
  }
  if (!isJson(json)) return {};
  const out: { span?: { start: number; end: number }; backTranslation?: string } = {};
  if (typeof json.backTranslation === "string" && json.backTranslation.trim()) {
    out.backTranslation = json.backTranslation.trim();
  }
  if (!markers || typeof json.marked !== "string") return out;
  const marked = json.marked;
  const [l, r] = markers;
  const at = marked.indexOf(l);
  const to = marked.indexOf(r);
  if (at < 0 || to < at || marked.indexOf(l, at + 1) >= 0 || marked.indexOf(r, to + 1) >= 0) {
    return out;
  }
  const plain = marked.slice(0, at) + marked.slice(at + l.length, to) + marked.slice(to + r.length);
  const passage = align.passage;
  let shift: number;
  if (plain === passage) {
    shift = 0;
  } else if (plain.trim() === passage.trim()) {
    shift =
      passage.length - passage.trimStart().length - (plain.length - plain.trimStart().length);
  } else {
    return out;
  }
  let start = at + shift;
  let end = to - l.length + shift;
  if (start < 0 || end > passage.length) return out;
  while (start < end && /\s/.test(passage[start])) start++;
  while (end > start && /\s/.test(passage[end - 1])) end--;
  if (start < end && passage.slice(start, end) === marked.slice(at + l.length, to).trim()) {
    out.span = { start, end };
  }
  return out;
}

/** One non-streaming request to the translation model; `undefined` on any
 *  failure, which the caller treats as "no answer". */
async function complete(
  ctx: ExtensionContext,
  model: Model<Api>,
  systemPrompt: string,
  message: string,
  timeoutMs: number,
): Promise<string | undefined> {
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), timeoutMs);
  deadline.unref?.();
  try {
    const stream = ctx.modelRegistry.stream(
      model,
      { systemPrompt, messages: [{ role: "user", content: message, timestamp: Date.now() }] },
      { ...noThinkingOptions(model), signal: controller.signal },
    );
    for await (const _event of stream) {
      // Drained for the result; nothing is published from an alignment.
    }
    const result = await stream.result();
    return result.stopReason === "error" ? undefined : textOf(result.content);
  } catch {
    return undefined;
  } finally {
    clearTimeout(deadline);
  }
}

/** Resolve one translated quote to the agent's own words. */
async function resolveAlignment(
  align: QuoteAlign,
  ctx: ExtensionContext,
  config: TranslationSettings,
): Promise<Resolution> {
  const literal = literalResolution(align);
  if (literal) return literal;
  const configured = config.translationModel.trim();
  const model = configured ? configuredModel(ctx, configured) : undefined;
  if (!model || align.selected.length > MAX_TRANSLATION_CHARS) return { kind: "passage" };
  const pair = inputPair(config);
  const shown =
    pair.fromName ?? (lastUserLanguage ? languageName(lastUserLanguage) : undefined);
  const request = alignmentRequest(
    align,
    pair.toName ?? "its original language",
    shown ?? "another language",
  );
  const reply = await complete(ctx, model, request.systemPrompt, request.message, ALIGN_TIMEOUT_MS);
  if (!reply) return { kind: "passage" };
  const { span, backTranslation } = parseAlignment(reply, align, request.markers);
  if (span) {
    return {
      kind: "exact",
      text: align.passage.slice(span.start, span.end),
      passage: markOccurrence(align.passage, span.start, span.end),
    };
  }
  return backTranslation ? { kind: "approximate", text: backTranslation } : { kind: "passage" };
}

/** A quote entry with its alignment input resolved and removed. The quote
 *  field keeps its place; `passage` follows it. */
function resolvedEntry(
  entry: Json,
  key: string,
  resolution: Resolution | undefined,
): Json {
  const align = asAlign(entry[ALIGN_KEY]);
  const out: Json = {};
  for (const [name, value] of Object.entries(entry)) {
    if (name === ALIGN_KEY) continue;
    if (name === key && align && resolution?.kind === "exact") {
      out[key] = resolution.text;
      out.passage = resolution.passage;
    } else if (name === key && align && resolution?.kind === "approximate") {
      out.approximateText = resolution.text;
      out.passage = align.passage;
    } else {
      out[name] = value;
    }
  }
  return out;
}

/** What the lead has to say about resolved quotes in its block. */
function alignmentNotes(subject: string, key: string, resolutions: Resolution[]): string[] {
  const notes: string[] = [];
  const exact = resolutions.filter((r) => r.kind === "exact");
  if (exact.length || resolutions.some((r) => r.kind === "approximate")) {
    notes.push(`Where ${subject} has passage, that is the whole original paragraph it comes from.`);
  }
  if (exact.some((r) => r.kind === "exact" && r.passage.includes(OCCURRENCE_MARKERS[0]))) {
    notes.push(
      `Where the ${key} words occur more than once in passage, the occurrence the user selected is enclosed in ${OCCURRENCE_MARKERS[0]} ${OCCURRENCE_MARKERS[1]}.`,
    );
  }
  if (resolutions.some((r) => r.kind === "approximate")) {
    notes.push(
      `Where ${subject} has approximateText instead of ${key}, the exact words could not be recovered: approximateText is a back-translation of what the user selected, and the words it stands for are in passage, possibly worded differently.`,
    );
  }
  return notes;
}

/** Put a split prompt back together: comment notes and the request replaced
 *  where they were translated, and every alignment input resolved (or, with
 *  no resolution, simply removed — the quote already holds the original
 *  passage). Blocks nothing changed keep their exact original line. */
export function joinEnvelope(
  envelope: Envelope,
  request: string,
  notes: ReadonlyMap<string, string> = new Map(),
  resolutions: ReadonlyMap<string, Resolution> = new Map(),
): string {
  const lines = envelope.blocks.map((block, b) => {
    const comments = commentsOf(block);
    const used: Resolution[] = [];
    let changed = false;
    let json = block.json;
    if (comments) {
      json = {
        ...json,
        comments: comments.map((entry, e) => {
          if (!isJson(entry)) return entry;
          let next = entry;
          if (ALIGN_KEY in next) {
            const resolution = resolutions.get(siteKey(b, e));
            if (resolution && asAlign(next[ALIGN_KEY])) used.push(resolution);
            next = resolvedEntry(next, "quotedText", resolution);
            changed = true;
          }
          const translated = typeof next.comment === "string" ? notes.get(next.comment) : undefined;
          if (translated !== undefined) {
            next = { ...next, comment: translated };
            changed = true;
          }
          return next;
        }),
      };
    } else if (ALIGN_KEY in json) {
      const resolution = resolutions.get(siteKey(b));
      if (resolution && asAlign(json[ALIGN_KEY])) used.push(resolution);
      json = resolvedEntry(json, "selectedText", resolution);
      changed = true;
    }
    if (!changed) return block.line;
    const lead = [
      block.lead,
      ...(comments
        ? alignmentNotes("a comment", "quotedText", used)
        : alignmentNotes("the selection", "selectedText", used)),
    ].join(" ");
    return `${lead} ${JSON.stringify(json)}`;
  });
  return `${lines.join("\n\n")}${REQUEST_MARKER}${request}`;
}

/** The user's own words in a prompt: the request, and the comment notes of a
 *  wrapped one. Quoted text, referenced transcripts and a side chat's context
 *  are reference material in the agent's language already — translating them
 *  would hand the agent a paraphrase of its own words. */
function userWords(envelope: Envelope | undefined, text: string): string[] {
  if (!envelope) return [text];
  const words = [envelope.request];
  for (const block of envelope.blocks) {
    for (const entry of commentsOf(block) ?? []) {
      if (isJson(entry) && typeof entry.comment === "string") words.push(entry.comment);
    }
  }
  return words;
}

async function transformInput(
  pi: ExtensionAPI,
  event: InputEvent,
  ctx: ExtensionContext,
  config: TranslationSettings,
) {
  const text = event.text.trim();
  if (!text || text.startsWith("/") || text.startsWith("!")) return;
  const envelope = splitEnvelope(event.text);
  // Alignment input is resolved and removed whatever the session's own
  // translation setting: it holds the translated text the user selected
  // from, which the agent must never receive.
  const aligning = Boolean(envelope && event.text.includes(`"${ALIGN_KEY}"`));
  const enabled = enabledForSession(ctx, config);
  if (!enabled && !aligning) return;
  const alignment = Promise.all(
    (envelope && aligning ? alignSites(envelope) : []).map(
      async (site) => [site.key, await resolveAlignment(site.align, ctx, config)] as const,
    ),
  );
  const request = envelope ? envelope.request : event.text;
  const translations = new Map<string, string>();
  if (enabled) {
    const words = [...new Set(userWords(envelope, event.text).filter((w) => w.trim()))];
    // Learn the user's language even when their messages are not translated:
    // it is what the answer has to be translated back into. Judged on their
    // own words only — a wrapped prompt's reference material is in the
    // agent's.
    const detected = await detectLanguage(request.trim() ? request : words.join("\n"));
    if (detected?.reliable && typeof detected.language === "string") {
      lastUserLanguage = detected.language;
    }
    const pair = inputPair(config);
    const wanted = config.translateUserMessages && Boolean(config.translationModel.trim());
    if (wanted) {
      await Promise.all(
        words.map(async (source) => {
          const decision = source === request ? detected : await detectLanguage(source);
          if (!translationDecision(decision, pair)) return;
          const translated = await translate(source, "input", pair, ctx, config);
          if (translated && translated !== source.trim()) translations.set(source, translated);
        }),
      );
    }
    // Recorded after the request, so the reference block a message is
    // translated against holds only turns that came before it — and recorded
    // even when nothing was translated, because an untranslated turn still
    // fixes what the words in the next one refer to.
    if (request.trim()) recordUserTurn(ctx, request.trim(), translations.get(request));
  }
  const resolutions = new Map(await alignment);
  const translatedRequest = translations.get(request);
  if (!translations.size && !aligning) return;
  const transformed = envelope
    ? joinEnvelope(envelope, translatedRequest ?? request, translations, resolutions)
    : (translatedRequest ?? event.text);
  pi.appendEntry(TRANSLATION_INPUT_ENTRY, translationInputRecord(event.text, transformed));
  if (translatedRequest) publishInputTranslation(ctx, request, translatedRequest);
  return { action: "transform" as const, text: transformed, images: event.images };
}

async function transformFinalMessage(
  event: MessageEndEvent,
  ctx: ExtensionContext,
  config: TranslationSettings,
): Promise<MessageEndEventResult | undefined> {
  const message = event.message as AgentMessage;
  // An intermediate message with a tool call is not the answer: only the final
  // text, after the tool calling and the thinking, is translated or recorded.
  if (!enabledForSession(ctx, config) || message.role !== "assistant" || hasToolCall(message)) {
    return undefined;
  }
  const assistant = message as AssistantMessage;
  const original = responseText(assistant);
  if (!original.trim()) return undefined;
  const pair = outputPair(config, lastUserLanguage);
  const wanted = config.translateFinalResponses && Boolean(config.translationModel.trim());
  if (!wanted || !translationDecision(await detectLanguage(original), pair)) {
    // Recorded even when nothing was translated: an untranslated turn still
    // fixes what the words in the next one refer to.
    recordAssistantTurn(ctx, original.trim(), undefined);
    return undefined;
  }

  // Cypher has already streamed the original text into its transcript and owns
  // the rendering, so the translation goes to it as side-band status frames
  // and Pi's own message history is left alone. Everywhere else — TUI, plain
  // RPC, SDK — there is no such transcript, so the message itself is rewritten
  // once at the end.
  const live = Boolean(process.env.CYPHER_ENGINE_SOCKET && process.env.CYPHER_CHAT_ID);
  const pump = live ? new FramePump((text) => publishTranslation(ctx, text)) : undefined;
  // An opening frame that re-states what is already on screen. It renders as
  // no change at all, and exists only to prove the turn is still working while
  // the translation model spends its time-to-first-token.
  pump?.flush(original);

  const translated = await translate(
    original,
    "output",
    pair,
    ctx,
    config,
    pump && ((partial) => pump.offer(renderTranslation(original, partial, config.outputMode))),
    pump && (() => pump.tick()),
  );
  recordAssistantTurn(ctx, original.trim(), translated);

  if (pump) {
    // The definitive last frame, sent whatever happened. The partial frames
    // already replaced the answer on screen, so a translation that failed,
    // timed out, came back empty or came back unchanged has to put the
    // original back rather than leave a half-translated answer standing.
    pump.flush(
      translated ? renderTranslation(original, translated, config.outputMode) : original,
    );
    return undefined;
  }
  if (!translated) return undefined;

  let usedText = false;
  const content = assistant.content.map((part) => {
    if (part.type !== "text") return part;
    if (usedText) return { ...part, text: "" };
    usedText = true;
    return {
      ...part,
      text: config.outputMode === "append"
        ? `${part.text}\n\n---\n\n${translated}`
        : translated,
    };
  });
  return { message: { ...assistant, content } };
}

export default function (pi: ExtensionAPI) {
  pi.on("input", (event, ctx) => transformInput(pi, event, ctx, settings()));
  pi.on("message_end", (event, ctx) => transformFinalMessage(event, ctx, settings()));
}

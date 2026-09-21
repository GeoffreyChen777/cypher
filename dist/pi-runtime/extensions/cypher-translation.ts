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
  outputMode: "replace",
  translateUserMessages: true,
  translateFinalResponses: true,
};

const SETTINGS_FILE = "translation.json";
const MAX_TRANSLATION_CHARS = 24_000;

/** Status key of the side-band translation channel Cypher's harness consumes. */
const TRANSLATION_STATUS_KEY = "cypher.translation.v1";
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
      outputMode: parsed.outputMode === "append" ? "append" : "replace",
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

async function transformInput(event: InputEvent, ctx: ExtensionContext, config: TranslationSettings) {
  const text = event.text.trim();
  if (!text || text.startsWith("/") || text.startsWith("!") || !enabledForSession(ctx, config)) {
    return;
  }
  // Learn the user's language even when their messages are not translated:
  // it is what the answer has to be translated back into.
  const detected = await detectLanguage(event.text);
  if (detected?.reliable && typeof detected.language === "string") {
    lastUserLanguage = detected.language;
  }
  const pair = inputPair(config);
  const wanted = config.translateUserMessages && Boolean(config.translationModel.trim());
  const translated =
    wanted && translationDecision(detected, pair)
      ? await translate(event.text, "input", pair, ctx, config)
      : undefined;
  // Recorded after the request, so the reference block a message is translated
  // against holds only turns that came before it — and recorded even when
  // nothing was translated, because an untranslated turn still fixes what the
  // words in the next one refer to.
  recordUserTurn(ctx, text, translated);
  if (translated) {
    return { action: "transform" as const, text: translated, images: event.images };
  }
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
  pi.on("input", (event, ctx) => transformInput(event, ctx, settings()));
  pi.on("message_end", (event, ctx) => transformFinalMessage(event, ctx, settings()));
}

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

async function detectLanguage(value: string): Promise<LanguageDetection | undefined> {
  const modulePath = process.env.CYPHER_ENGINE_CLIENT_MODULE;
  const socketPath = process.env.CYPHER_ENGINE_SOCKET;
  if (!modulePath || !socketPath) return undefined;
  const pending = (detectorPromise ??= import(modulePath)
    .then((module) => module.connectEngine({ socketPath }))
    .catch(() => undefined));
  const client = await pending;
  if (!client) {
    // A failed connect must not poison the process: the engine restarts
    // independently of this pi child, so let the next call reconnect.
    if (detectorPromise === pending) detectorPromise = undefined;
    return undefined;
  }
  try {
    return await client.call("DetectPiLanguage", { text: value });
  } catch {
    // Same reasoning for a dropped socket: drop the dead client so detection
    // resumes after the engine comes back instead of silently staying off.
    if (detectorPromise === pending) detectorPromise = undefined;
    return undefined;
  }
}

/** Names and codes accepted for a configured language, mapped to ISO 639-3 —
 *  the code space the offline detector reports. */
const LANGUAGE_ALIASES: Record<string, string> = {
  english: "eng", en: "eng", eng: "eng",
  chinese: "cmn", mandarin: "cmn", zh: "cmn", zho: "cmn", cmn: "cmn",
  japanese: "jpn", ja: "jpn", jpn: "jpn",
  korean: "kor", ko: "kor", kor: "kor",
  french: "fra", fr: "fra", fra: "fra", fre: "fra",
  german: "deu", de: "deu", deu: "deu", ger: "deu",
  spanish: "spa", es: "spa", spa: "spa",
  portuguese: "por", pt: "por", por: "por",
  russian: "rus", ru: "rus", rus: "rus",
  italian: "ita", it: "ita", ita: "ita",
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

async function translate(
  value: string,
  direction: Direction,
  pair: LanguagePair,
  ctx: ExtensionContext,
  config: TranslationSettings,
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

  try {
    const result = await ctx.modelRegistry.complete(
      model,
      {
        systemPrompt: translationSystemPrompt(direction, pair, reference),
        messages: [{ role: "user", content: source, timestamp: Date.now() }],
      },
      noThinkingOptions(model),
    );
    // `complete()` resolves with a failed message instead of throwing, so an
    // API rejection would otherwise disable translation with no sign of it.
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
    ctx.ui.notify(`Translation skipped: ${error instanceof Error ? error.message : String(error)}`, "warning");
    return undefined;
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
  const translated =
    wanted && translationDecision(await detectLanguage(original), pair)
      ? await translate(original, "output", pair, ctx, config)
      : undefined;
  recordAssistantTurn(ctx, original.trim(), translated);
  if (!translated) return undefined;

  // Cypher has already streamed the original text into its transcript. Send a
  // side-band status event so the harness can replace/append the rendered
  // transcript without replacing Pi's own message history.
  if (process.env.CYPHER_ENGINE_SOCKET && process.env.CYPHER_CHAT_ID) {
    ctx.ui.setStatus(
      "cypher.translation.v1",
      JSON.stringify({
        version: 1,
        text: translated,
        mode: config.outputMode,
      }),
    );
    return undefined;
  }

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

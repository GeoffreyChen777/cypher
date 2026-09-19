/**
 * Cypher Pi translation extension.
 *
 * Translation is deliberately implemented as an extension rather than in the
 * harness bridge: it therefore applies to every Pi entry point (TUI, RPC and
 * SDK sessions) while keeping the main model's tool/thinking transcript intact.
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

function extractJson(text: string): unknown {
  const trimmed = text.trim().replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, "");
  try {
    return JSON.parse(trimmed);
  } catch {
    const start = trimmed.indexOf("{");
    const end = trimmed.lastIndexOf("}");
    if (start >= 0 && end > start) {
      try {
        return JSON.parse(trimmed.slice(start, end + 1));
      } catch {
        return undefined;
      }
    }
    return undefined;
  }
}

async function translate(
  value: string,
  direction: "input" | "output",
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

  const subject = direction === "input" ? "a user's request" : "an assistant's answer";
  const prompt = [
    "You are a precise translation helper.",
    pair.fromName
      ? `Translate ${subject} from ${pair.fromName} into ${pair.toName}.`
      : `Translate ${subject} into ${pair.toName}.`,
    "Return JSON only with this exact shape:",
    '{"shouldTranslate":true,"translation":"..."}',
    pair.fromName
      ? `Set shouldTranslate to false when the text is not primarily in ${pair.fromName}.`
      : `Set shouldTranslate to false when the text is already in ${pair.toName}.`,
    "Preserve Markdown, code fences, inline code, URLs, file paths, identifiers, and formatting.",
    "Do not explain the translation or add commentary.",
    "Text:",
    source,
  ].join("\n");

  try {
    const result = await ctx.modelRegistry.complete(
      model,
      { messages: [{ role: "user", content: prompt, timestamp: Date.now() }] },
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
    const parsed = extractJson(textOf(result.content)) as
      | { shouldTranslate?: boolean; translation?: string }
      | undefined;
    if (!parsed?.shouldTranslate || typeof parsed.translation !== "string") return undefined;
    const translation = parsed.translation.trim();
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
  if (!config.translateUserMessages || !config.translationModel.trim()) return;
  const pair = inputPair(config);
  if (!translationDecision(detected, pair)) return;
  const translated = await translate(event.text, "input", pair, ctx, config);
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
  if (
    !config.translateFinalResponses ||
    !config.translationModel.trim() ||
    !enabledForSession(ctx, config) ||
    message.role !== "assistant" ||
    hasToolCall(message)
  ) {
    return undefined;
  }
  const assistant = message as AssistantMessage;
  const original = responseText(assistant);
  if (!original.trim()) return undefined;
  const pair = outputPair(config, lastUserLanguage);
  if (!translationDecision(await detectLanguage(original), pair)) return undefined;
  const translated = await translate(original, "output", pair, ctx, config);
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

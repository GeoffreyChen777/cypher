import assert from "node:assert/strict";
import { test } from "node:test";

import {
  languageCode,
  languageName,
  noThinkingOptions,
  translationDecision,
} from "./cypher-translation.ts";

const reliable = (language) => ({ language, confidence: 1, reliable: true });

// The offline detector reports ISO 639-3 (`cmn`, `eng`, `nld`), so a configured
// language only gates anything once it has been mapped into that code space.
test("configured languages map onto the detector's ISO 639-3 codes", () => {
  assert.equal(languageCode("Chinese"), "cmn");
  assert.equal(languageCode("  english "), "eng");
  assert.equal(languageCode("ja"), "jpn");
});

test("auto, blank and unknown languages carry no local decision", () => {
  for (const value of ["auto", "AUTO", "", "   "]) {
    assert.equal(languageCode(value), undefined, `${JSON.stringify(value)} must not gate`);
  }
  // Regression: an unmapped name used to be compared raw against an ISO code,
  // so it could never match and silently disabled translation altogether.
  assert.equal(languageCode("Dutch"), undefined);
});

test("detected codes can be named for the prompt", () => {
  assert.equal(languageName("cmn"), "Chinese");
  assert.equal(languageName("nld"), "Dutch");
  assert.equal(languageName("not-a-code"), "not-a-code");
});

test("an answer in the working language is translated back to the user's language", () => {
  // The round trip that append mode depends on: request went out as English,
  // the answer comes back in the language the user actually writes.
  const pair = { fromCode: "eng", fromName: "English", toCode: "cmn", toName: "Chinese" };
  assert.equal(translationDecision(reliable("eng"), pair), true);
});

test("an answer already in the user's language is left alone", () => {
  const pair = { fromCode: "eng", fromName: "English", toCode: "cmn", toName: "Chinese" };
  assert.equal(translationDecision(reliable("cmn"), pair), false);
});

test("a pair with nothing to translate into never spends a request", () => {
  // `auto` source and no detected user language yet: there is no destination.
  assert.equal(translationDecision(reliable("eng"), { fromCode: "eng", fromName: "English" }), false);
});

test("an explicit origin language only translates that language", () => {
  const pair = { fromCode: "cmn", fromName: "Chinese", toCode: "eng", toName: "English" };
  assert.equal(translationDecision(reliable("cmn"), pair), true);
  assert.equal(translationDecision(reliable("deu"), pair), false);
});

test("an unknown origin language still translates instead of silently disabling", () => {
  const pair = { fromName: "Dutch", toCode: "eng", toName: "English" };
  assert.equal(translationDecision(reliable("nld"), pair), true);
});

test("uncertain or missing detection always falls through to the model", () => {
  const pair = { fromCode: "cmn", fromName: "Chinese", toCode: "eng", toName: "English" };
  assert.equal(translationDecision(undefined, pair), true);
  assert.equal(translationDecision({ language: "eng", confidence: 0.4, reliable: false }, pair), true);
  assert.equal(translationDecision({ reliable: true }, pair), true);
});

// Regression: naming an effort level both turns thinking back on (the adapter
// already applies `thinkingLevelMap.off` when none is given) and forwards an
// unsupported level verbatim, which failed the request with
// `level "minimal" not supported` and silently disabled translation.
test("no effort level is ever requested from the OpenAI families", () => {
  for (const api of [
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
    "openai-completions",
  ]) {
    assert.deepEqual(noThinkingOptions({ api, reasoning: true }), {});
  }
});

test("anthropic is the one family told explicitly not to think", () => {
  assert.deepEqual(noThinkingOptions({ api: "anthropic-messages", reasoning: true }), {
    thinkingEnabled: false,
  });
  // A non-reasoning model has nothing to disable.
  assert.deepEqual(noThinkingOptions({ api: "anthropic-messages", reasoning: false }), {});
});

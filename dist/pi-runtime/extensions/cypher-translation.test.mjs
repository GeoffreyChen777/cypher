import assert from "node:assert/strict";
import { test } from "node:test";

import {
  FramePump,
  languageCode,
  languageName,
  noThinkingOptions,
  referenceBlock,
  renderTranslation,
  translationDecision,
  translationSystemPrompt,
  unwrapPartialTranslation,
  unwrapTranslation,
} from "./cypher-translation.ts";

const reliable = (language) => ({ language, confidence: 1, reliable: true });

// The offline detector reports ISO 639-3 (`cmn`, `eng`, `nld`), so a configured
// language only gates anything once it has been mapped into that code space.
test("configured languages map onto the detector's ISO 639-3 codes", () => {
  assert.equal(languageCode("Chinese"), "cmn");
  assert.equal(languageCode("  english "), "eng");
  // Every spelling of Chinese lands on Mandarin's code, which is what the
  // detector reports for Lingua's Chinese.
  for (const value of ["zh", "zho", "cmn", "Mandarin"]) {
    assert.equal(languageCode(value), "cmn", value);
  }
});

test("auto, blank and unknown languages carry no local decision", () => {
  for (const value of ["auto", "AUTO", "", "   "]) {
    assert.equal(languageCode(value), undefined, `${JSON.stringify(value)} must not gate`);
  }
  // Regression: an unmapped name used to be compared raw against an ISO code,
  // so it could never match and silently disabled translation altogether.
  assert.equal(languageCode("Dutch"), undefined);
  // Languages the detector is no longer built with are unmapped too, including
  // ones the settings card used to offer.
  for (const value of ["Japanese", "French", "Russian"]) {
    assert.equal(languageCode(value), undefined, value);
  }
});

// The detector knows Chinese and English only. Asked about French it does not
// answer "unknown" — French is Latin script, so it comes back as confident
// English, and rule 1 would then skip the message as "already in the
// destination language". A named origin the table cannot map must therefore
// never skip, which is what keeps a language configured before the supported
// set was trimmed translating instead of silently passing through.
test("a configured language the detector cannot judge never skips the model", () => {
  const pair = { fromName: "French", toCode: "eng", toName: "English" };
  assert.equal(translationDecision(reliable("eng"), pair), true);
  assert.equal(translationDecision(reliable("cmn"), pair), true);
  assert.equal(translationDecision(undefined, pair), true);
  // A supported origin still gates normally.
  const supported = { fromCode: "cmn", fromName: "Chinese", toCode: "eng", toName: "English" };
  assert.equal(translationDecision(reliable("eng"), supported), false);
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

const inbound = { fromCode: "cmn", fromName: "Chinese", toCode: "eng", toName: "English" };
const outbound = { fromCode: "eng", fromName: "English", toCode: "cmn", toName: "Chinese" };
const exchange = {
  user: { original: "把接口改成 400", translated: "Change the endpoint to 400" },
  assistant: { original: "The endpoint now returns 400.", translated: "接口现在返回 400。" },
};

// Both sides of a turn are what make the block a translation memory: the pair
// shows which word was already chosen for 接口, so the next turn says endpoint
// again instead of drifting to interface.
test("a quoted turn carries both languages, labelled for the current direction", () => {
  const block = referenceBlock([exchange], "input", inbound);
  assert.equal(block, [
    "User (Chinese): 把接口改成 400",
    "User (English): Change the endpoint to 400",
    "Assistant (English): The endpoint now returns 400.",
    "Assistant (Chinese): 接口现在返回 400。",
  ].join("\n"));
  // Same stored turn, opposite direction: the pair flips, so the labels have to
  // flip with it or the block would claim the English line is Chinese.
  assert.equal(referenceBlock([exchange], "output", outbound), block);
});

test("an untranslated turn still contributes its own side", () => {
  assert.equal(
    referenceBlock([{ user: { original: "重启一下" } }], "input", inbound),
    "User (Chinese): 重启一下",
  );
  assert.equal(referenceBlock([], "input", inbound), undefined);
  assert.equal(referenceBlock([{}], "input", inbound), undefined);
});

test("languages that cannot be named are quoted without a label", () => {
  assert.equal(
    referenceBlock([{ user: { original: "ping" } }], "input", { toName: "English" }),
    "User: ping",
  );
});

test("the block is bounded in turns, in length, and cut on whole exchanges", () => {
  const turn = (n) => ({ user: { original: `turn ${n}` } });
  const block = referenceBlock([turn(1), turn(2), turn(3)], "input", inbound);
  // Oldest first, and no more than the two most recent exchanges.
  assert.equal(block, "User (Chinese): turn 2\nUser (Chinese): turn 3");

  const long = () => ({
    user: { original: "漫".repeat(5_000), translated: "x".repeat(5_000) },
    assistant: { original: "y".repeat(5_000), translated: "码".repeat(5_000) },
  });
  const bounded = referenceBlock([long(), long()], "input", inbound);
  const lines = bounded.split("\n");
  // The newest exchange always fits whole; the older one is dropped entire
  // rather than stranding a term away from its translation.
  assert.equal(lines.length, 4, bounded.slice(0, 80));
  assert.ok(lines.every((line) => line.endsWith("…")), "every quote is clipped");
  assert.ok(bounded.length < 1_600, `${bounded.length} chars`);
});

test("a quoted turn is collapsed to one line", () => {
  const block = referenceBlock(
    [{ user: { original: "first\n\n```sh\nls -la\n```" } }], "input", inbound,
  );
  assert.equal(block, "User (Chinese): first ```sh ls -la ```");
});

// The payload is the whole of the message sent to the model, so the prompt has
// to say what to do with it — and say it without inviting a preamble.
test("the prompt demands the translation and nothing else", () => {
  const prompt = translationSystemPrompt("input", inbound);
  assert.match(prompt, /written in Chinese\. Translate it into English\./);
  assert.match(prompt, /Output the translation itself and nothing else/);
  assert.match(prompt, /no code fence around the answer/);
  // The skip signal that replaced the old `shouldTranslate` field.
  assert.match(prompt, /already in English, output it unchanged/);
  // Nothing to reference: no dangling reference instructions.
  assert.doesNotMatch(prompt, /reference only/);
});

test("an unnamed source language is not asserted in the prompt", () => {
  const prompt = translationSystemPrompt("output", { toName: "Chinese" });
  assert.match(prompt, /The next message is an assistant's answer\. Translate it into Chinese\./);
});

test("reference turns are marked as reference, never as work", () => {
  const prompt = translationSystemPrompt("input", inbound, "User (Chinese): 把接口改成 400");
  assert.match(prompt, /Never translate them, never answer them and never mention them\./);
  assert.ok(prompt.endsWith("User (Chinese): 把接口改成 400"));
});

test("a fence the model wrapped around the whole answer is removed", () => {
  assert.equal(unwrapTranslation("```\nChange the endpoint\n```", "把接口改一下"), "Change the endpoint");
  assert.equal(unwrapTranslation("```markdown\nHello\n```", "你好"), "Hello");
  assert.equal(unwrapTranslation("  Hello  ", "你好"), "Hello");
});

test("a message that is itself a code block keeps its fence", () => {
  const source = "```rust\nlet x = 1; // 一\n```";
  const answer = "```rust\nlet x = 1; // one\n```";
  assert.equal(unwrapTranslation(answer, source), answer);
  // A fence inside the answer is not a wrapper either.
  const mixed = "Run this:\n\n```sh\nls\n```";
  assert.equal(unwrapTranslation(mixed, "运行这个：\n\n```sh\nls\n```"), mixed);
});

// Mid-stream a wrapping fence has no closing line yet, so the strict form
// cannot see it. Publishing the opener anyway would put literal backticks in
// the transcript and then have to take them back.
test("a partial holds back a fence opener until its line is complete", () => {
  assert.equal(unwrapPartialTranslation("`", "你好"), "");
  assert.equal(unwrapPartialTranslation("``", "你好"), "");
  assert.equal(unwrapPartialTranslation("```", "你好"), "");
  assert.equal(unwrapPartialTranslation("```mark", "你好"), "");
  assert.equal(unwrapPartialTranslation("```markdown\nHel", "你好"), "Hel");
  assert.equal(unwrapPartialTranslation("\n\nHel", "你好"), "Hel");
  // Inline code is not a fence, and must not be held back forever.
  assert.equal(unwrapPartialTranslation("`ls`", "你好"), "`ls`");
});

test("a partial of a message that is itself a code block keeps its fence", () => {
  const source = "```rust\nlet x = 1; // 一\n```";
  assert.equal(unwrapPartialTranslation("```rust\nlet", source), "```rust\nlet");
});

// Partials grow by appending, which is what lets the doc writer take its cheap
// trailing-append path instead of diffing the whole answer on every frame.
test("successive partials stay prefixes of one another", () => {
  // A model's output only ever grows, so every transform applied to it has to
  // keep that true — including across the fence and leading-whitespace cases,
  // which are the two that rewrite the head of the text.
  const source = "Change the endpoint";
  for (const stream of [
    ["把", "把接口", "把接口改 ", "把接口改 成", "把接口改 成 400"],
    ["\n", "\n把", "\n把接口"],
    ["`", "``", "```", "```md", "```md\n", "```md\n把", "```md\n把接口"],
    ["`", "``", "``把", "``把接口"],
  ]) {
    let previous = "";
    for (const raw of stream) {
      const partial = unwrapPartialTranslation(raw, source);
      assert.ok(
        partial.startsWith(previous),
        `${JSON.stringify(partial)} must extend ${JSON.stringify(previous)}`,
      );
      previous = partial;
    }
  }
});

test("a frame carries the whole rendering, so append mode never doubles", () => {
  assert.equal(renderTranslation("The answer", "译文", "replace"), "译文");
  assert.equal(
    renderTranslation("The answer", "译文", "append"),
    "The answer\n\n---\n\n译文",
  );
  // Re-rendering the same frame is what a keepalive and a replay both do.
  const once = renderTranslation("The answer", "译文", "append");
  assert.equal(renderTranslation("The answer", "译文", "append"), once);
});

const pump = (frameMs = 150, keepaliveMs = 4_000) => {
  const sent = [];
  let clock = 1_000;
  return {
    sent,
    advance: (ms) => {
      clock += ms;
    },
    pump: new FramePump((text) => sent.push(text), () => clock, frameMs, keepaliveMs),
  };
};

test("the opening frame goes out immediately, whatever the clock reads", () => {
  const { pump: p, sent } = pump();
  p.offer("译");
  assert.deepEqual(sent, ["译"]);
});

test("frames are throttled to the doc commit cadence", () => {
  const { pump: p, sent, advance } = pump();
  p.offer("译");
  p.offer("译文");
  p.offer("译文。");
  assert.deepEqual(sent, ["译"], "a burst inside one window costs one frame");
  advance(150);
  // The idle tick is what publishes a change that no later delta followed.
  p.tick();
  assert.deepEqual(sent, ["译", "译文。"], "and it is the newest rendering that goes");
});

// The reason the pump exists: a silent stream is a parked turn, and a parked
// turn drops the translation it was waiting for.
test("an unchanged frame is re-sent on the keepalive", () => {
  const { pump: p, sent, advance } = pump();
  p.offer("译文");
  advance(150);
  p.tick();
  assert.deepEqual(sent, ["译文"], "nothing changed, so the frame window is not enough");
  advance(4_000);
  p.tick();
  assert.deepEqual(sent, ["译文", "译文"], "the keepalive re-states it");
});

test("the last frame lands whatever the pacing rules would have said", () => {
  const { pump: p, sent } = pump();
  p.offer("译");
  p.flush("译文。");
  assert.deepEqual(sent, ["译", "译文。"]);
  // A failed translation puts the original back through the same door.
  p.flush("The answer");
  assert.deepEqual(sent, ["译", "译文。", "The answer"]);
});

test("a pump with nothing to say never publishes", () => {
  const { pump: p, sent, advance } = pump();
  p.tick();
  advance(10_000);
  p.tick();
  p.flush();
  assert.deepEqual(sent, []);
});

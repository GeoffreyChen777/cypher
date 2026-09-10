import { test } from "node:test";
import assert from "node:assert/strict";
import { inspectPatch, inspectSource, languageForPath, validateContext, MAX_BYTES, MAX_LINES } from "../src/policy.mjs";
import { parsePatchFiles } from "@pierre/diffs";

const header = "diff --git a/file b/file\n";
test("bounded patches keep highlighting; unknown languages stay plain", () => {
  assert.equal(inspectPatch(header + "@@ -1 +1 @@\n-old\n+new\n").plain, false);
  assert.equal(languageForPath("dir/example.swift"), "swift");
  assert.equal(languageForPath("dir/unknown.weird"), "text");
  assert.equal(languageForPath("constructor"), "text");
  assert.equal(languageForPath("__proto__"), "text");
});
test("large input and hostile hunk counts never enter the renderer", () => {
  for (const patch of [header + "x".repeat(MAX_BYTES), header + "\n".repeat(MAX_LINES),
                       header + "@@ -1,99999999 +1,99999999 @@\n"]) {
    assert.throws(() => inspectPatch(patch), /large/);
  }
  assert.throws(() => inspectPatch("not a patch"), /invalid/);
});
test("long lines and thousands of lines turn off expensive token work", () => {
  assert.equal(inspectPatch(header + "+" + "x".repeat(2_001)).plain, true);
  assert.equal(inspectPatch(header + " x\n".repeat(4_001)).plain, true);
});

test("full documents must match hunks and every unchanged gap", () => {
  const file = parsePatchFiles(header + "--- a/file\n+++ b/file\n@@ -2 +2 @@\n-old\n+new\n")[0].files[0];
  assert.doesNotThrow(() => validateContext(file, "prefix\nold\ntail\n", "prefix\nnew\ntail\n"));
  assert.doesNotThrow(() => validateContext(file, "prefix\r\nold\r\ntail\r\n", "prefix\nnew\ntail\n"));
  assert.throws(() => validateContext(file, "prefix\nold\ntail\n", "other\nnew\ntail\n"), /stale/);
  assert.throws(() => validateContext(file, "prefix\nold\ntail\n", "prefix\nwrong\ntail\n"), /stale/);
  assert.throws(() => validateContext(file, "prefix\nold\ntail\n", "prefix\nnew\nother\n"), /stale/);
  assert.throws(() => validateContext(file, null, "prefix\nnew\ntail\n"), /unavailable/);
});

test("zero-context insertion boundaries are not off by one", () => {
  const file = parsePatchFiles(header + "--- a/file\n+++ b/file\n@@ -2,0 +3 @@\n+new\n")[0].files[0];
  assert.doesNotThrow(() => validateContext(file, "a\nb\n", "a\nb\nnew\n"));
});

test("source limits count real lines and degrade expensive highlighting", () => {
  assert.deepEqual(inspectSource(""), { bytes: 0, lines: 0, plain: false });
  assert.equal(inspectSource("let x = 1\n").lines, 1);
  assert.equal(inspectSource("你好\n").bytes, 7);
  assert.equal(inspectSource("x".repeat(2001)).plain, true);
  assert.equal(inspectSource("x\n".repeat(4001)).plain, true);
  assert.equal(inspectSource("x\n".repeat(MAX_LINES)).lines, MAX_LINES);
  assert.throws(() => inspectSource("x\n".repeat(MAX_LINES + 1)), /large/);
  assert.throws(() => inspectSource("🌿".repeat(MAX_BYTES / 4 + 1)), /large/);
  assert.throws(() => inspectSource(null), /invalid/);
});

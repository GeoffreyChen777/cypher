import { describe, expect, it } from "vitest";
import vectors from "./fixtures/stream-preview-v1.json";
import { decodeFrame, encodeFrame } from "./chat-frames";
import type { FrameType } from "./chat-frames";
import { decodeStream, encodeStream, STREAM, MAX_STREAM_TEXT_BYTES, MAX_STREAM_FRAME_BYTES } from "./stream-preview";

const header = { chatId: "c", runId: "r", segmentId: "s", epoch: "e", revision: 0, baseSeq: 0 };
describe("inactive ephemeral stream v1 codec", () => {
  for (const v of vectors) it(v.name, () => {
    const bytes = v.hex !== undefined
      ? Uint8Array.from(v.hex.match(/../g) ?? [], hex => parseInt(hex, 16))
      : encodeFrame(v.kind as FrameType, v.header!, new TextEncoder().encode(v.text!));
    const decoded = decodeStream(bytes);
    expect(!!decoded).toBe(v.valid);
    if (decoded) {
      expect(decodeStream(encodeStream(decoded.kind, decoded.header, decoded.text)!)).toEqual(decoded);
      // New protocol must NOT be accepted by the active production dispatcher.
      expect(decodeFrame(bytes)).toBeUndefined();
    }
  });

  it("enforces byte limits, strict UTF-8, and ID limits", () => {
    expect(encodeStream(STREAM.snapshot, header, "x".repeat(MAX_STREAM_TEXT_BYTES))).toBeDefined();
    expect(encodeStream(STREAM.snapshot, header, "界".repeat(MAX_STREAM_TEXT_BYTES / 3 + 1))).toBeUndefined();
    expect(decodeStream(new Uint8Array(MAX_STREAM_FRAME_BYTES + 1))).toBeUndefined();
    expect(decodeStream(encodeFrame(STREAM.snapshot as FrameType, header, new Uint8Array([0xff])))).toBeUndefined();
    expect(encodeStream(STREAM.snapshot, { ...header, epoch: "x".repeat(128) })).toBeDefined();
    expect(encodeStream(STREAM.snapshot, { ...header, epoch: "x".repeat(129) })).toBeUndefined();
    expect(encodeStream(STREAM.snapshot, { ...header, epoch: "x".repeat(4096) })).toBeUndefined();
    expect(encodeStream(0x121, header)).toBeUndefined();
    expect(encodeStream(STREAM.snapshot, header, "\ud800")).toBeUndefined();
  });
});

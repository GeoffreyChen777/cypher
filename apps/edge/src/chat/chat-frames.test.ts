import { describe, expect, it } from "vitest";
import vectorFile from "../../../../protocol/vectors/chat-frames-v1.json";
import { decodeFrame, encodeFrame, FRAME, MAX_HEADER_BYTES, type FrameType } from "./chat-frames";
import { bytesOf, fromHex, toHex } from "../../test/support/bytes";

/** The chat2 wire codec is a cross-language contract: the Rust and Swift
 * clients run the same vectors (protocol/vectors/chat-frames-v1.json,
 * protocol/README.md). */

interface FrameVector {
  name: string;
  type: number;
  header: Record<string, unknown>;
  payload: string;
  hex: string;
}

interface Vectors {
  maxHeaderBytes: number;
  types: Record<string, number>;
  encode: FrameVector[];
  malformed: { name: string; hex: string }[];
  unknownType: FrameVector[];
  headerSize: { name: string; bytes: number; valid: boolean }[];
}

const vectors = vectorFile as Vectors;

describe("chat2 frame codec: shared vectors", () => {
  it("frame type bytes and the header limit", () => {
    expect(FRAME).toStrictEqual(vectors.types);
    expect(MAX_HEADER_BYTES).toBe(vectors.maxHeaderBytes);
  });

  it.each(vectors.encode)("encodes and decodes: $name", (v) => {
    const frame = encodeFrame(v.type as FrameType, v.header, fromHex(v.payload));
    expect(toHex(frame)).toBe(v.hex);
    const decoded = decodeFrame(frame);
    expect(decoded?.type).toBe(v.type);
    expect(decoded?.header).toStrictEqual(v.header);
    expect(toHex(decoded?.payload ?? new Uint8Array())).toBe(v.payload);
  });

  it.each(vectors.malformed)("rejects malformed: $name", (v) => {
    expect(decodeFrame(fromHex(v.hex))).toBeUndefined();
  });

  // The DO rejects frame types it does not know; the clients decode them.
  it.each(vectors.unknownType)("rejects an unknown type: $name", (v) => {
    expect(decodeFrame(fromHex(v.hex))).toBeUndefined();
  });

  it.each(vectors.headerSize)("header size limit: $name", (v) => {
    // `{"pad":""}` is 10 bytes; the pad fills the header to `bytes`.
    const frame = encodeFrame(FRAME.hello, { pad: "x".repeat(v.bytes - 10) });
    expect(decodeFrame(frame) !== undefined).toBe(v.valid);
  });
});

describe("chat2 frame codec", () => {
  it("round-trips every frame type, with and without payload", () => {
    for (const type of Object.values(FRAME)) {
      const payload = bytesOf(1000, type);
      const decoded = decodeFrame(encodeFrame(type, { seq: 7, device: "dev-a" }, payload));
      expect(decoded).toBeDefined();
      expect(decoded!.type).toBe(type);
      expect(decoded!.header).toEqual({ seq: 7, device: "dev-a" });
      expect(decoded!.payload).toEqual(payload);

      const bare = decodeFrame(encodeFrame(type, {}));
      expect(bare!.payload.length).toBe(0);
    }
  });

  it("round-trips a subarray view (offset ≠ 0 — the ws buffer case)", () => {
    const inner = encodeFrame(FRAME.row, { seq: 1 }, bytesOf(64, 3));
    const shifted = new Uint8Array(inner.length + 8);
    shifted.set(inner, 8);
    const decoded = decodeFrame(shifted.subarray(8));
    expect(decoded?.header).toEqual({ seq: 1 });
    expect(decoded?.payload).toEqual(bytesOf(64, 3));
  });
});

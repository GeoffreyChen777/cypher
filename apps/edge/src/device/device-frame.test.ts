import { describe, expect, it } from "vitest";
import vectorFile from "../../../../protocol/vectors/device-frames-v1.json";
import { decodeDeviceFrame, encodeDeviceFrame, type DeviceFrameHeader } from "./device-frame";
import { fromHex, toHex } from "../../test/support/bytes";

// The codec runs the shared vectors in protocol/vectors/device-frames-v1.json,
// which the Rust and Swift codecs run too (protocol/README.md).
interface Vectors {
  frames: { name: string; header: DeviceFrameHeader; json: string; payload: string; hex: string }[];
  malformed: { name: string; hex: string }[];
}

const vectors = vectorFile as Vectors;

describe("device frame codec: shared vectors", () => {
  it.each(vectors.frames)("encodes and decodes: $name", (v) => {
    expect(JSON.stringify(v.header)).toBe(v.json);
    const frame = encodeDeviceFrame(v.header, fromHex(v.payload));
    expect(toHex(frame)).toBe(v.hex);
    const decoded = decodeDeviceFrame(frame);
    expect(decoded.header).toStrictEqual(v.header);
    expect(toHex(decoded.payload)).toBe(v.payload);
  });

  it.each(vectors.malformed)("rejects malformed: $name", (v) => {
    expect(() => decodeDeviceFrame(fromHex(v.hex))).toThrow();
  });
});

describe("device frame codec", () => {
  it("rejects a sixth length byte as a bad length, never wrapping into a small one", () => {
    expect(() => decodeDeviceFrame(Uint8Array.of(0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x7b, 0x7d))).toThrow(
      "bad frame length"
    );
  });
});

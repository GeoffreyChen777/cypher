import { describe, expect, it } from "vitest";
import vectors from "../../../../protocol/vectors/device-trust-v1.json";
import { entryHash, parseEntry, replay, signingBytes, toHex } from "./trust-chain";

// The shared vectors in protocol/vectors/device-trust-v1.json, which the Rust
// and Swift implementations run too (protocol/README.md).
describe("device trust chain vectors", () => {
  it("encodes and hashes every entry byte-for-byte like Rust", async () => {
    for (const value of vectors.chain.entries) {
      const entry = parseEntry(value);
      expect(entry).toBeDefined();
      expect(toHex(signingBytes(entry!))).toBe(value.signingBytes);
      expect(await entryHash(entry!)).toBe(value.hash);
    }
  });

  it("replays the valid chain to its members", async () => {
    const state = await replay(vectors.chain.entries);
    expect(typeof state).not.toBe("string");
    if (typeof state === "string") return;
    expect([...state.members.keys()].sort()).toEqual(vectors.chain.members);
    expect([...state.removed]).toEqual(vectors.chain.removed);
    expect(state.requirePaired).toBe(vectors.chain.requirePaired);
    expect(state.head).toBe(vectors.chain.entries.at(-1)!.hash);
  });

  for (const invalid of vectors.invalidChains) {
    it(`refuses ${invalid.name}`, async () => {
      expect(await replay(invalid.entries)).toBe(invalid.reason);
    });
  }

  it("refuses malformed wire shapes", () => {
    const entry = vectors.chain.entries[0]!;
    expect(parseEntry({ ...entry, sig: entry.sig.toUpperCase() })).toBeUndefined();
    expect(parseEntry({ ...entry, seq: -1 })).toBeUndefined();
    expect(parseEntry({ ...entry, kind: "promote" })).toBeUndefined();
    expect(parseEntry({ ...entry, device: { ...entry.device, name: "a\nb" } })).toBeUndefined();
    expect(parseEntry({ ...entry, device: { ...entry.device, id: "x".repeat(129) } })).toBeUndefined();
  });
});

import { describe, expect, it } from "vitest";
import vectorFile from "../../../../protocol/vectors/registry-core-v1.json";
import {
  applyOp,
  encodeHlc,
  hlcNewer,
  maxClock,
  rowToSeedOp,
  validateOp,
  type Op,
  type Row
} from "./registry-core";

// The merge cases are the shared vectors in protocol/vectors/registry-core-v1.json,
// run by the Rust and Swift mirrors too (protocol/README.md).
interface Vectors {
  encodeHlc: { name: string; ms: number; counter: number; device: string; hlc: string }[];
  hlcNewer: { name: string; a: string; b: string | null; newer: boolean }[];
  applyOp: { name: string; row: Row | null; steps: { op: Op; changed: boolean; row?: Row }[] }[];
  maxClock: { name: string; row: Row; hlc: string | null }[];
  rowToSeedOp: { name: string; row: Row; op: Op }[];
  convergence: { name: string; prefix: Op[]; permute: Op[]; row: Row }[];
}

const vectors = vectorFile as unknown as Vectors;

const permutations = <T>(items: T[]): T[][] =>
  items.length === 0
    ? [[]]
    : items.flatMap((item, i) =>
        permutations([...items.slice(0, i), ...items.slice(i + 1)]).map((rest) => [item, ...rest])
      );

describe("shared vectors", () => {
  it.each(vectors.encodeHlc)("encodeHlc: $name", (v) => {
    expect(encodeHlc(v.ms, v.counter, v.device)).toBe(v.hlc);
  });

  it.each(vectors.hlcNewer)("hlcNewer: $name", (v) => {
    expect(hlcNewer(v.a, v.b ?? undefined)).toBe(v.newer);
  });

  it.each(vectors.applyOp)("applyOp: $name", (v) => {
    let row = v.row ?? undefined;
    v.steps.forEach((step, i) => {
      const result = applyOp(row, step.op);
      expect(result.changed, `step ${i}: changed`).toBe(step.changed);
      expect(result.row, `step ${i}: row`).toStrictEqual(step.changed ? step.row : row);
      row = result.row;
    });
  });

  it.each(vectors.maxClock)("maxClock: $name", (v) => {
    expect(maxClock(v.row)).toBe(v.hlc ?? undefined);
  });

  it.each(vectors.rowToSeedOp)("rowToSeedOp: $name", (v) => {
    const op = rowToSeedOp(v.row);
    expect(op).toStrictEqual(v.op);
    expect(applyOp(undefined, op)).toStrictEqual({ row: v.row, changed: true });
  });

  it.each(vectors.convergence)("convergence: $name", (v) => {
    for (const order of permutations(v.permute)) {
      let row: Row | undefined;
      for (const op of [...v.prefix, ...order]) row = applyOp(row, op).row ?? row;
      expect(row).toStrictEqual(v.row);
    }
  });
});

describe("validateOp", () => {
  const hlc = (ms: number) => encodeHlc(ms, 0, "dev-a");
  const upsert = (over: Partial<Op> = {}): Op => ({
    kind: "chats",
    id: "chat-1",
    op: "upsert",
    set: { title: "hello", archived: false },
    hlc: hlc(1000),
    ...over
  });

  it("accepts well-formed ops and rejects malformed ones", () => {
    expect(validateOp(upsert())).toBeNull();
    expect(validateOp({ ...upsert(), kind: "Nope Kind" })).toMatch(/kind/);
    expect(validateOp({ ...upsert(), id: "" })).toMatch(/id/);
    expect(validateOp({ ...upsert(), op: "merge" as unknown as Op["op"] })).toMatch(/op/);
    expect(validateOp({ ...upsert(), hlc: "not-a-clock" })).toMatch(/hlc/);
    expect(validateOp({ ...upsert(), set: undefined })).toMatch(/set/);
    expect(validateOp({ ...upsert(), set: { "bad field!": 1 } })).toMatch(/field/);
    expect(validateOp({ kind: "chats", id: "c", op: "delete", hlc: hlc(1), set: {} })).toMatch(/delete/);
    expect(validateOp({ ...upsert(), clocks: { title: "junk" } })).toMatch(/clock/);
    const huge = upsert({ set: { blob: "x".repeat(20_000) } });
    expect(validateOp(huge)).toMatch(/large/);
  });
});

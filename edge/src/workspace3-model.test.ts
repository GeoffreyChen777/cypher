import { describe, it, expect } from "vitest";
import { applyOp, validateOp, type Op } from "./workspace3-model";

const op = (n: number, kind: Op["op"], set?: Op["set"]): Op =>
  ({ kind: "chats", id: "chat", op: kind, hlc: `0000000000001-${String(n).padStart(6, "0")}-host`, ...(set ? { set } : {}) });

describe("native metadata", () => {
  it("has no clock-override/reseed or coercing admission", () => {
    const valid = op(1, "upsert", { title: "first" });
    expect(validateOp(valid)).toBeNull();
    for (const extra of [{ clocks: {} }, { clocks: null }, { reseed: true }, { id: 1 }, { hlc: 1 }, { kind: "sessions" }]) {
      expect(validateOp({ ...valid, ...extra } as unknown as Op)).not.toBeNull();
    }
  });
  it("preserves deletes, immutable equal-clock writes and fieldwise convergence", () => {
    const first = applyOp(undefined, op(1, "upsert", { title: "first", cwd: "/work" })).row!;
    expect(applyOp(first, op(1, "update", { title: "changed" }))).toEqual({ row: first, changed: false });
    const edited = applyOp(first, op(3, "update", { title: "new", cwd: null })).row!;
    expect(edited.fields).toEqual({ title: "new" });
    expect(applyOp(edited, op(2, "delete")).changed).toBe(false);
    const gone = applyOp(edited, op(4, "delete")).row!;
    expect(applyOp(gone, op(5, "update", { title: "wrong" })).changed).toBe(false);
    expect(applyOp(gone, op(4, "upsert", { title: "old" })).changed).toBe(false);
    const revived = applyOp(gone, op(5, "upsert", { title: "revived" })).row!;
    expect(revived.deleted).toBe(false);
    expect(revived.fields).toEqual({ title: "revived" });
    expect(revived.delHlc).toBe(gone.delHlc);
    const a = op(1, "upsert", { title: "A" }), b = op(2, "upsert", { archived: true });
    expect(applyOp(applyOp(undefined, a).row, b).row).toEqual(applyOp(applyOp(undefined, b).row, a).row);
  });
});

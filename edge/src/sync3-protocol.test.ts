import { describe, expect, it } from "vitest";
import golden from "../../fixtures/sync3/golden.json";
import invalid from "../../fixtures/sync3/invalid.json";
import numbers from "../../fixtures/sync3/numbers.json";
import {
  applyOperation, canonical, parseRequest, validateOperation,
  type EntityKind, type Projection, type ProjectionStore,
} from "./sync3-protocol";

function memory(): { projection: Projection; store: ProjectionStore } {
  const projection: Projection = { commands: {}, runs: {}, messages: {}, tools: {}, inputs: {} };
  const store: ProjectionStore = {
    get<K extends EntityKind>(kind: K, id: string): Projection[K][string] | undefined {
      return Object.hasOwn(projection[kind], id) ? projection[kind][id] as Projection[K][string] : undefined;
    },
    set<K extends EntityKind>(kind: K, id: string, value: Projection[K][string]): void {
      Object.defineProperty(projection[kind], id, { value, configurable: true, writable: true, enumerable: true });
    },
    hasAcceptedRun: run => Object.values(projection.commands).some(c => c.runId === run),
  };
  return { projection, store };
}

describe("sync3 shared contract", () => {
  it("uses the shared canonical JSON number domain", () => {
    // Receipt identity is canonical JSON bytes, not Object.is(-0, 0).
    expect(canonical(validateOperation(numbers.source))).toBe(canonical(validateOperation(numbers.canonical)));
  });
  it("rejects the shared invalid vectors", () => {
    for (const operation of invalid) expect(() => validateOperation(operation)).toThrow();
  });
  it("reduces the Rust/Swift fixture including UTF-8 byte offsets", () => {
    const { projection, store } = memory();
    for (const value of golden.operations) applyOperation(store, validateOperation(value), "host", 1);
    expect(projection).toEqual(golden.projection);
  });
  it("fences wrong actors and stale owners before mutation", () => {
    const { projection, store } = memory();
    applyOperation(store, validateOperation(golden.operations[0]), "host", 1);
    expect(() => applyOperation(store, { ...validateOperation(golden.operations[1]), actor: "phone" }, "host", 1)).toThrow("not_owner");
    expect(() => applyOperation(store, validateOperation(golden.operations[1]), "host", 2)).toThrow("stale_owner_epoch");
    expect(projection.commands.command.runId).toBeNull();
  });
  it("rejects unknown versions, extra fields, unsafe integers and malformed Unicode", () => {
    expect(() => parseRequest('{"version":2,"type":"probe"}')).toThrow("upgrade_required");
    expect(() => parseRequest('{"version":3,"type":"probe","extra":1}')).toThrow("invalid_shape");
    expect(() => parseRequest('{"version":3,"type":"hello","actor":"a","epoch":1,"after":9007199254740992}')).toThrow("invalid_cursor");
    expect(() => validateOperation({ ...golden.operations[4], event: { type: "textAppended", messageId: "message", offset: 0, text: "\ud800" } })).toThrow("invalid_unicode");
    expect(() => parseRequest('{"version":3,"type":"push","operations":[]}')).toThrow("invalid_batch");
  });
  it("identity is stable under key order and proto-like IDs are ordinary data", () => {
    expect(canonical({ b: 1, a: [2, 3] })).toBe(canonical({ a: [2, 3], b: 1 }));
    const { store, projection } = memory();
    const op = validateOperation({ ...golden.operations[0], event: { ...golden.operations[0].event, commandId: "__proto__" } });
    applyOperation(store, op, "host", 1);
    expect(Object.keys(projection.commands)).toEqual(["__proto__"]);
    expect(Object.getPrototypeOf(projection.commands)).toBe(Object.prototype);
  });
  it("does not append using character offsets or write after terminal outcome", () => {
    const { store } = memory();
    for (const v of golden.operations.slice(0, 5)) applyOperation(store, validateOperation(v), "host", 1);
    const wrong = validateOperation({ ...golden.operations[5], event: { ...golden.operations[5].event, offset: 2 } });
    expect(() => applyOperation(store, wrong, "host", 1)).toThrow("text_offset_mismatch");
    for (const v of golden.operations.slice(5)) applyOperation(store, validateOperation(v), "host", 1);
    expect(() => applyOperation(store, validateOperation({ ...golden.operations[5], id: "late" }), "host", 1)).toThrow("run_not_live");
  });
});

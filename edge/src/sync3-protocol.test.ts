import { describe, expect, it } from "vitest";
import golden from "../../fixtures/sync3/golden.json";
import invalid from "../../fixtures/sync3/invalid.json";
import numbers from "../../fixtures/sync3/numbers.json";
import commands from "../../fixtures/sync3/command-validation.json";
import lifecycle from "../../fixtures/sync3/command-lifecycle.json";
import systemMessage from "../../fixtures/sync3/system-message.json";
import partShapes from "../../fixtures/sync3/part-validation.json";
import {
  applyOperation as applyCommitted, canonical, parseRequest, validateOperation,
  type EntityKind, type Projection, type ProjectionStore, type Operation,
} from "./sync3-protocol";

const heads = new WeakMap<ProjectionStore, number>();
function applyOperation(store: ProjectionStore, op: Operation, owner: string, epoch: number): void {
  const seq = (heads.get(store) ?? 0) + 1;
  applyCommitted(store, op, owner, epoch, seq);
  heads.set(store, seq);
}

function memory(): { projection: Projection; store: ProjectionStore } {
  const projection: Projection = { commands: {}, runs: {}, messages: {}, attachments: {} };
  const store: ProjectionStore = {
    get<K extends EntityKind>(kind: K, id: string): Projection[K][string] | undefined {
      return Object.hasOwn(projection[kind], id) ? projection[kind][id] as Projection[K][string] : undefined;
    },
    set<K extends EntityKind>(kind: K, id: string, value: Projection[K][string]): void {
      Object.defineProperty(projection[kind], id, { value, configurable: true, writable: true, enumerable: true });
    },
    hasAcceptedRun: run => Object.values(projection.commands).some(c => c.runId === run && ["pending", "applied"].includes(c.command.status)),
    hasOpenMessage: run => Object.values(projection.messages).some(m => m.runId === run && m.entry.status === "streaming"),
  };
  return { projection, store };
}

describe("sync3 shared contract", () => {
  it("preserves complete parts and rejects private inputs and unknown nested properties", () => {
    for (const [group, valid] of [[partShapes.valid, true], [partShapes.invalid, false]] as const) {
      for (const part of group) {
        const op = { id: "part-op", actor: "host", ownerEpoch: 1, event: { type: "partPut", messageId: "message", index: 0, part } };
        if (valid) expect(validateOperation(op)).toEqual(op);
        else expect(() => validateOperation(op)).toThrow();
      }
    }
  });
  it("validates all complete command payloads without discarding unknown properties", () => {
    for (const payload of commands.payloads) {
      const op = structuredClone(golden.operations[0]);
      expect(() => validateOperation({ ...op, event: { ...op.event, command: { ...op.event.command, payload } } })).not.toThrow();
    }
    for (const edit of commands.invalid) {
      const op = structuredClone(golden.operations[0]);
      let parent = op.event.command! as unknown as Record<string, unknown>;
      for (const key of edit.path.slice(0, -1)) parent = parent[key] as Record<string, unknown>;
      const key = edit.path.at(-1)!;
      if ("remove" in edit && edit.remove) delete parent[key];
      else if ("value" in edit) parent[key] = edit.value;
      expect(() => validateOperation(op), JSON.stringify(edit)).toThrow();
    }
    const op = structuredClone(golden.operations[0]);
    expect(() => validateOperation({ ...op, event: { ...op.event, command: {
      ...op.event.command, payload: { kind: "interrupt" }, basedOn: null,
    } } })).toThrow("interrupt_requires_target");
  });
  it.each(lifecycle)("enforces command lifecycle: $name", scenario => {
    const { projection, store } = memory();
    for (const value of golden.operations.slice(0, "initialPrefix" in scenario ? scenario.initialPrefix : 1)) {
      applyOperation(store, validateOperation(value), "host", 1);
    }
    scenario.steps.forEach((step, index) => {
      const op = validateOperation({ id: `step-${index}`, actor: step.actor, ownerEpoch: "ownerEpoch" in step ? step.ownerEpoch : 1, event: step.event });
      const before = structuredClone(projection);
      if ("error" in step && step.error) {
        expect(() => applyOperation(store, op, "host", 1)).toThrow(step.error);
        expect(projection).toEqual(before);
      } else applyOperation(store, op, "host", 1);
    });
    expect(projection.commands.command.command.status).toBe(scenario.status);
    if ("acceptedOpId" in scenario) expect(projection.commands.command.acceptedOpId).toBe(scenario.acceptedOpId);
  });
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
  it("retains system roles and continuation identities", () => {
    const { projection, store } = memory();
    for (const value of golden.operations.slice(0, 3)) applyOperation(store, validateOperation(value), "host", 1);
    applyOperation(store, validateOperation(systemMessage), "host", 1);
    expect(projection.messages["system-message#c1"].entry.role).toBe("system");
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
    expect(() => validateOperation({ ...golden.operations[4], event: { type: "textAppended", messageId: "message", partId: "text", offset: 0, text: "\ud800" } })).toThrow("invalid_unicode");
    expect(() => parseRequest('{"version":3,"type":"push","operations":[]}')).toThrow("invalid_batch");
  });
  it("identity is stable under key order and proto-like IDs are ordinary data", () => {
    expect(canonical({ b: 1, a: [2, 3] })).toBe(canonical({ a: [2, 3], b: 1 }));
    const { store, projection } = memory();
    const first = structuredClone(golden.operations[0]);
    first.event.command!.id = "__proto__";
    const op = validateOperation({ ...first, event: { ...first.event, commandId: "__proto__" } });
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

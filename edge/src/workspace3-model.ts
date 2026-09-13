/** Native metadata only. No registry transport, reseed or clock overrides. */
export type FieldValue = string | number | boolean | null | FieldValue[] | { [k: string]: FieldValue };
export interface Row {
  kind: string; id: string; seq: number; deleted: boolean; delHlc?: string;
  fields: Record<string, FieldValue>; clocks: Record<string, string>;
}
export interface Op {
  kind: string; id: string; op: "upsert" | "update" | "delete";
  set?: Record<string, FieldValue>; hlc: string;
}
export function validateOp(op: Op): string | null {
  if (typeof op.kind !== "string" || !["devices", "spaces", "chats"].includes(op.kind)) return "bad kind";
  if (typeof op.id !== "string" || !/^[A-Za-z0-9_.:@/-]{1,256}$/.test(op.id)) return "bad id";
  if (typeof op.hlc !== "string" || !/^\d{13}-\d{6}-[A-Za-z0-9_-]{1,128}$/.test(op.hlc)) return "bad hlc";
  if (!["upsert", "update", "delete"].includes(op.op)) return "bad operation";
  if (Object.keys(op).some(k => !["kind", "id", "op", "set", "hlc"].includes(k))) return "unknown field";
  if (op.op === "delete") return op.set === undefined ? null : "delete carries fields";
  if (!op.set || typeof op.set !== "object" || Array.isArray(op.set)) return "missing fields";
  if (Object.keys(op.set).some(k => !/^[A-Za-z][A-Za-z0-9]{0,63}$/.test(k) ||
      ["constructor", "prototype", "__proto__"].includes(k))) return "bad field";
  return null;
}
const newer = (a: string, b?: string) => b === undefined || a > b;
export function applyOp(row: Row | undefined, op: Op): { row: Row | undefined; changed: boolean } {
  if (op.op === "delete") {
    const previous = row?.deleted ? row.delHlc :
      [row?.delHlc, ...Object.values(row?.clocks ?? {})].filter((s): s is string => s !== undefined).sort().at(-1);
    if (row && !newer(op.hlc, previous)) return { row, changed: false };
    return { row: { kind: op.kind, id: op.id, seq: row?.seq ?? 0, deleted: true,
      delHlc: op.hlc, fields: {}, clocks: {} }, changed: true };
  }
  if ((!row && op.op === "update") || (row?.deleted && (op.op === "update" || !newer(op.hlc, row.delHlc)))) {
    return { row, changed: false };
  }
  const base: Row = row && !row.deleted ? { ...row, fields: { ...row.fields }, clocks: { ...row.clocks } } :
    { kind: op.kind, id: op.id, seq: row?.seq ?? 0, deleted: false,
      ...(row?.delHlc === undefined ? {} : { delHlc: row.delHlc }), fields: {}, clocks: {} };
  let changed = !row || row.deleted;
  for (const [key, value] of Object.entries(op.set ?? {})) {
    if (!newer(op.hlc, base.clocks[key])) continue;
    if (value === null) delete base.fields[key]; else base.fields[key] = value;
    base.clocks[key] = op.hlc;
    changed = true;
  }
  return { row: base, changed };
}

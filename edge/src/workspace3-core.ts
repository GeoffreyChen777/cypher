/** Fresh account-scoped native metadata. No old protocol or storage import. */
import { applyOp, validateOp, type Op, type Row } from "./workspace3-model";

export const HUB_FRAME_BYTES = 256 * 1024;
export const HUB_ROW_BYTES = 64 * 1024;
export const HUB_PUSH_OPS = 3;
export const HUB_PAGE_ROWS = 32;
const encoder = new TextEncoder();
export const bytes = (value: unknown): number => encoder.encode(JSON.stringify(value)).length;
export class HubError extends Error {}
export function fail(code: string): never { throw new HubError(code); }
export function id(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}
export function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
export function keys(value: Record<string, unknown>, required: string[], optional: string[] = []): void {
  if (required.some(k => !Object.hasOwn(value, k)) ||
      Object.keys(value).some(k => !required.includes(k) && !optional.includes(k))) fail("invalid_fields");
}
export function integer(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
export function decode(text: string): Record<string, unknown> {
  if (encoder.encode(text).length > HUB_FRAME_BYTES) fail("frame_too_large");
  let value: unknown;
  try { value = JSON.parse(text); } catch { fail("invalid_json"); }
  const pending: [unknown, number][] = [[value, 0]];
  let visited = 0;
  while (pending.length) {
    const [item, depth] = pending.pop()!;
    if (++visited > 32768 || depth > 32) fail("json_too_complex");
    if (typeof item === "number" && (!Number.isFinite(item) || (Number.isInteger(item) && !Number.isSafeInteger(item)))) fail("invalid_number");
    if (typeof item === "string") {
      for (let i = 0; i < item.length; i++) {
        const code = item.charCodeAt(i);
        if (code >= 0xD800 && code <= 0xDBFF) {
          const low = item.charCodeAt(++i);
          if (!(low >= 0xDC00 && low <= 0xDFFF)) fail("invalid_unicode");
        } else if (code >= 0xDC00 && code <= 0xDFFF) fail("invalid_unicode");
      }
    } else if (Array.isArray(item)) {
      for (const child of item) pending.push([child, depth + 1]);
    } else if (object(item)) {
      for (const [key, child] of Object.entries(item)) {
        pending.push([key, depth + 1], [child, depth + 1]);
      }
    }
  }
  if (!object(value)) fail("invalid_frame");
  if (value.version !== 3) fail("upgrade_required");
  if (typeof value.type !== "string") fail("invalid_frame");
  return value;
}
export function operations(value: unknown, actor: string): Op[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > HUB_PUSH_OPS) fail("invalid_batch");
  return value.map(raw => {
    if (!object(raw)) fail("invalid_operation");
    keys(raw, ["kind", "id", "op", "hlc"], ["set"]);
    if (!["devices", "spaces", "chats"].includes(String(raw.kind))) fail("invalid_kind");
    if (raw.set !== undefined && !object(raw.set)) fail("invalid_fields");
    if (object(raw.set) && Object.hasOwn(raw.set, "id") && raw.set.id !== raw.id) fail("row_identity_mismatch");
    if (object(raw.set) && Object.keys(raw.set).some(k => ["constructor", "prototype", "__proto__"].includes(k))) fail("invalid_fields");
    const op = raw as unknown as Op;
    if (validateOp(op) || bytes(op) > 16 * 1024) fail("invalid_operation");
    if (!op.hlc.endsWith(`-${actor}`)) fail("clock_actor_mismatch");
    if (op.kind === "devices" && op.op !== "delete" && op.id !== actor &&
        !(op.op === "update" && Object.keys(op.set ?? {}).every(k => k === "name"))) fail("not_device_author");
    return op;
  });
}

/** All caller mutations occur in a storage transaction. Seq belongs to one
 * changed row, not a batch: a page boundary cannot lose equal-seq siblings.
 * Tombstones are retained; no silently truncated/resurrected offline state. */
export class HubRows {
  constructor(private readonly sql: SqlStorage) {
    sql.exec("CREATE TABLE IF NOT EXISTS workspace3_rows(kind TEXT NOT NULL,id TEXT NOT NULL,seq INTEGER NOT NULL UNIQUE,body TEXT NOT NULL,PRIMARY KEY(kind,id))");
    sql.exec("CREATE INDEX IF NOT EXISTS workspace3_rows_seq ON workspace3_rows(seq)");
    sql.exec("CREATE TABLE IF NOT EXISTS workspace3_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)");
  }
  meta(key: string): string | undefined {
    return [...this.sql.exec("SELECT value FROM workspace3_meta WHERE key=?", key)][0]?.value as string | undefined;
  }
  setMeta(key: string, value: string): void {
    this.sql.exec("INSERT INTO workspace3_meta VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, value);
  }
  head(): number {
    const head = Number(this.meta("head") ?? 0);
    if (!integer(head)) fail("invalid_workspace_state");
    return head;
  }
  row(kind: string, id: string): Row | undefined {
    const body = [...this.sql.exec("SELECT body FROM workspace3_rows WHERE kind=? AND id=?", kind, id)][0]?.body;
    return body === undefined ? undefined : JSON.parse(String(body)) as Row;
  }
  push(ops: Op[]): { through: number; rows: Row[] } {
    let head = this.head();
    const rows = new Map<string, Row>();
    for (const op of ops) {
      const result = applyOp(this.row(op.kind, op.id), op);
      if (result.row && result.changed) {
        if (head === Number.MAX_SAFE_INTEGER) fail("sequence_exhausted");
        result.row.seq = ++head;
        if (bytes(result.row) > HUB_ROW_BYTES) fail("row_too_large");
        this.sql.exec("INSERT INTO workspace3_rows VALUES(?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,body=excluded.body",
          op.kind, op.id, head, JSON.stringify(result.row));
      }
      if (result.row) rows.set(`${op.kind}/${op.id}`, result.row);
    }
    if (head !== this.head()) this.setMeta("head", String(head));
    return { through: head, rows: [...rows.values()] };
  }
  page(after: number): { through: number; next: number; done: boolean; rows: Row[] } {
    const through = this.head();
    if (!integer(after) || after > through) fail("invalid_cursor");
    const rows: Row[] = [];
    let used = 1024;
    for (const raw of this.sql.exec("SELECT body FROM workspace3_rows WHERE seq>? ORDER BY seq LIMIT ?", after, HUB_PAGE_ROWS)) {
      const body = String(raw.body);
      const size = encoder.encode(body).length + 1;
      if (size - 1 > HUB_ROW_BYTES) fail("invalid_workspace_state");
      if (used + size > HUB_FRAME_BYTES) break;
      rows.push(JSON.parse(body) as Row);
      used += size;
    }
    const next = rows.at(-1)?.seq ?? through;
    return { through, next, done: next === through, rows };
  }
}

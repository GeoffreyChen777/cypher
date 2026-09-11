/** Experimental typed v3 protocol; no Loro/WASM and no implicit chat2 conversion. */
import { validCommand, isEntityId, type Command, type CommandStatus } from "./sync3-command";
export type { Command } from "./sync3-command";
export const VERSION = 3;
export const MAX_FRAME_BYTES = 256 * 1024;
export const MAX_BATCH_OPS = 64;
export const MAX_OPERATION_BYTES = MAX_FRAME_BYTES / 2;
const utf8 = new TextEncoder();

export type Event =
  | { type: "commandQueued"; commandId: string; command: Command }
  | { type: "commandAccepted"; commandId: string; runId: string }
  | { type: "commandResolved"; commandId: string; status: CommandStatus; resolution: string | null }
  | { type: "commandCancelled"; commandId: string }
  | { type: "runStarted"; runId: string }
  | { type: "messageCreated"; runId: string; messageId: string; role: "user" | "assistant" }
  | { type: "textAppended"; messageId: string; offset: number; text: string }
  | { type: "toolStarted"; runId: string; toolId: string; name: string }
  | { type: "toolFinished"; toolId: string; failed: boolean; summary: string }
  | { type: "inputRequested"; runId: string; requestId: string; prompt: string }
  | { type: "runFinished"; runId: string; outcome: "completed" | "failed" | "interrupted" };
export interface Operation { id: string; actor: string; ownerEpoch: number; event: Event }
export interface Row { seq: number; operation: Operation }
export interface Receipt { id: string; seq: number }
export type RequestFrame =
  | { type: "hello"; version: 3; actor: string; epoch: number; after: number }
  | { type: "push"; version: 3; operations: Operation[] }
  | { type: "pull"; version: 3; epoch: number; after: number; through: number }
  | { type: "probe"; version: 3 };
export type Reply =
  | { type: "state"; version: 3; epoch: number; owner: string; ownerEpoch: number; head: number }
  | { type: "ack"; version: 3; epoch: number; receipts: Receipt[] }
  | { type: "page"; version: 3; epoch: number; through: number; next: number; rows: Row[]; done: boolean }
  | { type: "error"; version: 3; code: string };

export class ProtocolError extends Error {}
export function reject(code: string): never { throw new ProtocolError(code); }
export function isId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}
export function safeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
export function byteLength(value: string): number { return utf8.encode(value).length; }
function shape(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) reject("invalid_shape");
  const record = value as Record<string, unknown>;
  if (Object.keys(record).length !== keys.length || !keys.every(key => Object.hasOwn(record, key))) {
    reject("invalid_shape");
  }
  return record;
}
function identifier(value: unknown): void { if (!isId(value)) reject("invalid_id"); }
function text(value: unknown): void {
  if (typeof value !== "string") reject("invalid_text");
  // JSON permits escaped lone surrogates; Rust/Swift strings do not. Reject
  // them rather than silently replacing bytes and disagreeing on offsets.
  for (let i = 0; i < value.length; i++) {
    const c = value.charCodeAt(i);
    if (c >= 0xd800 && c <= 0xdbff) {
      const next = value.charCodeAt(++i);
      if (!(next >= 0xdc00 && next <= 0xdfff)) reject("invalid_unicode");
    } else if (c >= 0xdc00 && c <= 0xdfff) reject("invalid_unicode");
  }
}
function validateJSON(value: unknown, depth = 0): void {
  if (depth > 32) reject("json_too_deep");
  if (typeof value === "string") text(value);
  else if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) reject("invalid_number");
  }
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) { text(k); validateJSON(v, depth + 1); }
  } else if (value !== null && typeof value !== "boolean") reject("invalid_json");
}
export function validateOperation(value: unknown): Operation {
  const op = shape(value, ["id", "actor", "ownerEpoch", "event"]);
  identifier(op.id); identifier(op.actor);
  if (!safeInteger(op.ownerEpoch) || op.ownerEpoch === 0) reject("invalid_epoch");
  const ev = op.event as Record<string, unknown>;
  if (!ev || typeof ev !== "object") reject("invalid_event");
  const fields: Record<string, string[]> = {
    commandQueued: ["commandId", "command"], commandAccepted: ["commandId", "runId"],
    commandResolved: ["commandId", "status", "resolution"], commandCancelled: ["commandId"],
    runStarted: ["runId"], messageCreated: ["runId", "messageId", "role"],
    textAppended: ["messageId", "offset", "text"], toolStarted: ["runId", "toolId", "name"],
    toolFinished: ["toolId", "failed", "summary"], inputRequested: ["runId", "requestId", "prompt"],
    runFinished: ["runId", "outcome"],
  };
  if (typeof ev.type !== "string" || !Object.hasOwn(fields, ev.type)) reject("invalid_event");
  shape(ev, ["type", ...fields[ev.type]]);
  for (const k of ["commandId", "runId"]) {
    if (Object.hasOwn(ev, k)) identifier(ev[k]);
  }
  for (const k of ["messageId", "toolId", "requestId"]) if (Object.hasOwn(ev, k) && !isEntityId(ev[k])) reject("invalid_id");
  for (const k of ["text", "name", "summary", "prompt"]) if (Object.hasOwn(ev, k)) text(ev[k]);
  if (ev.type === "textAppended" && !safeInteger(ev.offset)) reject("invalid_offset");
  if (ev.type === "messageCreated" && (typeof ev.role !== "string" || !["user", "assistant"].includes(ev.role))) reject("invalid_role");
  if (ev.type === "toolFinished" && typeof ev.failed !== "boolean") reject("invalid_tool_result");
  if (ev.type === "runFinished" && (typeof ev.outcome !== "string" || !["completed", "failed", "interrupted"].includes(ev.outcome))) reject("invalid_outcome");
  if (ev.type === "commandQueued") {
    const cmd = ev.command;
    if (!validCommand(cmd)) reject("invalid_command");
    if (cmd.status !== "pending" || cmd.resolution !== null) reject("command_not_pending");
    if (cmd.id !== ev.commandId || cmd.issuedBy !== op.actor) reject("command_identity_mismatch");
    if (cmd.payload.kind === "interrupt" && !cmd.basedOn?.turnId) reject("interrupt_requires_target");
  }
  if (ev.type === "commandResolved") {
    if (typeof ev.status !== "string" || !["applied", "rejected", "expired", "superseded"].includes(ev.status)) reject("invalid_command_resolution");
    if (ev.resolution !== null) text(ev.resolution);
  }
  validateJSON(op);
  if (byteLength(JSON.stringify(op)) > MAX_OPERATION_BYTES) reject("operation_too_large");
  return op as unknown as Operation;
}

export function parseRequest(raw: string): RequestFrame {
  if (byteLength(raw) > MAX_FRAME_BYTES) reject("frame_too_large");
  let frame: Record<string, unknown>;
  try { frame = JSON.parse(raw); } catch { return reject("invalid_json"); }
  if (!frame || frame.version !== VERSION) reject("upgrade_required");
  switch (frame.type) {
    case "hello":
      shape(frame, ["type", "version", "actor", "epoch", "after"]); identifier(frame.actor);
      if (!safeInteger(frame.epoch) || !safeInteger(frame.after)) reject("invalid_cursor");
      break;
    case "push":
      shape(frame, ["type", "version", "operations"]);
      if (!Array.isArray(frame.operations) || frame.operations.length < 1 || frame.operations.length > MAX_BATCH_OPS) reject("invalid_batch");
      frame.operations.forEach(validateOperation); break;
    case "pull":
      shape(frame, ["type", "version", "epoch", "after", "through"]);
      if (![frame.epoch, frame.after, frame.through].every(safeInteger)) reject("invalid_cursor");
      break;
    case "probe": shape(frame, ["type", "version"]); break;
    default: reject("invalid_frame");
  }
  return frame as unknown as RequestFrame;
}

/** Canonical receipt identity ignores JSON key order, not payload contents. */
export function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.keys(value).sort().map(k =>
    `${JSON.stringify(k)}:${canonical((value as Record<string, unknown>)[k])}`).join(",")}}`;
}

export interface Projection {
  commands: Record<string, { command: Command; actor: string; runId: string | null }>;
  runs: Record<string, { outcome: "completed" | "failed" | "interrupted" | null }>;
  messages: Record<string, { runId: string; role: "user" | "assistant"; text: string }>;
  tools: Record<string, { runId: string; name: string; failed: boolean | null; summary: string | null }>;
  inputs: Record<string, { runId: string; prompt: string }>;
}
export type EntityKind = keyof Projection;
export interface ProjectionStore {
  get<K extends EntityKind>(kind: K, id: string): Projection[K][string] | undefined;
  set<K extends EntityKind>(kind: K, id: string, value: Projection[K][string]): void;
  hasAcceptedRun(runId: string): boolean;
}
export function applyOperation(store: ProjectionStore, op: Operation, owner: string, ownerEpoch: number): void {
  validateOperation(op);
  if (op.ownerEpoch !== ownerEpoch) reject("stale_owner_epoch");
  const ev = op.event;
  if (ev.type !== "commandQueued" && ev.type !== "commandCancelled" && op.actor !== owner) reject("not_owner");
  const live = (run: string) => {
    if (store.get("runs", run)?.outcome !== null) reject("run_not_live");
  };
  switch (ev.type) {
    case "commandQueued":
      if (store.get("commands", ev.commandId)) reject("command_exists");
      store.set("commands", ev.commandId, { command: ev.command, actor: op.actor, runId: null }); break;
    case "commandAccepted": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.command.status !== "pending") reject("command_resolved");
      if (cmd.runId !== null) reject("command_already_accepted");
      store.set("commands", ev.commandId, { ...cmd, runId: ev.runId }); break;
    }
    case "commandResolved": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.command.status !== "pending") reject("command_resolved");
      if (ev.status === "applied" && cmd.runId === null) reject("command_not_accepted");
      store.set("commands", ev.commandId, { ...cmd, command: { ...cmd.command, status: ev.status, resolution: ev.resolution } }); break;
    }
    case "commandCancelled": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.actor !== op.actor) reject("not_command_author");
      if (cmd.runId !== null || cmd.command.status !== "pending") reject("command_not_cancellable");
      store.set("commands", ev.commandId, { ...cmd, command: { ...cmd.command, status: "cancelled" } }); break;
    }
    case "runStarted":
      if (store.get("runs", ev.runId)) reject("run_exists");
      if (!store.hasAcceptedRun(ev.runId)) reject("run_not_accepted");
      store.set("runs", ev.runId, { outcome: null }); break;
    case "messageCreated":
      live(ev.runId);
      if (store.get("messages", ev.messageId)) reject("message_exists");
      store.set("messages", ev.messageId, { runId: ev.runId, role: ev.role, text: "" }); break;
    case "textAppended": {
      const msg = store.get("messages", ev.messageId);
      if (!msg) reject("unknown_message");
      live(msg.runId);
      if (byteLength(msg.text) !== ev.offset) reject("text_offset_mismatch");
      store.set("messages", ev.messageId, { ...msg, text: msg.text + ev.text }); break;
    }
    case "toolStarted":
      live(ev.runId);
      if (store.get("tools", ev.toolId)) reject("tool_exists");
      store.set("tools", ev.toolId, { runId: ev.runId, name: ev.name, failed: null, summary: null }); break;
    case "toolFinished": {
      const tool = store.get("tools", ev.toolId);
      if (!tool) reject("unknown_tool");
      live(tool.runId);
      if (tool.failed !== null) reject("tool_finished");
      store.set("tools", ev.toolId, { ...tool, failed: ev.failed, summary: ev.summary }); break;
    }
    case "inputRequested":
      live(ev.runId);
      if (store.get("inputs", ev.requestId)) reject("input_exists");
      store.set("inputs", ev.requestId, { runId: ev.runId, prompt: ev.prompt }); break;
    case "runFinished":
      live(ev.runId); store.set("runs", ev.runId, { outcome: ev.outcome }); break;
  }
}

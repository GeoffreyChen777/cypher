/** Experimental typed v3 protocol; no Loro/WASM and no implicit chat2 conversion. */
import { validCommand, isEntityId, type Command, type CommandStatus } from "./sync3-command";
import { validPart, type Part, type Entry } from "./sync3-transcript";
export type { Command } from "./sync3-command";
export const VERSION = 3;
export const MAX_FRAME_BYTES = 256 * 1024;
export const MAX_BATCH_OPS = 64;
export const MAX_OPERATION_BYTES = MAX_FRAME_BYTES / 2;
export const MAX_MESSAGE_BYTES = 256 * 1024;
const utf8 = new TextEncoder();

export type Event =
  | { type: "commandQueued"; commandId: string; command: Command }
  | { type: "commandClaimAttempted"; commandId: string; runId: string }
  | { type: "commandResolved"; commandId: string; status: CommandStatus; resolution: string | null }
  | { type: "commandCancelAttempted"; commandId: string }
  | { type: "executionStarted"; executionId: string; commandId: string }
  | { type: "executionFinished"; executionId: string }
  | { type: "runStarted"; runId: string }
  | { type: "runObserved"; runId: string; executionId: string }
  | { type: "messageCreated"; runId: string | null; messageId: string; role: Entry["role"]; deviceId: string; createdAt: number; continuationOf: string | null }
  | { type: "partPut"; messageId: string; index: number; part: Part }
  | { type: "textAppended"; messageId: string; partId: string; offset: number; text: string }
  | { type: "messageFinished"; messageId: string; status: "complete" | "aborted" | null }
  | { type: "attachmentSealed"; uploadId: string; path: string; fileName: string }
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
    commandQueued: ["commandId", "command"], commandClaimAttempted: ["commandId", "runId"],
    commandResolved: ["commandId", "status", "resolution"], commandCancelAttempted: ["commandId"],
    executionStarted: ["executionId", "commandId"], executionFinished: ["executionId"],
    runStarted: ["runId"], runObserved: ["runId", "executionId"], messageCreated: ["runId", "messageId", "role", "deviceId", "createdAt", "continuationOf"],
    partPut: ["messageId", "index", "part"], textAppended: ["messageId", "partId", "offset", "text"],
    messageFinished: ["messageId", "status"], attachmentSealed: ["uploadId", "path", "fileName"],
    runFinished: ["runId", "outcome"],
  };
  if (typeof ev.type !== "string" || !Object.hasOwn(fields, ev.type)) reject("invalid_event");
  shape(ev, ["type", ...fields[ev.type]]);
  for (const k of ["commandId", "executionId", "runId", "deviceId", "uploadId"]) {
    if (Object.hasOwn(ev, k) && !(k === "runId" && ev.type === "messageCreated" && ev[k] === null)) identifier(ev[k]);
  }
  for (const k of ["messageId", "partId"]) if (Object.hasOwn(ev, k) && !isEntityId(ev[k])) reject("invalid_id");
  for (const k of ["text", "path", "fileName"]) if (Object.hasOwn(ev, k)) text(ev[k]);
  if (ev.type === "textAppended" && !safeInteger(ev.offset)) reject("invalid_offset");
  if (ev.type === "messageCreated" && (typeof ev.role !== "string" || !["user", "assistant", "system"].includes(ev.role))) reject("invalid_role");
  if (ev.type === "messageCreated") {
    if (!safeInteger(ev.createdAt)) reject("invalid_timestamp");
    if (ev.continuationOf !== null && (!isEntityId(ev.continuationOf) || ev.continuationOf === ev.messageId)) reject("invalid_continuation");
  }
  if (ev.type === "partPut") {
    if (!safeInteger(ev.index) || ev.index >= 256) reject("too_many_parts");
    if (!validPart(ev.part)) reject("invalid_part");
  }
  if (ev.type === "messageFinished" && ev.status !== null && ev.status !== "complete" && ev.status !== "aborted") reject("invalid_message_status");
  if (ev.type === "attachmentSealed" && (!ev.path || !ev.fileName)) reject("invalid_attachment");
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
  executions: Record<string, { commandId: string; actor: string; ownerEpoch: number; closed: boolean }>;
  commands: Record<string, { command: Command; actor: string; runId: string | null; acceptedOpId: string | null }>;
  runs: Record<string, { outcome: "completed" | "failed" | "interrupted" | null }>;
  messages: Record<string, { createdSeq: number; runId: string | null; entry: Entry }>;
  attachments: Record<string, { path: string; fileName: string }>;
}
export type EntityKind = keyof Projection;
export interface ProjectionStore {
  get<K extends EntityKind>(kind: K, id: string): Projection[K][string] | undefined;
  set<K extends EntityKind>(kind: K, id: string, value: Projection[K][string]): void;
  hasAcceptedRun(runId: string): boolean;
  hasOpenMessage(runId: string): boolean;
  hasExecution(commandId?: string): boolean;
  hasUnresolvedExecution(): boolean;
}
export function applyOperation(store: ProjectionStore, op: Operation, owner: string, ownerEpoch: number, seq: number): void {
  if (!safeInteger(seq) || seq === 0) reject("invalid_sequence");
  validateOperation(op);
  if (op.ownerEpoch !== ownerEpoch) reject("stale_owner_epoch");
  const ev = op.event;
  if (ev.type !== "commandQueued" && ev.type !== "commandCancelAttempted" && op.actor !== owner) reject("not_owner");
  const live = (run: string) => {
    if (store.get("runs", run)?.outcome !== null) reject("run_not_live");
  };
  const writable = (id: string) => {
    const msg = store.get("messages", id);
    if (!msg) reject("unknown_message");
    if (msg.runId !== null) live(msg.runId);
    if (msg.entry.status !== "streaming") reject("message_finished");
    return structuredClone(msg);
  };
  const putMessage = (id: string, msg: Projection["messages"][string]) => {
    if (byteLength(JSON.stringify(msg)) > MAX_MESSAGE_BYTES) reject("message_too_large");
    store.set("messages", id, msg);
  };
  switch (ev.type) {
    case "executionStarted": {
      if (store.get("executions", ev.executionId)) reject("execution_exists");
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.acceptedOpId === null || cmd.command.status !== "pending") reject("command_not_accepted");
      if (!["run", "steer"].includes(cmd.command.payload.kind)) reject("invalid_execution_command");
      if (store.hasExecution(ev.commandId)) reject("command_has_execution");
      if (store.hasExecution()) reject("execution_busy");
      store.set("executions", ev.executionId, { commandId: ev.commandId, actor: op.actor, ownerEpoch: op.ownerEpoch, closed: false }); break;
    }
    case "executionFinished": {
      const execution = store.get("executions", ev.executionId);
      if (!execution) reject("unknown_execution");
      if (execution.closed) reject("execution_closed");
      if (execution.actor !== op.actor || execution.ownerEpoch !== op.ownerEpoch) reject("execution_owner_mismatch");
      if (store.hasUnresolvedExecution()) reject("execution_unresolved");
      store.set("executions", ev.executionId, { ...execution, closed: true }); break;
    }
    case "commandQueued":
      if (store.get("commands", ev.commandId)) reject("command_exists");
      store.set("commands", ev.commandId, { command: ev.command, actor: op.actor, runId: null, acceptedOpId: null }); break;
    case "commandClaimAttempted": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.command.status === "pending" && cmd.runId === null) {
        store.set("commands", ev.commandId, { ...cmd, runId: ev.runId, acceptedOpId: op.id });
      }
      break;
    }
    case "commandResolved": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.command.status !== "pending") reject("command_resolved");
      if (ev.status === "applied" && cmd.runId === null) reject("command_not_accepted");
      store.set("commands", ev.commandId, { ...cmd, command: { ...cmd.command, status: ev.status, resolution: ev.resolution } }); break;
    }
    case "commandCancelAttempted": {
      const cmd = store.get("commands", ev.commandId);
      if (!cmd) reject("unknown_command");
      if (cmd.actor !== op.actor) reject("not_command_author");
      if (cmd.runId === null && cmd.command.status === "pending") {
        store.set("commands", ev.commandId, { ...cmd, command: { ...cmd.command, status: "cancelled" } });
      }
      break;
    }
    case "runStarted":
      if (store.get("runs", ev.runId)) reject("run_exists");
      if (!store.hasAcceptedRun(ev.runId)) reject("run_not_accepted");
      store.set("runs", ev.runId, { outcome: null }); break;
    case "runObserved": {
      if (store.get("runs", ev.runId)) reject("run_exists");
      const execution = store.get("executions", ev.executionId);
      if (!execution) reject("unknown_execution");
      if (execution.closed) reject("execution_closed");
      if (execution.actor !== op.actor || execution.ownerEpoch !== op.ownerEpoch) reject("execution_owner_mismatch");
      store.set("runs", ev.runId, { outcome: null }); break;
    }
    case "messageCreated":
      if (ev.runId !== null) live(ev.runId);
      if (store.get("messages", ev.messageId)) reject("message_exists");
      store.set("messages", ev.messageId, { createdSeq: seq, runId: ev.runId, entry: {
        id: ev.messageId, role: ev.role, deviceId: ev.deviceId, createdAt: ev.createdAt,
        parts: [], status: "streaming", ...(ev.continuationOf === null ? {} : { continuationOf: ev.continuationOf }),
      } }); break;
    case "partPut": {
      const msg = writable(ev.messageId), parts = msg.entry.parts;
      if (ev.index > parts.length) reject("part_gap");
      const old = parts[ev.index], part = ev.part;
      if (old) {
        if (old.id !== part.id || old.kind !== part.kind) reject("part_identity_mismatch");
        if (old.kind === "text" && part.kind === "text" && old.text !== part.text) reject("text_requires_delta");
        if (old.kind === "tool" && part.kind === "tool" && old.resolved && !part.resolved) reject("part_resolved");
        if (old.kind === "input" && part.kind === "input") {
          if (old.requestId !== part.requestId || canonical(old.questions) !== canonical(part.questions)) reject("question_changed");
          if (old.resolved && !part.resolved) reject("part_resolved");
        }
        parts[ev.index] = structuredClone(part);
      } else {
        if (parts.some(p => p.id === part.id)) reject("part_exists");
        parts.push(structuredClone(part));
      }
      putMessage(ev.messageId, msg); break;
    }
    case "textAppended": {
      const msg = writable(ev.messageId), part = msg.entry.parts.find(p => p.id === ev.partId);
      if (!part) reject("unknown_part");
      if (part.kind !== "text") reject("not_text");
      if (byteLength(part.text) !== ev.offset) reject("text_offset_mismatch");
      part.text += ev.text; putMessage(ev.messageId, msg); break;
    }
    case "messageFinished": {
      const msg = writable(ev.messageId);
      if (ev.status === null) delete msg.entry.status; else msg.entry.status = ev.status;
      putMessage(ev.messageId, msg); break;
    }
    case "attachmentSealed": {
      const value = { path: ev.path, fileName: ev.fileName }, old = store.get("attachments", ev.uploadId);
      if (old && canonical(old) !== canonical(value)) reject("attachment_conflict");
      store.set("attachments", ev.uploadId, value); break;
    }
    case "runFinished":
      live(ev.runId);
      if (store.hasOpenMessage(ev.runId)) reject("unfinished_messages");
      store.set("runs", ev.runId, { outcome: ev.outcome }); break;
  }
}

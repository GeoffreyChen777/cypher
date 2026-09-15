/** P1 codec, NOT authorization. Used only by the opt-in development relay;
 * not registered in the production decoder. See docs/ephemeral-stream-v1.md. */
import { encodeFrame, MAX_HEADER_BYTES } from "./chat-frames";
import type { FrameType } from "./chat-frames";

export const STREAM_CAPABILITY = "ephemeral-stream-v1";
export const STREAM = { delta: 0x20, snapshot: 0x21, resume: 0x22, finished: 0x23,
  start: 0x24, state: 0x25, receipt: 0x26 } as const;
export const MAX_STREAM_FRAME_BYTES = 65_536;
export const MAX_STREAM_TEXT_BYTES = 61_440;
const utf8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const encoder = new TextEncoder();
const identifier = (v: unknown): v is string => typeof v === "string" && v.length >= 1 && v.length <= 128 && !/[^A-Za-z0-9._:-]/.test(v);
const integer = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;

export interface StreamFrame {
  kind: number;
  header: Record<string, unknown>;
  text: string;
}

export function decodeStream(bytes: Uint8Array): StreamFrame | undefined {
  if (bytes.length < 5 || bytes.length > MAX_STREAM_FRAME_BYTES) return;
  const kind = bytes[0]!;
  if (!Object.values(STREAM).some(v => v === kind)) return;
  const length = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(1, true);
  if (length > MAX_HEADER_BYTES || 5 + length > bytes.length) return;
  try {
    const header = JSON.parse(utf8.decode(bytes.subarray(5, 5 + length)));
    if (!header || typeof header !== "object" || Array.isArray(header)) return;
    const payload = bytes.subarray(5 + length);
    if (kind === STREAM.start || kind === STREAM.state) {
      const keys = kind === STREAM.start ? ["chatId", "runId", "segmentId"]
        : header.mode === "preview" ? ["chatId", "mode", "runId", "segmentId", "epoch"] : ["chatId", "mode"];
      if (payload.length || Object.keys(header).length !== keys.length || !keys.every(k => Object.hasOwn(header, k))) return;
      if (!keys.filter(k => k !== "mode").every(k => identifier(header[k]))) return;
      if (kind === STREAM.state && !["legacy", "ready", "preview"].includes(header.mode)) return;
      return { kind, header, text: "" };
    }
    const keys = ["chatId", "runId", "segmentId", "epoch", "revision", "baseSeq"];
    if (kind === STREAM.delta) keys.push("prevRevision");
    if (kind === STREAM.finished) keys.push("batchId");
    if (Object.keys(header).length !== keys.length || !keys.every(k => Object.hasOwn(header, k))) return;
    if (!keys.slice(0, 4).every(k => identifier(header[k]))) return;
    if (!integer(header.revision) || !integer(header.baseSeq)) return;
    if (payload.length > MAX_STREAM_TEXT_BYTES) return;
    if (kind === STREAM.delta && (!integer(header.prevRevision) || header.prevRevision + 1 !== header.revision || !payload.length)) return;
    if (kind === STREAM.finished && !identifier(header.batchId)) return;
    if ([STREAM.resume, STREAM.finished, STREAM.receipt].some(v => v === kind) && payload.length) return;
    return { kind, header, text: utf8.decode(payload) };
  } catch {
    return;
  }
}

export function encodeStream(kind: number, header: Record<string, unknown>, text = ""): Uint8Array | undefined {
  if (!Object.values(STREAM).some(v => v === kind) || text.length > MAX_STREAM_TEXT_BYTES) return;
  // Existing envelope encoder accepts a byte; do not add these types to the
  // legacy decoder's allowlist until negotiation/authorization are implemented.
  try {
    const bytes = encodeFrame(kind as FrameType, header, encoder.encode(text));
    return decodeStream(bytes)?.text === text ? bytes : undefined;
  } catch {
    return;
  }
}

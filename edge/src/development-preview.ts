/** Development-only, opt-in preview relay. No storage API, timers or transcript.
 * A grant lives only in this instance: cold start requires fresh WS admission.
 * Never construct this from the production entrypoint. */
import type { Env } from "./env";
import { encodeFrame, FRAME } from "./chat-frames";
import { decodeStream, encodeStream, STREAM, STREAM_CAPABILITY, MAX_STREAM_TEXT_BYTES } from "./stream-preview";

export interface PreviewConfig {
  DEV_PREVIEW_ENABLED?: string;
  DEV_PREVIEW_PUBLISH_TOKEN?: string;
}
export const PREVIEW_CAPABILITY_HEADER = "x-cypher-preview-capability";
export const PREVIEW_PUBLISH_HEADER = "x-cypher-preview-publisher";
export const PREVIEW_LIMITS = { peers: 8, frames: 100, bytes: 1024 * 1024, windowMs: 10_000,
  pendingFrames: 32, pendingBytes: 256 * 1024, idleMs: 60_000, resumeMs: 1000, controls: 128 } as const;

export interface PreviewAdmission { chatId: string; capable: boolean; publisher: boolean }
interface Peer extends PreviewAdmission {
  ready: boolean;
  windowAt: number;
  frames: number;
  bytes: number;
  resumedAt: number;
  controls: number;
  pending: { epoch: string; revision: number; bytes: number }[];
}
interface Grant {
  author: WebSocket;
  runId: string;
  segmentId: string;
  epoch: string;
  revision: number;
  baseSeq: number;
  textBytes: number;
  initialized: boolean;
  finished: boolean;
  touchedAt: number;
}

export function createDevelopmentPreview(ctx: DurableObjectState, env: Pick<Env, "AUTH_MODE" | "DEV_ACCESS_TOKEN"> & PreviewConfig): DevelopmentPreviewRelay | undefined {
  const secret = env.DEV_PREVIEW_PUBLISH_TOKEN;
  if (env.AUTH_MODE !== "dev-locked" || env.DEV_PREVIEW_ENABLED !== "true" || !env.DEV_ACCESS_TOKEN
      || !secret || secret.length !== 64 || !/^[a-f0-9]{64}$/.test(secret) || secret === env.DEV_ACCESS_TOKEN) return;
  return new DevelopmentPreviewRelay(() => ctx.getWebSockets(), secret);
}

export class DevelopmentPreviewRelay {
  private readonly peers = new Map<WebSocket, Peer>();
  private readonly gone = new WeakSet<WebSocket>();
  private grant?: Grant;
  private chatId?: string;

  constructor(private readonly sockets: () => WebSocket[], private readonly secret: string) {}

  /** Called only after normal user authentication + room ownership checks.
   * The Worker overwrites chatId from the route, not the caller's query. */
  admit(request: Request): PreviewAdmission | Response {
    const chatId = new URL(request.url).searchParams.get("chatId") ?? "";
    if (!chatId || chatId.length > 128 || /[^A-Za-z0-9_-]/.test(chatId)
        || (this.chatId !== undefined && this.chatId !== chatId)) return new Response("Invalid preview room", { status: 400 });
    if (this.live().length >= PREVIEW_LIMITS.peers) return new Response("Preview room connection limit", { status: 429 });
    const capable = request.headers.get(PREVIEW_CAPABILITY_HEADER) === STREAM_CAPABILITY;
    const token = request.headers.get(PREVIEW_PUBLISH_HEADER);
    if (token !== null) {
      // Fixed-length comparison. Never echo/store credentials in attachments,
      // protocol errors or URLs. This token is independent of the login token.
      let difference = token.length ^ this.secret.length;
      for (let i = 0; i < 64; i++) difference |= (token.charCodeAt(i) || 0) ^ this.secret.charCodeAt(i);
      if (!capable || difference !== 0) return new Response("Preview publisher forbidden", { status: 403 });
    }
    return { chatId, capable, publisher: token !== null };
  }

  joined(ws: WebSocket, admission: PreviewAdmission): void {
    this.chatId = admission.chatId;
    if (admission.publisher) {
      for (const [old, peer] of this.peers) if (peer.publisher) this.drop(old, "Preview publisher replaced");
    }
    this.peers.set(ws, { ...admission, ready: false, windowAt: Date.now(), frames: 0, bytes: 0,
      pending: [], controls: 0, resumedAt: -Infinity });
    this.invalidate();
  }

  hello(ws: WebSocket): void {
    const peer = this.peers.get(ws);
    if (!peer) return; // Pre-hibernation sockets cannot regain privileges via HELLO.
    if (!peer.ready) { peer.ready = true; this.invalidate(); }
  }

  left(ws: WebSocket): void {
    if (this.gone.has(ws)) return; // A replaced connection's late close cannot revoke its successor.
    this.gone.add(ws);
    this.peers.delete(ws);
    this.invalidate();
  }

  reset(): void {
    for (const ws of this.peers.keys()) this.gone.add(ws);
    this.invalidate();
    this.peers.clear();
  }

  /** True means this was a preview frame, including a rejected preview frame.
   * MUST run before ChatRoom's legacy decoder; no fallback to PUSH/PRESENCE. */
  message(ws: WebSocket, bytes: Uint8Array): boolean {
    if (!bytes.length || bytes[0]! < 0x20 || bytes[0]! > 0x2f) return false;
    if (this.gone.has(ws)) return true;
    this.prune();
    const peer = this.peers.get(ws);
    if (!peer?.ready || !peer.capable) { this.error(ws, "preview_reconnect_required"); return true; }
    const now = Date.now();
    if (now - peer.windowAt >= PREVIEW_LIMITS.windowMs) { peer.windowAt = now; peer.frames = 0; peer.bytes = 0; }
    if (++peer.frames > PREVIEW_LIMITS.frames || (peer.bytes += bytes.length) > PREVIEW_LIMITS.bytes) {
      this.drop(ws, "Preview rate limit"); this.invalidate(); return true;
    }
    const frame = decodeStream(bytes);
    if (!frame || frame.header.chatId !== peer.chatId) { this.error(ws, "bad_preview_frame"); return true; }
    if (frame.kind === STREAM.receipt) {
      // Receipts for pre-transition epochs can still free this connection's
      // credit. A mode switch must NOT erase queued bytes for a slow viewer.
      const revision = frame.header.revision as number;
      if (peer.publisher || !peer.pending.some(p => p.epoch === frame.header.epoch && p.revision === revision)) this.error(ws, "bad_preview_receipt");
      else peer.pending = peer.pending.filter(p => p.epoch !== frame.header.epoch || p.revision > revision);
      return true;
    }
    if (this.grant && now - this.grant.touchedAt > PREVIEW_LIMITS.idleMs) this.invalidate();
    if (frame.kind === STREAM.start) {
      if (!peer.publisher || !this.ready()) { this.error(ws, "preview_not_ready"); return true; }
      this.grant = { author: ws, runId: frame.header.runId as string, segmentId: frame.header.segmentId as string,
        epoch: crypto.randomUUID(), revision: 0, baseSeq: 0, textBytes: 0, initialized: false,
        finished: false, touchedAt: now };
      this.announce();
      return true;
    }
    const grant = this.grant;
    if (!grant || !this.ready() || frame.header.epoch !== grant.epoch || frame.header.runId !== grant.runId
        || frame.header.segmentId !== grant.segmentId) { this.error(ws, "preview_stale_grant"); return true; }
    const revision = frame.header.revision as number;
    if (frame.kind === STREAM.resume) {
      if (peer.publisher || now - peer.resumedAt < PREVIEW_LIMITS.resumeMs) return true;
      peer.resumedAt = now;
      if (!this.emit(grant.author, bytes)) this.invalidate();
      return true; // Engine broadcasts a fresh snapshot; DO has no text cache.
    }
    if (grant.author !== ws || !peer.publisher || frame.kind === STREAM.state) {
      this.error(ws, "preview_publisher_required"); return true;
    }
    if ((grant.finished && frame.kind !== STREAM.snapshot) || (frame.header.baseSeq as number) < grant.baseSeq) {
      this.error(ws, "preview_stale_revision"); return true;
    }
    const textBytes = bytes.length - 5 - new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(1, true);
    if (frame.kind === STREAM.snapshot) {
      if (revision < grant.revision || (grant.finished && revision !== grant.revision)) { this.error(ws, "preview_stale_revision"); return true; }
      grant.textBytes = textBytes; grant.initialized = true;
    } else if (frame.kind === STREAM.delta) {
      if (!grant.initialized || frame.header.prevRevision !== grant.revision) { this.error(ws, "preview_snapshot_required"); return true; }
      if (grant.textBytes + textBytes > MAX_STREAM_TEXT_BYTES) { this.invalidate(); return true; }
      grant.textBytes += textBytes;
    } else if (frame.kind === STREAM.finished) {
      if (!grant.initialized || revision !== grant.revision) { this.error(ws, "preview_snapshot_required"); return true; }
      grant.finished = true;
    } else { this.error(ws, "bad_preview_direction"); return true; }
    grant.revision = revision; grant.baseSeq = frame.header.baseSeq as number; grant.touchedAt = now;
    let failed = false;
    for (const target of this.live()) {
      if (target === ws) continue;
      const viewer = this.peers.get(target)!;
      if (viewer.pending.length >= PREVIEW_LIMITS.pendingFrames
          || viewer.pending.reduce((n, p) => n + p.bytes, 0) + bytes.length > PREVIEW_LIMITS.pendingBytes) {
        this.drop(target, "Preview consumer stalled"); failed = true; continue;
      }
      if (this.emit(target, bytes)) viewer.pending.push({ epoch: grant.epoch, revision, bytes: bytes.length });
      else failed = true;
    }
    if (failed) this.invalidate();
    return true;
  }

  private live(): WebSocket[] { return this.sockets().filter(ws => !this.gone.has(ws) && ws.readyState === WebSocket.OPEN); }
  private prune(): void {
    const live = new Set(this.live());
    let changed = false;
    for (const ws of this.peers.keys()) if (!live.has(ws)) { this.peers.delete(ws); changed = true; }
    if (changed) this.invalidate();
  }
  private ready(): boolean {
    const live = this.live();
    return live.length > 0 && live.length <= PREVIEW_LIMITS.peers
      && live.every(ws => this.peers.get(ws)?.capable && this.peers.get(ws)?.ready)
      && live.some(ws => this.peers.get(ws)?.publisher);
  }
  private invalidate(): void { this.grant = undefined; this.announce(); }
  private announce(round = 0): void {
    if (!this.chatId) return;
    const g = this.grant;
    const header = g ? { chatId: this.chatId, mode: "preview", runId: g.runId, segmentId: g.segmentId, epoch: g.epoch }
      : { chatId: this.chatId, mode: this.ready() ? "ready" : "legacy" };
    const bytes = encodeStream(STREAM.state, header)!;
    let failed = false;
    for (const ws of this.live()) if (this.peers.get(ws)?.capable && this.peers.get(ws)?.ready) {
      if (!this.control(ws, bytes)) failed = true;
    }
    // A failed control send can remove the author or change membership. Send
    // a corrected state to survivors, not an already-revoked grant. Bounded by
    // the admitted connection count; a failed peer is removed on each pass.
    if (failed && round < PREVIEW_LIMITS.peers) this.announce(round + 1);
  }
  private error(ws: WebSocket, code: string): void {
    if (!this.peers.has(ws)) { this.drop(ws, "Preview reconnect required"); this.invalidate(); return; }
    if (!this.control(ws, encodeFrame(FRAME.error, { code, message: "Preview unavailable" }))) this.invalidate();
  }
  private control(ws: WebSocket, bytes: Uint8Array): boolean {
    const peer = this.peers.get(ws);
    if (!peer || ++peer.controls > PREVIEW_LIMITS.controls) { this.drop(ws, "Preview control limit; reconnect"); return false; }
    return this.emit(ws, bytes);
  }
  private emit(ws: WebSocket, bytes: Uint8Array): boolean {
    try { ws.send(bytes); return true; } catch { this.drop(ws, "Preview socket unavailable"); return false; }
  }
  private drop(ws: WebSocket, reason: string): void {
    this.gone.add(ws); this.peers.delete(ws); this.grant = undefined;
    try { ws.close(1013, reason); } catch { /* Already closed. */ }
  }
}

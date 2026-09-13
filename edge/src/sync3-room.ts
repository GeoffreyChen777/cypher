import { AUTH_USER_HEADER, AUTH_DEADLINE_HEADER } from "./env";
import { Sync3Log } from "./sync3-log";
import { MAX_FRAME_BYTES, parseRequest, ProtocolError, reject, VERSION, type Reply } from "./sync3-protocol";

interface SocketState { actor?: string; epoch?: number; expires: number }
const json = (body: unknown, status = 200) => Response.json(body, { status, headers: { "cache-control": "no-store" } });

/** Experimental direct chat stream. No periodic timers and no DO-to-DO
 * per-token forwarding. Not bound/enabled in production wrangler.jsonc. */
export class Sync3Room implements DurableObject {
  private readonly log: Sync3Log;
  constructor(private readonly ctx: DurableObjectState) {
    this.log = new Sync3Log(ctx.storage);
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }
  async fetch(request: Request): Promise<Response> {
    const account = request.headers.get(AUTH_USER_HEADER);
    if (!account) return json({ error: "unauthenticated" }, 401);
    const url = new URL(request.url);
    try {
      const m = this.log.meta();
      if (m && m.account !== account) return json({ error: "forbidden" }, 403);
      if (url.pathname === "/init" && request.method === "POST") {
        const raw = await boundedBody(request);
        let body: { owner?: unknown };
        try { body = JSON.parse(raw); } catch { return json(this.failure(new ProtocolError("invalid_json")), 400); }
        if (!body || typeof body !== "object" || Array.isArray(body) ||
            Object.keys(body).length !== 1 || typeof body.owner !== "string") reject("invalid_owner");
        this.log.initialize(account, body.owner);
        return json(this.log.state());
      }
      if (!m) return json(this.failure(new ProtocolError("not_initialized")), 409);
      if (url.pathname === "/ws") {
        if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") return json({ error: "expected_websocket" }, 426);
        const expires = Number(request.headers.get(AUTH_DEADLINE_HEADER));
        if (!Number.isFinite(expires) || expires <= Date.now()) return json({ error: "reauth_required" }, 401);
        const pair = new WebSocketPair();
        this.ctx.acceptWebSocket(pair[1]);
        // Bounded authentication lifetime for this experimental endpoint.
        // Full control-channel reauth is a separate acceptance item.
        pair[1].serializeAttachment({ expires: Math.min(expires, Date.now() + 300_000) } satisfies SocketState);
        return new Response(null, { status: 101, webSocket: pair[0] });
      }
      if (url.pathname === "/exchange" && request.method === "POST") {
        const reply = this.dispatch(await boundedBody(request));
        if (reply.type === "ack") this.broadcastHead();
        return json(reply);
      }
      return json({ error: "not_found" }, 404);
    } catch (error) {
      return json(this.failure(error), error instanceof ProtocolError ? 400 : 500);
    }
  }
  private dispatch(raw: string, socket?: WebSocket): Reply {
    const frame = parseRequest(raw);
    const state = socket?.deserializeAttachment() as SocketState | undefined;
    if (state && state.expires <= Date.now()) reject("reauth_required");
    if (frame.type === "hello") {
      const current = this.log.state();
      // epoch=0 is only a genuinely new local store, not a reset escape hatch.
      if (frame.epoch !== current.epoch && !(frame.epoch === 0 && frame.after === 0)) reject("epoch_mismatch");
      if (frame.after > current.head) reject("server_behind");
      if (socket && state) {
        if (state.actor && state.actor !== frame.actor) reject("actor_mismatch");
        socket.serializeAttachment({ ...state, actor: frame.actor, epoch: current.epoch });
      }
      return current;
    }
    if (socket && !state?.actor) reject("hello_required");
    if (frame.type === "probe") return this.log.state();
    if (frame.type === "pull") return this.log.page(frame.epoch, frame.after, frame.through);
    return this.log.append(frame.operations, state?.actor);
  }
  webSocketMessage(socket: WebSocket, message: string | ArrayBuffer): void {
    try {
      if (typeof message !== "string") reject("expected_json");
      const reply = this.dispatch(message, socket);
      try { socket.send(JSON.stringify(reply)); }
      finally {
        // A dead sender cannot suppress committed-head hints to readers.
        // ACK never advances the sender's applied cursor.
        if (reply.type === "ack") this.broadcastHead();
      }
    } catch (error) {
      this.send(socket, this.failure(error));
    }
  }
  webSocketClose(socket: WebSocket, code: number): void {
    try { socket.close(code === 1006 ? 1000 : code, "closed"); } catch { /* already gone */ }
  }
  webSocketError(socket: WebSocket): void {
    try { socket.close(1011, "reconnect"); } catch { /* already gone */ }
  }
  private send(socket: WebSocket, reply: Reply): void {
    try { socket.send(JSON.stringify(reply)); } catch { this.webSocketError(socket); }
  }
  private broadcastHead(): void {
    const head = this.log.state();
    for (const socket of this.ctx.getWebSockets()) {
      const state = socket.deserializeAttachment() as SocketState;
      if (state.expires <= Date.now()) {
        try { socket.close(1008, "reauth_required"); } catch { /* already gone */ }
      } else if (state.actor) this.send(socket, head);
    }
  }
  private failure(error: unknown): Reply {
    // Never expose SQL, request bodies or credentials to clients/logs.
    return { type: "error", version: VERSION, code: error instanceof ProtocolError ? error.message : "internal_error" };
  }
}

async function boundedBody(request: Request): Promise<string> {
  const reader = request.body?.getReader();
  if (!reader) reject("empty_body");
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > MAX_FRAME_BYTES) { await reader.cancel(); reject("frame_too_large"); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  try { return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { return reject("invalid_unicode"); }
}

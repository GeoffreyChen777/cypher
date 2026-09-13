/** Workspace v3: one authenticated account, one control connection per
 * device/role, bounded current-state pages, replaceable presence/demand and
 * hibernation-safe RPC routing. This never grants execution ownership. */
import { AUTH_USER_HEADER, AUTH_ORG_HEADER, AUTH_DEADLINE_HEADER, type Env } from "./env";
import { Notifications } from "./notifications";
import { HUB_FRAME_BYTES, HubRows, HubError, bytes, decode, fail, id, integer, keys, object, operations } from "./workspace3-core";

type Call = { id: string; host: string; nonce: string; next: number; ack: number; done: boolean;
  input?: { next: number; ack: number; done: boolean } };
type Peer = { actor: string; role: "host" | "viewer"; connection: string; topics: string[]; calls: Call[] };
type SocketInfo = { expires: number; peer?: Peer };
type Route = { from: string; to: string; request: string; nonce: string };
const LEASE_MS = 45_000;
const MAX_SOCKETS = 64;
const MAX_TOPICS = 8;
const MAX_CALLS = 8;
const RPC_WINDOW = 2;
const RPC_BYTES = 64 * 1024;
const encoder = new TextEncoder();

function base64(value: Uint8Array): string {
  return btoa(String.fromCharCode(...value)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function unbase64(value: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) fail("invalid_route");
  try { return Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/")), c => c.charCodeAt(0)); }
  catch { fail("invalid_route"); }
}

export class WorkspaceHub implements DurableObject {
  private readonly rows: HubRows;
  private readonly signing: Promise<CryptoKey>;
  private readonly presence = new Map<string, { actor: string; role: Peer["role"]; expiresAt: number; state: unknown }>();
  private readonly demands = new Map<string, string>();
  private messages: Promise<void> = Promise.resolve();
  private queuedMessages = 0;
  private queuedBytes = 0;
  private readonly notifications: Notifications;
  private alarmScheduling: Promise<void> = Promise.resolve();

  constructor(private readonly ctx: DurableObjectState, env: Env) {
    this.rows = new HubRows(ctx.storage.sql);
    this.notifications = new Notifications(ctx, env, (kind, id) => this.rows.row(kind, id), () => this.scheduleAlarm());
    let secret = this.rows.meta("route_secret");
    if (!secret) {
      secret = base64(crypto.getRandomValues(new Uint8Array(32)));
      this.rows.setMeta("route_secret", secret);
    }
    this.signing = crypto.subtle.importKey("raw", unbase64(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request: Request): Promise<Response> {
    const user = request.headers.get(AUTH_USER_HEADER);
    if (!user) return new Response("unauthorized", { status: 401 });
    const known = this.rows.meta("user");
    if (known && known !== user) return new Response("scope mismatch", { status: 403 });
    const org = request.headers.get(AUTH_ORG_HEADER);
    if (!id(org)) return new Response("unauthorized", { status: 401 });
    const knownOrg = this.rows.meta("org");
    if (knownOrg && knownOrg !== org) return new Response("scope mismatch", { status: 403 });
    if (!known) this.rows.setMeta("user", user);
    if (!knownOrg) this.rows.setMeta("org", org);
    const path = new URL(request.url).pathname;
    if (path.startsWith("/notifications/")) return this.notifications.fetch(request, path.slice("/notifications/".length));
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return new Response("WebSocket required", { status: 426 });
    const expires = Number(request.headers.get(AUTH_DEADLINE_HEADER));
    if (!integer(expires) || expires <= Date.now()) return new Response("reauth required", { status: 401 });
    for (const ws of this.ctx.getWebSockets()) {
      if ((ws.deserializeAttachment() as SocketInfo).expires <= Date.now()) ws.close(4401, "reauth_required");
    }
    if (this.ctx.getWebSockets().filter(ws => ws.readyState === WebSocket.OPEN &&
        (ws.deserializeAttachment() as SocketInfo).expires > Date.now()).length >= MAX_SOCKETS)
      return new Response("connection limit", { status: 429 });
    const pair = new WebSocketPair();
    this.ctx.acceptWebSocket(pair[1]);
    pair[1].serializeAttachment({ expires: Math.min(expires, Date.now() + 300_000) } satisfies SocketInfo);
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  private send(ws: WebSocket, value: Record<string, unknown>): void {
    if ((ws.deserializeAttachment() as SocketInfo).expires <= Date.now() && value.type !== "error") {
      ws.close(4401, "reauth_required"); return;
    }
    const body = JSON.stringify({ version: 3, ...value });
    if (encoder.encode(body).length > HUB_FRAME_BYTES) fail("frame_too_large");
    try { ws.send(body); } catch { try { ws.close(1011, "send_failed"); } catch {} }
  }
  private peer(ws: WebSocket): Peer | undefined {
    return (ws.deserializeAttachment() as SocketInfo | undefined)?.peer;
  }
  private bind(ws: WebSocket, peer: Peer): void {
    const info = ws.deserializeAttachment() as SocketInfo;
    // Hibernation attachment limit is 2 KiB. Reserve full counter widths now,
    // before dispatch, rather than discovering a capacity failure mid-reply.
    const reserved = { ...info, peer: { ...peer, calls: peer.calls.map(c =>
      ({ ...c, next: Number.MAX_SAFE_INTEGER, ack: Number.MAX_SAFE_INTEGER, done: false,
        ...(c.input ? { input: { next: Number.MAX_SAFE_INTEGER, ack: Number.MAX_SAFE_INTEGER, done: false } } : {}) })) } };
    if (bytes(reserved) > 2048) fail("connection_capacity");
    ws.serializeAttachment({ ...info, peer } satisfies SocketInfo);
  }
  private sockets(): [WebSocket, Peer][] {
    return this.ctx.getWebSockets().flatMap(ws => {
      const peer = this.peer(ws);
      return peer && (ws.deserializeAttachment() as SocketInfo).expires > Date.now() &&
        ws.readyState === WebSocket.OPEN ? [[ws, peer] as [WebSocket, Peer]] : [];
    });
  }
  private broadcast(value: Record<string, unknown>): void {
    for (const [ws] of this.sockets()) this.send(ws, value);
  }
  private async token(route: Route): Promise<string> {
    const payload = base64(encoder.encode(JSON.stringify(route)));
    const signature = await crypto.subtle.sign("HMAC", await this.signing, encoder.encode(payload));
    return `${payload}.${base64(new Uint8Array(signature))}`;
  }
  private async route(token: unknown): Promise<Route> {
    if (typeof token !== "string" || token.length > 1024) fail("invalid_route");
    const fields = token.split(".");
    if (fields.length !== 2 || !await crypto.subtle.verify("HMAC", await this.signing, unbase64(fields[1]), encoder.encode(fields[0]))) fail("invalid_route");
    const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(unbase64(fields[0])));
    if (!object(value)) fail("invalid_route");
    keys(value, ["from", "to", "request", "nonce"]);
    if (!id(value.from) || !id(value.to) || !id(value.request) || !id(value.nonce)) fail("invalid_route");
    return value as unknown as Route;
  }
  private demand(): void {
    const peers = this.sockets();
    for (const [host, peer] of peers.filter(([, p]) => p.role === "host")) {
      const chats = [...new Set(peers.flatMap(([, p]) => p.topics).filter(chat => {
        const row = this.rows.row("chats", chat);
        return row && !row.deleted && row.fields.deviceId === peer.actor;
      }))].sort();
      const encoded = JSON.stringify(chats);
      if (this.demands.get(peer.connection) !== encoded) {
        this.send(host, { type: "demand", chats });
        this.demands.set(peer.connection, encoded);
      }
    }
  }

  webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    // Crypto awaits must not reorder consecutive frames from one peer.
    if (typeof message !== "string" || message.length > HUB_FRAME_BYTES) {
      ws.close(1009, "invalid_frame"); return Promise.resolve();
    }
    const size = encoder.encode(message).length;
    if (size > HUB_FRAME_BYTES || this.queuedMessages >= 64 || this.queuedBytes + size > 2 * 1024 * 1024) {
      ws.close(1013, "backpressure"); return Promise.resolve();
    }
    this.queuedMessages++; this.queuedBytes += size;
    const pending = this.messages.then(() => this.message(ws, message));
    this.messages = pending.catch(() => {}).finally(() => { this.queuedMessages--; this.queuedBytes -= size; });
    return pending;
  }
  private async message(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (ws.readyState !== WebSocket.OPEN) return;
    let request: unknown;
    let requestToken: unknown;
    try {
      if (typeof message !== "string") fail("binary_not_supported");
      const frame = decode(message);
      request = frame.id;
      requestToken = frame.token;
      if ((ws.deserializeAttachment() as SocketInfo).expires <= Date.now()) fail("reauth_required");
      let peer = this.peer(ws);
      if (!peer) {
        if (frame.type !== "hello") fail("hello_required");
        keys(frame, ["version", "type", "user", "org", "actor", "role", "after"]);
        if (frame.user !== this.rows.meta("user") || frame.org !== this.rows.meta("org")) fail("account_mismatch");
        if (!id(frame.actor) || !["host", "viewer"].includes(String(frame.role)) || !integer(frame.after)) fail("invalid_hello");
        const page = this.rows.page(frame.after);
        for (const [old, oldPeer] of this.sockets()) {
          if (oldPeer.actor === frame.actor && oldPeer.role === frame.role) old.close(4001, "replaced");
        }
        peer = { actor: frame.actor, role: frame.role as Peer["role"], connection: crypto.randomUUID(), topics: [], calls: [] };
        this.bind(ws, peer);
        this.send(ws, { type: "welcome", user: this.rows.meta("user"), org: this.rows.meta("org"), connection: peer.connection, leaseMs: LEASE_MS, ...page });
        for (const [connection, value] of this.presence) {
          if (value.expiresAt > Date.now()) this.send(ws, { type: "presence", connection, ...value });
        }
        this.demand();
        return;
      }
      switch (frame.type) {
        case "page":
          keys(frame, ["version", "type", "after"]);
          if (!integer(frame.after)) fail("invalid_cursor");
          this.send(ws, { type: "page", ...this.rows.page(frame.after) });
          break;
        case "push": {
          keys(frame, ["version", "type", "id", "ops"]);
          if (!id(frame.id)) fail("invalid_request");
          const ops = operations(frame.ops, peer.actor);
          // Bind the ACK to the exact transmitted bytes without accumulating
          // a durable receipt log for idempotent metadata LWW operations.
          const digest = await crypto.subtle.digest("SHA-256", encoder.encode(message as string));
          const requestHash = [...new Uint8Array(digest)].map(v => v.toString(16).padStart(2, "0")).join("");
          if (!this.sockets().some(([socket]) => socket === ws)) fail("connection_retired");
          const result = this.ctx.storage.transactionSync(() => {
            const before = new Map(ops.map(op => [`${op.kind}:${op.id}`, this.rows.row(op.kind, op.id)]));
            const result = this.rows.push(ops);
            this.notifications.observe(result.rows.map(after => ({ before: before.get(`${after.kind}:${after.id}`), after })), peer!.actor);
            return result;
          });
          this.send(ws, { type: "pushed", id: frame.id, requestHash, ...result });
          this.broadcast({ type: "changed", through: result.through });
          this.demand();
          break;
        }
        case "watch":
          keys(frame, ["version", "type", "chats"]);
          if (!Array.isArray(frame.chats) || frame.chats.length > MAX_TOPICS || !frame.chats.every(id)) fail("invalid_topics");
          peer.topics = [...new Set(frame.chats as string[])];
          this.bind(ws, peer);
          this.send(ws, { type: "watching", chats: peer.topics });
          this.demand();
          break;
        case "presence": {
          keys(frame, ["version", "type", "state"]);
          if (!object(frame.state) || bytes(frame.state) > RPC_BYTES) fail("invalid_presence");
          const value = { actor: peer.actor, role: peer.role,
            expiresAt: Math.min(Date.now() + LEASE_MS, (ws.deserializeAttachment() as SocketInfo).expires), state: frame.state };
          this.presence.set(peer.connection, value);
          this.broadcast({ type: "presence", connection: peer.connection, ...value });
          break;
        }
        case "probe":
          keys(frame, ["version", "type", "id"]);
          if (!id(frame.id)) fail("invalid_request");
          this.send(ws, { type: "probeOk", id: frame.id, through: this.rows.head() });
          break;
        case "call": {
          keys(frame, ["version", "type", "id", "target", "method", "params"], ["input"]);
          if (!id(frame.id) || !id(frame.target) || typeof frame.method !== "string" ||
              !/^[A-Za-z][A-Za-z0-9]{0,95}$/.test(frame.method) || bytes(frame.params) > RPC_BYTES ||
              (frame.input !== undefined && typeof frame.input !== "boolean")) fail("invalid_call");
          const target = this.sockets().find(([, p]) => p.actor === frame.target && p.role === "host");
          if (!target) fail("host_unavailable");
          if (peer.calls.length >= MAX_CALLS) fail("rpc_capacity");
          if (this.sockets().flatMap(([, p]) => p.calls).filter(c => c.host === target[1].connection).length >= MAX_CALLS)
            fail("host_capacity");
          if (peer.calls.some(c => c.id === frame.id)) fail("request_exists");
          const nonce = crypto.randomUUID();
          peer.calls.push({ id: frame.id, host: target[1].connection, nonce, next: 0, ack: 0, done: false,
            ...(frame.input === true ? { input: { next: 0, ack: 0, done: false } } : {}) });
          this.bind(ws, peer);
          try {
            const token = await this.token({ from: peer.connection, to: target[1].connection, request: frame.id, nonce });
            const current = this.sockets();
            if (!current.some(([socket]) => socket === ws) || !current.some(([socket]) => socket === target[0])) fail("connection_retired");
            // This receipt is a route, NOT confirmation of delivery/execution.
            this.send(ws, { type: "routed", id: frame.id, token, window: RPC_WINDOW });
            this.send(target[0], { type: "call", token, from: peer.actor, method: frame.method, params: frame.params,
              ...(frame.input === true ? { input: true } : {}), window: RPC_WINDOW });
          } catch (error) {
            const current = this.peer(ws);
            if (current) { current.calls = current.calls.filter(c => c.id !== frame.id); this.bind(ws, current); }
            throw error;
          }
          break;
        }
        case "input": {
          keys(frame, ["version", "type", "token", "sequence", "done", "value"]);
          if (!integer(frame.sequence) || typeof frame.done !== "boolean" || bytes(frame.value) > RPC_BYTES) fail("invalid_input");
          const route = await this.route(frame.token);
          if (route.from !== peer.connection) fail("not_rpc_caller");
          const current = this.sockets();
          if (!current.some(([socket]) => socket === ws)) fail("connection_retired");
          peer = this.peer(ws)!;
          const call = peer.calls.find(c => c.id === route.request && c.host === route.to && c.nonce === route.nonce);
          if (!call || call.done || !call.input || call.input.done) fail("request_closed");
          if (call.input.next !== frame.sequence || call.input.next >= 256) fail("rpc_sequence");
          if (call.input.next - call.input.ack >= RPC_WINDOW) fail("rpc_backpressure");
          const target = current.find(([, p]) => p.connection === route.to);
          if (!target) fail("host_unavailable");
          call.input.next++; call.input.done = frame.done;
          this.bind(ws, peer);
          this.send(target[0], { type: "input", token: frame.token, sequence: frame.sequence, done: frame.done, value: frame.value });
          break;
        }
        case "inputAck": {
          keys(frame, ["version", "type", "token", "through"]);
          if (peer.role !== "host" || !integer(frame.through)) fail("rpc_sequence");
          const route = await this.route(frame.token);
          if (route.to !== peer.connection) fail("not_rpc_host");
          const current = this.sockets();
          if (!current.some(([socket]) => socket === ws)) fail("connection_retired");
          const target = current.find(([, p]) => p.connection === route.from);
          if (!target) fail("caller_unavailable");
          const call = target[1].calls.find(c => c.id === route.request && c.host === route.to && c.nonce === route.nonce);
          if (!call || !call.input || frame.through < call.input.ack || frame.through > call.input.next) fail("rpc_sequence");
          call.input.ack = frame.through;
          this.bind(target[0], target[1]);
          this.send(target[0], { type: "inputCredit", id: route.request, token: frame.token, through: frame.through });
          break;
        }
        case "reply": {
          keys(frame, ["version", "type", "token", "sequence", "done", "value"]);
          if (peer.role !== "host" || !integer(frame.sequence) || typeof frame.done !== "boolean" || bytes(frame.value) > RPC_BYTES) fail("invalid_reply");
          const route = await this.route(frame.token);
          if (route.to !== peer.connection) fail("not_rpc_host");
          const current = this.sockets();
          if (!current.some(([socket]) => socket === ws)) fail("connection_retired");
          const target = current.find(([, p]) => p.connection === route.from);
          if (!target) fail("caller_unavailable");
          const call = target[1].calls.find(c => c.id === route.request && c.host === route.to && c.nonce === route.nonce);
          if (!call || call.done) fail("request_closed");
          // A host may reject malformed/oversized input immediately. Ordinary
          // streaming output cannot start while upload is incomplete.
          if (call.input && !call.input.done && !frame.done) fail("input_incomplete");
          if (call.next !== frame.sequence) fail("rpc_sequence");
          if (call.next - call.ack >= RPC_WINDOW) fail("rpc_backpressure");
          if (call.next === Number.MAX_SAFE_INTEGER) fail("sequence_exhausted");
          call.next++; call.done = frame.done;
          this.bind(target[0], target[1]);
          this.send(target[0], { type: "reply", id: route.request, sequence: frame.sequence, done: frame.done, value: frame.value });
          break;
        }
        case "ack": {
          keys(frame, ["version", "type", "token", "through"]);
          if (!integer(frame.through)) fail("rpc_sequence");
          const route = await this.route(frame.token);
          if (route.from !== peer.connection) fail("not_rpc_caller");
          peer = this.peer(ws)!;
          const call = peer.calls.find(c => c.id === route.request && c.host === route.to && c.nonce === route.nonce);
          if (!call || frame.through < call.ack || frame.through > call.next) fail("rpc_sequence");
          call.ack = frame.through;
          if (call.done && call.ack === call.next) peer.calls = peer.calls.filter(c => c !== call);
          this.bind(ws, peer);
          const target = this.sockets().find(([, p]) => p.connection === route.to);
          if (target) this.send(target[0], { type: "credit", token: frame.token, through: frame.through });
          break;
        }
        case "cancel": {
          keys(frame, ["version", "type", "token"]);
          const route = await this.route(frame.token);
          if (route.from !== peer.connection) fail("not_rpc_caller");
          peer = this.peer(ws)!;
          peer.calls = peer.calls.filter(c => !(c.id === route.request && c.nonce === route.nonce));
          this.bind(ws, peer);
          const target = this.sockets().find(([, p]) => p.connection === route.to);
          if (target) this.send(target[0], { type: "cancel", token: frame.token });
          break;
        }
        default: fail("unknown_message");
      }
    } catch (error) {
      this.send(ws, { type: "error", ...(id(request) ? { id: request } : {}),
        ...(typeof requestToken === "string" && requestToken.length <= 1024 ? { token: requestToken } : {}),
        code: error instanceof HubError ? error.message : "internal_error" });
      if (error instanceof HubError && error.message === "reauth_required") ws.close(4401, "reauth_required");
      if (!this.peer(ws)) ws.close(1008, "invalid_hello");
    }
  }
  async webSocketClose(ws: WebSocket): Promise<void> {
    try { ws.close(1000, "closed"); } catch {}
    const peer = this.peer(ws);
    if (!peer) return;
    const info = ws.deserializeAttachment() as SocketInfo;
    ws.serializeAttachment({ expires: info.expires } satisfies SocketInfo);
    this.presence.delete(peer.connection);
    this.demands.delete(peer.connection);
    this.broadcast({ type: "peerClosed", actor: peer.actor, connection: peer.connection });
    // Release remote stream/upload work when its caller disappears. Tokens
    // retain the original generation; a replacement caller is unaffected.
    for (const call of peer.calls) {
      const token = await this.token({ from: peer.connection, to: call.host, request: call.id, nonce: call.nonce });
      const host = this.sockets().find(([, p]) => p.connection === call.host);
      if (host) this.send(host[0], { type: "cancel", token });
    }
    for (const [socket, current] of this.sockets()) {
      const lost = current.calls.filter(call => call.host === peer.connection);
      if (lost.length) {
        current.calls = current.calls.filter(call => call.host !== peer.connection);
        this.bind(socket, current);
        for (const call of lost) this.send(socket, { type: "error", id: call.id, code: "delivery_unknown" });
      }
    }
    this.demand();
  }
  async webSocketError(ws: WebSocket): Promise<void> {
    try { ws.close(1011, "socket_error"); } catch {}
    await this.webSocketClose(ws);
  }
  private scheduleAlarm(): void {
    const schedule = async () => {
      const due = this.notifications.nextDue();
      if (due === undefined) await this.ctx.storage.deleteAlarm();
      else await this.ctx.storage.setAlarm(Math.max(Date.now() + 1000, due));
    };
    this.alarmScheduling = this.alarmScheduling.then(schedule, schedule);
    this.ctx.waitUntil(this.alarmScheduling);
  }
  async alarm(): Promise<void> {
    await this.notifications.flush();
    this.scheduleAlarm();
    await this.alarmScheduling;
  }
}

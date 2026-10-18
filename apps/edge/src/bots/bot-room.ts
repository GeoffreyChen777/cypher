/**
 * BotRoom — one Durable Object per networked bot (`bot1/{botRoomId}`,
 * docs/design/agent-bots.md §11): its public profile, its invites, its
 * connections to other users' bots, and the questions in flight between
 * them. The Edge relays text between bots and never runs a model.
 *
 * Ownership: the first `publish` claims the room for the caller; every
 * owner route after that is owner-only (the RegistryRoom/DeviceRoom rule).
 *
 * Cross-user traffic moves DO to DO over `/peer/*`, paths the Worker never
 * routes (room-routes.ts forwards only the owner routes), so one user's
 * bearer never reaches another user's room. A peer call names the calling
 * room, and the callee checks it against the connection it recorded.
 *
 * Delivery: bots hold no sockets. A room that queues an entry for its host
 * sends the host device's DeviceRoom a durable `bots` nudge, and the host
 * pulls `GET /inbox` and acknowledges what it has written down. An exchange
 * not settled 7 days after it was sent expires on both sides; settled rows
 * stay a day without bodies so the per-connection rate limits can count them.
 */
import { AUTH_USER_HEADER, json, type Env } from "../env";
import { ID_RE } from "../identifiers";
import {
  ANSWER_MAX_BYTES,
  ASKS_IN_FLIGHT,
  ASKS_PER_DAY,
  ASKS_PER_HOUR,
  DAY_MS,
  EXCHANGE_TTL_MS,
  HOUR_MS,
  INVITE_MAX_USES,
  INVITE_TTL_MS,
  QUESTION_MAX_BYTES,
  isFinal,
  splitInviteCode,
  utf8Bytes,
  type BotFrame,
  type WirePeer,
  type WireState
} from "./bot-frames";

/** Request bodies above this are refused before parsing. */
const BODY_MAX_BYTES = 96 * 1024;
const NAME_MAX = 80;
/** Entries one inbox pull returns per kind. */
const PULL_MAX = 100;
const INVITE_TTL_MIN_MS = 10 * 60_000;
const INVITE_TTL_MAX_MS = 30 * DAY_MS;
const INVITE_USES_MAX = 100;

interface Profile {
  name: string;
  ownerName: string;
}

interface ConnectionRow {
  id: string;
  peer_room: string;
  peer_name: string;
  peer_owner_name: string;
  created_at: number;
  revoked_at: number | null;
  invite_id: string | null;
  link_pending: string | null;
}

interface InboxRow {
  exchange_id: string;
  connection_id: string;
  text: string | null;
  state: WireState;
  sent_at: number;
}

interface OutboxRow {
  exchange_id: string;
  connection_id: string;
  state: WireState;
  text: string | null;
  sent_at: number;
  answered_at: number | null;
}

class Refusal extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string
  ) {
    super(message);
  }
}

const refuse = (status: number, code: string, message: string): never => {
  throw new Refusal(status, code, message);
};

const ok = (value: unknown = { ok: true }) => json(value);

const str = (value: unknown, what: string, max: number, min = 1): string => {
  if (typeof value !== "string") return refuse(400, "bad_request", `${what} is missing`);
  const trimmed = value.trim();
  if (trimmed.length < min || trimmed.length > max) return refuse(400, "bad_request", `${what} is empty or too long`);
  return trimmed;
};

const id = (value: unknown, what: string): string => {
  if (typeof value !== "string" || !ID_RE.test(value)) return refuse(400, "bad_request", `${what} is not a valid id`);
  return value;
};

const hex = (bytes: ArrayBuffer): string =>
  [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");

const sha256 = async (text: string): Promise<string> =>
  hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));

const randomSecret = (): string => {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

export class BotRoom implements DurableObject {
  private readonly ctx: DurableObjectState;
  private readonly env: Env;

  constructor(ctx: DurableObjectState, env: Env) {
    this.ctx = ctx;
    this.env = env;
    const sql = ctx.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
    sql.exec(
      `CREATE TABLE IF NOT EXISTS invites (
        id TEXT PRIMARY KEY, hash TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL, max_uses INTEGER NOT NULL, uses INTEGER NOT NULL DEFAULT 0,
        revoked_at INTEGER)`
    );
    sql.exec(
      `CREATE TABLE IF NOT EXISTS connections (
        id TEXT PRIMARY KEY, peer_room TEXT NOT NULL, peer_name TEXT NOT NULL,
        peer_owner_name TEXT NOT NULL, created_at INTEGER NOT NULL, revoked_at INTEGER,
        invite_id TEXT, link_pending TEXT)`
    );
    // Questions addressed to this bot. `text` is dropped once the host has it.
    sql.exec(
      `CREATE TABLE IF NOT EXISTS inbox (
        exchange_id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, text TEXT,
        state TEXT NOT NULL, sent_at INTEGER NOT NULL, acked INTEGER NOT NULL DEFAULT 0)`
    );
    // Questions this bot asked; `text` is the answer, dropped once acked.
    sql.exec(
      `CREATE TABLE IF NOT EXISTS outbox (
        exchange_id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, state TEXT NOT NULL,
        text TEXT, sent_at INTEGER NOT NULL, answered_at INTEGER, acked_state TEXT)`
    );
  }

  // ── storage helpers ───────────────────────────────────────────────────────

  private meta(key: string): string | undefined {
    const rows = [...this.ctx.storage.sql.exec("SELECT value FROM meta WHERE key = ?", key)];
    return rows[0]?.value as string | undefined;
  }

  private setMeta(key: string, value: string): void {
    this.ctx.storage.sql.exec(
      "INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
      key,
      value
    );
  }

  private rows<T>(query: string, ...bindings: unknown[]): T[] {
    return [...this.ctx.storage.sql.exec(query, ...bindings)] as unknown as T[];
  }

  private profile(): Profile | undefined {
    const raw = this.meta("profile");
    return raw ? (JSON.parse(raw) as Profile) : undefined;
  }

  private self(): WirePeer {
    const profile = this.profile() ?? refuse(404, "not_published", "This bot is not published.");
    return { botRoomId: this.meta("room") ?? "", name: profile.name, ownerName: profile.ownerName };
  }

  private connection(connectionId: string): ConnectionRow | undefined {
    return this.rows<ConnectionRow>("SELECT * FROM connections WHERE id = ?", connectionId)[0];
  }

  /** A live connection recorded with `peerRoom` (a peer call's check). */
  private peerConnection(connectionId: unknown, peerRoom: unknown): ConnectionRow {
    const row = typeof connectionId === "string" ? this.connection(connectionId) : undefined;
    if (!row || row.peer_room !== peerRoom || row.revoked_at !== null) {
      return refuse(404, "no_connection", "These bots are not connected.");
    }
    return row;
  }

  /** Refuse a question over the per-connection limits on this side. */
  private checkLimits(table: "inbox" | "outbox", connectionId: string, now: number): void {
    const [counts] = this.rows<{ in_flight: number; hour: number; day: number }>(
      `SELECT
        COALESCE(SUM(CASE WHEN state NOT IN ('answered', 'declined', 'expired') THEN 1 ELSE 0 END), 0) AS in_flight,
        COALESCE(SUM(CASE WHEN sent_at > ? THEN 1 ELSE 0 END), 0) AS hour,
        COALESCE(SUM(CASE WHEN sent_at > ? THEN 1 ELSE 0 END), 0) AS day
      FROM ${table} WHERE connection_id = ?`,
      now - HOUR_MS,
      now - DAY_MS,
      connectionId
    );
    if (!counts) return;
    if (counts.in_flight >= ASKS_IN_FLIGHT) {
      refuse(429, "too_many_in_flight", `At most ${ASKS_IN_FLIGHT} questions can wait for an answer on one connection.`);
    }
    if (counts.hour >= ASKS_PER_HOUR) refuse(429, "rate_limited", `At most ${ASKS_PER_HOUR} questions an hour on one connection.`);
    if (counts.day >= ASKS_PER_DAY) refuse(429, "rate_limited", `At most ${ASKS_PER_DAY} questions a day on one connection.`);
  }

  /** Expire unsettled exchanges on `connectionId` (a revoked connection). */
  private expireConnection(connectionId: string, now: number): void {
    const sql = this.ctx.storage.sql;
    sql.exec(
      "UPDATE inbox SET state = 'expired', text = NULL WHERE connection_id = ? AND state NOT IN ('answered', 'declined', 'expired')",
      connectionId
    );
    sql.exec(
      `UPDATE outbox SET state = 'expired', text = NULL, answered_at = ?
       WHERE connection_id = ? AND state NOT IN ('answered', 'declined', 'expired')`,
      now,
      connectionId
    );
  }

  /** Expire what is past its time, prune settled rows, and arm the alarm
   * for the next deadline. Runs on every request and on the alarm. */
  private async sweep(now: number): Promise<void> {
    const sql = this.ctx.storage.sql;
    const deadline = now - EXCHANGE_TTL_MS;
    sql.exec(
      "UPDATE inbox SET state = 'expired', text = NULL WHERE sent_at <= ? AND state NOT IN ('answered', 'declined', 'expired')",
      deadline
    );
    sql.exec(
      `UPDATE outbox SET state = 'expired', text = NULL, answered_at = ?
       WHERE sent_at <= ? AND state NOT IN ('answered', 'declined', 'expired')`,
      now,
      deadline
    );
    // Settled rows the host has seen stay a day, without bodies, for the
    // daily limit; a settled answer the host never pulls goes after 7 days.
    sql.exec(
      "DELETE FROM inbox WHERE state IN ('answered', 'declined', 'expired') AND (acked = 1 OR sent_at <= ?) AND sent_at <= ?",
      deadline,
      now - DAY_MS
    );
    sql.exec(
      `DELETE FROM outbox WHERE state IN ('answered', 'declined', 'expired')
       AND (acked_state = state OR COALESCE(answered_at, sent_at) <= ?) AND sent_at <= ?`,
      deadline,
      now - DAY_MS
    );
    sql.exec("DELETE FROM invites WHERE expires_at <= ?", now - 30 * DAY_MS);
    // The alarm only expires; settled rows are pruned by whatever request
    // or alarm comes next.
    const [next] = this.rows<{ at: number | null }>(
      `SELECT MIN(sent_at) + ? AS at FROM (
        SELECT sent_at FROM inbox WHERE state NOT IN ('answered', 'declined', 'expired')
        UNION ALL SELECT sent_at FROM outbox WHERE state NOT IN ('answered', 'declined', 'expired'))`,
      EXCHANGE_TTL_MS
    );
    if (next?.at != null) {
      const at = Math.max(next.at, now + 1_000);
      const current = await this.ctx.storage.getAlarm();
      if (current === null || current > at || current <= now) await this.ctx.storage.setAlarm(at);
    }
  }

  async alarm(): Promise<void> {
    await this.sweep(Date.now());
  }

  /** Tell the host device a new entry waits: a durable nudge to its
   * DeviceRoom, sent as the owner (the DeviceRoom accepts only its owner,
   * so a room can only ever nudge its own owner's devices). */
  private async nudgeHost(): Promise<void> {
    const host = this.meta("host");
    const owner = this.meta("owner");
    const room = this.meta("room");
    if (!host || !owner || !room || !ID_RE.test(host)) return;
    try {
      const stub = this.env.DEVICE_ROOMS.get(this.env.DEVICE_ROOMS.idFromName(`d2/${host}`));
      await stub.fetch(
        new Request("https://device/nudge", {
          method: "POST",
          headers: { [AUTH_USER_HEADER]: owner, "content-type": "application/json" },
          body: JSON.stringify({ botRoomId: room })
        })
      );
    } catch {
      // Best effort: the host also pulls every minute while it waits.
    }
  }

  /** Call `path` on another bot's room. */
  private peerCall(peerRoom: string, path: string, body: unknown): Promise<Response> {
    const stub = this.env.BOT_ROOMS.get(this.env.BOT_ROOMS.idFromName(`bot1/${peerRoom}`));
    return stub.fetch(
      new Request(`https://bot/peer/${path}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body)
      })
    );
  }

  // ── entry ─────────────────────────────────────────────────────────────────

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    try {
      const text = await request.text();
      if (utf8Bytes(text) > BODY_MAX_BYTES) refuse(413, "too_large", "The request is too large.");
      const body = (text ? JSON.parse(text) : {}) as Record<string, unknown>;
      const now = Date.now();
      await this.sweep(now);
      if (url.pathname.startsWith("/peer/")) {
        // Only other BotRooms call these; the Worker never forwards here and
        // always stamps the user header on what it does forward.
        if (request.headers.has(AUTH_USER_HEADER)) return json({ error: "not_found" }, 404);
        return await this.peer(url.pathname.slice("/peer/".length), body, now);
      }
      const userId = request.headers.get(AUTH_USER_HEADER);
      if (!userId) return json({ error: "unauthenticated" }, 401);
      const room = url.searchParams.get("room") ?? "";
      if (!ID_RE.test(room)) return json({ error: "bad_room" }, 400);
      return await this.owner(request.method, url.pathname, body, userId, room, now);
    } catch (error) {
      if (error instanceof Refusal) {
        return json({ error: error.code, message: error.message }, error.status);
      }
      if (error instanceof SyntaxError) return json({ error: "bad_request", message: "The body is not JSON." }, 400);
      throw error;
    }
  }

  // ── owner routes ──────────────────────────────────────────────────────────

  private async owner(
    method: string,
    path: string,
    body: Record<string, unknown>,
    userId: string,
    room: string,
    now: number
  ): Promise<Response> {
    const owner = this.meta("owner");
    if (path === "/publish" && method === "POST") {
      if (owner && owner !== userId) refuse(403, "forbidden", "This bot room belongs to someone else.");
      const profile: Profile = {
        name: str(body.name, "name", NAME_MAX),
        ownerName: str(body.ownerName ?? "", "ownerName", NAME_MAX, 0)
      };
      const host = id(body.hostDeviceId, "hostDeviceId");
      if (!owner) this.setMeta("owner", userId);
      this.setMeta("room", room);
      this.setMeta("profile", JSON.stringify(profile));
      this.setMeta("host", host);
      this.setMeta("paused", body.paused === true ? "1" : "0");
      return ok({ roomId: room });
    }
    if (!owner) refuse(404, "not_published", "This bot is not published.");
    if (owner !== userId) refuse(403, "forbidden", "This bot room belongs to someone else.");

    const segments = path.split("/").filter(Boolean);
    const [head, tail] = segments;
    if (head === "inbox" && method === "GET") return ok({ entries: this.pull() });
    if (head === "invites" && method === "POST" && !tail) return ok(await this.createInvite(body, room, now));
    if (head === "invites" && method === "DELETE" && tail) {
      const changed = this.ctx.storage.sql.exec(
        "UPDATE invites SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?",
        now,
        tail
      ).rowsWritten;
      if (changed === 0) refuse(404, "no_invite", "No such invite.");
      return ok();
    }
    if (head === "connect" && method === "POST") return ok(await this.connectPeer(body, room, userId));
    if (head === "connections" && method === "DELETE" && tail) return ok(await this.revoke(tail, room, now));
    if (head === "ask" && method === "POST") return ok(await this.ask(body, room, now));
    if (head === "answer" && method === "POST") return ok(await this.answer(body, room, now));
    if (head === "state" && method === "POST") return ok(await this.state(body, room));
    if (head === "ack" && method === "POST") return ok(await this.ack(body, room));
    return json({ error: "not_found" }, 404);
  }

  /** Everything the host has not acknowledged yet: link changes, questions
   * for it, and the progress and answers of its own questions. */
  private pull(): BotFrame[] {
    const frames: BotFrame[] = [];
    for (const row of this.rows<ConnectionRow>(
      "SELECT * FROM connections WHERE link_pending IS NOT NULL ORDER BY created_at LIMIT ?",
      PULL_MAX
    )) {
      const op = row.link_pending === "revoked" ? "revoked" : "added";
      frames.push({
        t: "link",
        connectionId: row.id,
        op,
        ...(op === "added"
          ? { peer: { botRoomId: row.peer_room, name: row.peer_name, ownerName: row.peer_owner_name } }
          : {}),
        ...(op === "added" && row.invite_id ? { inviteId: row.invite_id } : {})
      });
    }
    for (const row of this.rows<InboxRow>(
      "SELECT * FROM inbox WHERE acked = 0 AND text IS NOT NULL AND state = 'queued' ORDER BY sent_at LIMIT ?",
      PULL_MAX
    )) {
      frames.push({ t: "ask", exchangeId: row.exchange_id, connectionId: row.connection_id, text: row.text ?? "", sentAt: row.sent_at });
    }
    for (const row of this.rows<OutboxRow>(
      "SELECT * FROM outbox WHERE acked_state IS NULL OR acked_state != state ORDER BY sent_at LIMIT ?",
      PULL_MAX
    )) {
      if (isFinal(row.state)) {
        frames.push({
          t: "answer",
          exchangeId: row.exchange_id,
          connectionId: row.connection_id,
          status: row.state as "answered" | "declined" | "expired",
          ...(row.text === null ? {} : { text: row.text }),
          answeredAt: row.answered_at ?? row.sent_at
        });
      } else {
        frames.push({ t: "state", exchangeId: row.exchange_id, state: row.state });
      }
    }
    return frames;
  }

  private async createInvite(body: Record<string, unknown>, room: string, now: number) {
    const ttl = typeof body.ttlMs === "number" ? body.ttlMs : INVITE_TTL_MS;
    const uses = typeof body.maxUses === "number" ? Math.floor(body.maxUses) : INVITE_MAX_USES;
    const expiresAt = now + Math.min(Math.max(ttl, INVITE_TTL_MIN_MS), INVITE_TTL_MAX_MS);
    const maxUses = Math.min(Math.max(uses, 1), INVITE_USES_MAX);
    const inviteId = crypto.randomUUID();
    const secret = randomSecret();
    this.ctx.storage.sql.exec(
      "INSERT INTO invites (id, hash, created_at, expires_at, max_uses) VALUES (?, ?, ?, ?, ?)",
      inviteId,
      await sha256(secret),
      now,
      expiresAt,
      maxUses
    );
    return { inviteId, code: `${room}.${secret}`, createdAt: now, expiresAt, maxUses };
  }

  /** Redeem a peer's invite code on behalf of this bot. */
  private async connectPeer(body: Record<string, unknown>, room: string, userId: string) {
    const code = str(body.code, "code", 300);
    const split = splitInviteCode(code) ?? refuse(400, "bad_code", "That is not an invite code.");
    const [peerRoom, secret] = split;
    if (peerRoom === room) refuse(400, "self", "A bot can't connect to itself.");
    const self = this.self();
    const response = await this.peerCall(peerRoom, "redeem", { secret, room, owner: userId, peer: self });
    const reply = (await response.json()) as Record<string, unknown>;
    if (!response.ok) {
      refuse(response.status, String(reply.error ?? "failed"), String(reply.message ?? "The invite could not be redeemed."));
    }
    const peer = reply.peer as WirePeer;
    const connectionId = id(reply.connectionId, "connectionId");
    const createdAt = Number(reply.createdAt);
    this.ctx.storage.sql.exec(
      `INSERT INTO connections (id, peer_room, peer_name, peer_owner_name, created_at, link_pending)
       VALUES (?, ?, ?, ?, ?, 'added') ON CONFLICT(id) DO NOTHING`,
      connectionId,
      peerRoom,
      peer.name,
      peer.ownerName,
      createdAt
    );
    return { connectionId, peer: { ...peer, botRoomId: peerRoom }, createdAt };
  }

  private async revoke(connectionId: string, room: string, now: number) {
    const row = this.connection(connectionId) ?? refuse(404, "no_connection", "No such connection.");
    if (row.revoked_at === null) {
      this.ctx.storage.sql.exec("UPDATE connections SET revoked_at = ?, link_pending = NULL WHERE id = ?", now, connectionId);
      this.expireConnection(connectionId, now);
      try {
        await this.peerCall(row.peer_room, "revoke", { connectionId, room });
      } catch {
        // The peer keeps a dangling connection, but this room refuses every
        // question on it.
      }
    }
    return { ok: true };
  }

  private async ask(body: Record<string, unknown>, room: string, now: number) {
    const exchangeId = id(body.exchangeId, "exchangeId");
    const connectionId = id(body.connectionId, "connectionId");
    const text = str(body.text, "text", BODY_MAX_BYTES);
    if (utf8Bytes(text) > QUESTION_MAX_BYTES) refuse(413, "too_large", "Questions are at most 8 KiB.");
    const row = this.connection(connectionId);
    if (!row || row.revoked_at !== null) refuse(404, "no_connection", "These bots are not connected.");
    const existing = this.rows<OutboxRow>("SELECT * FROM outbox WHERE exchange_id = ?", exchangeId)[0];
    if (existing) return { exchangeId, sentAt: existing.sent_at };
    this.checkLimits("outbox", connectionId, now);
    const response = await this.peerCall(row!.peer_room, "ask", { connectionId, room, exchangeId, text, sentAt: now });
    if (!response.ok) {
      const reply = (await response.json()) as Record<string, unknown>;
      refuse(response.status, String(reply.error ?? "failed"), String(reply.message ?? "The question could not be sent."));
    }
    this.ctx.storage.sql.exec(
      "INSERT INTO outbox (exchange_id, connection_id, state, sent_at) VALUES (?, ?, 'queued', ?)",
      exchangeId,
      connectionId,
      now
    );
    await this.sweep(now);
    return { exchangeId, sentAt: now };
  }

  private async answer(body: Record<string, unknown>, room: string, now: number) {
    const exchangeId = id(body.exchangeId, "exchangeId");
    const status = body.status;
    if (status !== "answered" && status !== "declined") refuse(400, "bad_request", "status is answered or declined");
    let text: string | undefined;
    if (status === "answered") {
      text = str(body.text, "text", BODY_MAX_BYTES);
      if (utf8Bytes(text) > ANSWER_MAX_BYTES) refuse(413, "too_large", "Answers are at most 64 KiB.");
    }
    const row =
      this.rows<InboxRow>("SELECT * FROM inbox WHERE exchange_id = ?", exchangeId)[0] ??
      refuse(404, "no_exchange", "No such question.");
    if (isFinal(row.state)) refuse(409, "settled", `This question is already ${row.state}.`);
    const connection = this.connection(row.connection_id);
    let delivered = false;
    if (connection && connection.revoked_at === null) {
      const response = await this.peerCall(connection.peer_room, "answer", {
        exchangeId,
        connectionId: row.connection_id,
        room,
        status,
        ...(text === undefined ? {} : { text }),
        answeredAt: now
      });
      // A peer that is unreachable keeps the question open for a retry; one
      // that refuses (revoked, expired) settles it here too.
      if (response.status >= 500) refuse(502, "peer_unavailable", "The asking bot's room did not answer; try again.");
      delivered = response.ok;
    }
    this.ctx.storage.sql.exec("UPDATE inbox SET state = ?, text = NULL WHERE exchange_id = ?", status, exchangeId);
    return { ok: true, delivered };
  }

  private async state(body: Record<string, unknown>, room: string) {
    const exchangeId = id(body.exchangeId, "exchangeId");
    const state = body.state;
    if (state !== "working" && state !== "awaiting_review") refuse(400, "bad_request", "state is working or awaiting_review");
    const row =
      this.rows<InboxRow>("SELECT * FROM inbox WHERE exchange_id = ?", exchangeId)[0] ??
      refuse(404, "no_exchange", "No such question.");
    if (isFinal(row.state)) refuse(409, "settled", `This question is already ${row.state}.`);
    this.ctx.storage.sql.exec("UPDATE inbox SET state = ? WHERE exchange_id = ?", state, exchangeId);
    await this.forwardState(row.connection_id, exchangeId, state as WireState, room);
    return { ok: true };
  }

  private async forwardState(connectionId: string, exchangeId: string, state: WireState, room: string) {
    const connection = this.connection(connectionId);
    if (!connection || connection.revoked_at !== null) return;
    try {
      await this.peerCall(connection.peer_room, "state", { exchangeId, connectionId, room, state });
    } catch {
      // Progress is advisory; the answer itself is what must arrive.
    }
  }

  private async ack(body: Record<string, unknown>, room: string) {
    const sql = this.ctx.storage.sql;
    const list = (value: unknown): unknown[] => (Array.isArray(value) ? value.slice(0, 500) : []);
    for (const exchangeId of list(body.asks)) {
      if (typeof exchangeId !== "string") continue;
      const row = this.rows<InboxRow>("SELECT * FROM inbox WHERE exchange_id = ? AND acked = 0", exchangeId)[0];
      if (!row) continue;
      sql.exec("UPDATE inbox SET acked = 1, text = NULL WHERE exchange_id = ?", exchangeId);
      if (row.state === "queued") {
        sql.exec("UPDATE inbox SET state = 'delivered' WHERE exchange_id = ?", exchangeId);
        await this.forwardState(row.connection_id, exchangeId, "delivered", room);
      }
    }
    for (const entry of list(body.states)) {
      const { exchangeId, state } = (entry ?? {}) as { exchangeId?: unknown; state?: unknown };
      if (typeof exchangeId !== "string" || typeof state !== "string") continue;
      sql.exec(
        `UPDATE outbox SET acked_state = state,
           text = CASE WHEN state IN ('answered', 'declined', 'expired') THEN NULL ELSE text END
         WHERE exchange_id = ? AND state = ?`,
        exchangeId,
        state
      );
    }
    for (const connectionId of list(body.links)) {
      if (typeof connectionId !== "string") continue;
      sql.exec("UPDATE connections SET link_pending = NULL WHERE id = ?", connectionId);
    }
    return { ok: true };
  }

  // ── peer routes (DO to DO) ────────────────────────────────────────────────

  private async peer(path: string, body: Record<string, unknown>, now: number): Promise<Response> {
    const sql = this.ctx.storage.sql;
    const owner = this.meta("owner");
    switch (path) {
      case "redeem": {
        const invalid = () => refuse(404, "invite_invalid", "This invite code is invalid, expired or used up.");
        if (!owner || typeof body.secret !== "string" || typeof body.room !== "string") return invalid();
        if (body.owner === owner) refuse(403, "same_owner", "Both bots belong to you; connect bots of different people.");
        const peer = body.peer as WirePeer | undefined;
        if (!peer || typeof peer.name !== "string") refuse(400, "bad_request", "The redeeming bot has no profile.");
        const invite = this.rows<{ id: string; expires_at: number; max_uses: number; uses: number; revoked_at: number | null }>(
          "SELECT * FROM invites WHERE hash = ?",
          await sha256(body.secret)
        )[0];
        if (!invite || invite.revoked_at !== null || invite.expires_at <= now || invite.uses >= invite.max_uses) return invalid();
        const live = this.rows<ConnectionRow>(
          "SELECT * FROM connections WHERE peer_room = ? AND revoked_at IS NULL",
          body.room
        )[0];
        if (live) refuse(409, "already_connected", "These bots are already connected.");
        const self = this.self();
        const connectionId = crypto.randomUUID();
        sql.exec("UPDATE invites SET uses = uses + 1 WHERE id = ?", invite.id);
        sql.exec(
          `INSERT INTO connections (id, peer_room, peer_name, peer_owner_name, created_at, invite_id, link_pending)
           VALUES (?, ?, ?, ?, ?, ?, 'added')`,
          connectionId,
          body.room,
          String(peer!.name).slice(0, NAME_MAX),
          String(peer!.ownerName ?? "").slice(0, NAME_MAX),
          now,
          invite.id
        );
        await this.nudgeHost();
        return ok({ connectionId, peer: self, createdAt: now });
      }
      case "revoke": {
        const row = typeof body.connectionId === "string" ? this.connection(body.connectionId) : undefined;
        if (!row || row.peer_room !== body.room) return ok({ ok: false });
        if (row.revoked_at === null) {
          sql.exec("UPDATE connections SET revoked_at = ?, link_pending = 'revoked' WHERE id = ?", now, row.id);
          this.expireConnection(row.id, now);
          await this.nudgeHost();
        }
        return ok();
      }
      case "ask": {
        const connection = this.peerConnection(body.connectionId, body.room);
        const exchangeId = id(body.exchangeId, "exchangeId");
        const text = str(body.text, "text", QUESTION_MAX_BYTES);
        if (utf8Bytes(text) > QUESTION_MAX_BYTES) refuse(413, "too_large", "Questions are at most 8 KiB.");
        if (this.rows("SELECT 1 FROM inbox WHERE exchange_id = ?", exchangeId).length > 0) return ok();
        if (this.meta("paused") === "1") refuse(503, "paused", "This bot is paused and isn't taking questions.");
        this.checkLimits("inbox", connection.id, now);
        const sentAt = typeof body.sentAt === "number" ? body.sentAt : now;
        sql.exec(
          "INSERT INTO inbox (exchange_id, connection_id, text, state, sent_at) VALUES (?, ?, ?, 'queued', ?)",
          exchangeId,
          connection.id,
          text,
          sentAt
        );
        await this.sweep(now);
        await this.nudgeHost();
        return ok();
      }
      case "answer": {
        const connection = this.peerConnection(body.connectionId, body.room);
        const exchangeId = id(body.exchangeId, "exchangeId");
        const row =
          this.rows<OutboxRow>("SELECT * FROM outbox WHERE exchange_id = ? AND connection_id = ?", exchangeId, connection.id)[0] ??
          refuse(404, "no_exchange", "No such question.");
        if (isFinal(row.state)) return ok({ ok: row.state === body.status });
        const status = body.status === "answered" ? "answered" : "declined";
        const text = status === "answered" && typeof body.text === "string" ? body.text.slice(0, ANSWER_MAX_BYTES) : null;
        sql.exec(
          "UPDATE outbox SET state = ?, text = ?, answered_at = ? WHERE exchange_id = ?",
          status,
          text,
          typeof body.answeredAt === "number" ? body.answeredAt : now,
          exchangeId
        );
        await this.nudgeHost();
        return ok();
      }
      case "state": {
        const connection = this.peerConnection(body.connectionId, body.room);
        const state = body.state;
        if (state !== "delivered" && state !== "working" && state !== "awaiting_review") return ok({ ok: false });
        sql.exec(
          `UPDATE outbox SET state = ? WHERE exchange_id = ? AND connection_id = ?
           AND state NOT IN ('answered', 'declined', 'expired')`,
          state,
          body.exchangeId,
          connection.id
        );
        return ok();
      }
      default:
        return json({ error: "not_found" }, 404);
    }
  }
}

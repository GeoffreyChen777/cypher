/**
 * TrustRoom — one Durable Object per user (`t1/{userId}`): the user's device
 * trust chain and the short-lived pairing mailboxes (crates/trust).
 *
 *   GET  /chain                       → { entries }
 *   POST /chain { entry }             → append; 409 { length } if `entry.seq`
 *                                       is not the next one, 400 if invalid
 *   GET|PUT /pairing/:mailbox/:slot   → one pairing message (`join` | `welcome`)
 *   DELETE  /pairing/:mailbox
 *   GET  /head?subscribe=<room>       → { length }; room-to-room only
 *
 * Chain-head pushes (T1, docs/design/do-cost.md): a RegistryRoom subscribes
 * with `GET /head?subscribe=reg1/{org}/{user}`; after every append this room
 * POSTs `{ length }` to each subscriber's `/trust-head`, and retries the ones
 * that failed from an alarm (5 s, doubling to 5 min). The push is only a hint
 * — devices still read and verify the chain over HTTPS — so a lost one only
 * delays a sync to the device's safety poll. Room-to-room paths refuse any
 * request carrying the user header: the Worker stamps it on every request it
 * forwards, so clients cannot reach them.
 *
 * Appends are serialized and validated in full (trust-chain.ts), so the log
 * here is always well formed; devices verify it again, and accept only logs
 * that extend the one they hold. Mailbox messages are opaque to the edge —
 * MACed with a code it never sees — and expire after `MAILBOX_TTL_MS`.
 */
import { AUTH_USER_HEADER, json, type Env } from "../env";
import { applyEntry, emptyState, parseEntry, type ChainState } from "./trust-chain";

const MAX_BODY = 16 * 1024;
const MAILBOX_TTL_MS = 15 * 60_000;
const MAX_MAILBOXES = 64;
const MAILBOX_RE = /^[0-9a-f]{32}$/;
const SLOTS = new Set(["join", "welcome"]);
/** A subscriber is a RegistryRoom name (room-routes.ts). */
const SUBSCRIBER_RE = /^reg1\/[^/]{1,200}\/[^/]{1,200}$/;
const MAX_SUBSCRIBERS = 64;
const RETRY_BASE_MS = 5_000;
const RETRY_CAP_MS = 5 * 60_000;

const readBody = async (request: Request): Promise<string | undefined> => {
  const text = await request.text();
  return new TextEncoder().encode(text).length > MAX_BODY ? undefined : text;
};

export class TrustRoom implements DurableObject {
  private readonly ctx: DurableObjectState;
  private readonly env: Env;
  private state?: ChainState;

  constructor(ctx: DurableObjectState, env: Env) {
    this.ctx = ctx;
    this.env = env;
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS entries (seq INTEGER PRIMARY KEY, body TEXT NOT NULL)");
    ctx.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS mailboxes (id TEXT NOT NULL, slot TEXT NOT NULL, body TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (id, slot))"
    );
    // `pending` = the last head push to this room failed; `kv.retryMs` is
    // the alarm's current backoff.
    ctx.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS subscribers (room TEXT PRIMARY KEY, pending INTEGER NOT NULL DEFAULT 0)"
    );
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
  }

  private entries(): unknown[] {
    return [...this.ctx.storage.sql.exec("SELECT body FROM entries ORDER BY seq")].map((row) =>
      JSON.parse(row.body as string)
    );
  }

  /** The membership the stored log yields; entries were verified on append. */
  private async chainState(): Promise<ChainState> {
    if (this.state) return this.state;
    const state = emptyState();
    for (const value of this.entries()) {
      const entry = parseEntry(value);
      if (!entry || (await applyEntry(state, entry, { signatures: false }))) {
        throw new Error("stored trust chain is corrupt");
      }
    }
    this.state = state;
    return state;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/head") return this.subscribe(request, url);
    if (!request.headers.get(AUTH_USER_HEADER)) return json({ error: "unauthenticated" }, 401);
    const parts = url.pathname.split("/").filter(Boolean);

    if (parts.length === 1 && parts[0] === "chain") {
      if (request.method === "GET") return json({ entries: this.entries() });
      if (request.method === "POST") return this.append(request);
      return json({ error: "method not allowed" }, 405);
    }

    if (parts[0] === "pairing" && parts[1] && MAILBOX_RE.test(parts[1])) {
      const mailbox = parts[1];
      this.ctx.storage.sql.exec("DELETE FROM mailboxes WHERE at < ?", Date.now() - MAILBOX_TTL_MS);
      if (parts.length === 2 && request.method === "DELETE") {
        this.ctx.storage.sql.exec("DELETE FROM mailboxes WHERE id = ?", mailbox);
        return json({ ok: true });
      }
      const slot = parts[2];
      if (parts.length === 3 && slot && SLOTS.has(slot)) {
        if (request.method === "GET") {
          const [row] = [
            ...this.ctx.storage.sql.exec("SELECT body FROM mailboxes WHERE id = ? AND slot = ?", mailbox, slot)
          ];
          return row ? new Response(row.body as string, { headers: { "content-type": "application/json" } }) : json({ error: "not_found" }, 404);
        }
        if (request.method === "PUT") {
          const body = await readBody(request);
          if (body === undefined) return json({ error: "too_large" }, 413);
          try {
            JSON.parse(body);
          } catch {
            return json({ error: "bad_json" }, 400);
          }
          const [count] = [...this.ctx.storage.sql.exec("SELECT COUNT(DISTINCT id) AS n FROM mailboxes")];
          const [exists] = [...this.ctx.storage.sql.exec("SELECT 1 FROM mailboxes WHERE id = ? LIMIT 1", mailbox)];
          if (!exists && Number(count?.n ?? 0) >= MAX_MAILBOXES) return json({ error: "too_many_pairings" }, 429);
          this.ctx.storage.sql.exec(
            "INSERT INTO mailboxes (id, slot, body, at) VALUES (?, ?, ?, ?) ON CONFLICT(id, slot) DO UPDATE SET body = excluded.body, at = excluded.at",
            mailbox,
            slot,
            body,
            Date.now()
          );
          return json({ ok: true });
        }
      }
    }
    return json({ error: "not_found" }, 404);
  }

  private async append(request: Request): Promise<Response> {
    const body = await readBody(request);
    if (body === undefined) return json({ error: "too_large" }, 413);
    let entry;
    try {
      entry = parseEntry((JSON.parse(body) as { entry?: unknown }).entry);
    } catch {
      entry = undefined;
    }
    if (!entry) return json({ error: "invalid", reason: "malformed entry" }, 400);
    const valid = entry;
    // One append at a time: validation awaits WebCrypto, and two appends of
    // the same sequence number must not both pass.
    return this.ctx.blockConcurrencyWhile(async () => {
      const current = await this.chainState();
      if (valid.seq !== current.length) return json({ error: "conflict", length: current.length }, 409);
      const next: ChainState = {
        length: current.length,
        head: current.head,
        members: new Map(current.members),
        removed: new Set(current.removed),
        keysSeen: new Set(current.keysSeen),
        requirePaired: current.requirePaired,
        receiveOnly: new Set(current.receiveOnly)
      };
      const refused = await applyEntry(next, valid);
      if (refused) return json({ error: "invalid", reason: refused }, 400);
      this.ctx.storage.sql.exec("INSERT INTO entries (seq, body) VALUES (?, ?)", valid.seq, JSON.stringify(valid));
      this.state = next;
      this.ctx.waitUntil(this.pushHead(false));
      return json({ length: next.length });
    });
  }

  /** `GET /head?subscribe=<room>`: room-to-room only (no user header). */
  private async subscribe(request: Request, url: URL): Promise<Response> {
    if (request.headers.get(AUTH_USER_HEADER)) return json({ error: "not_found" }, 404);
    if (request.method !== "GET") return json({ error: "method not allowed" }, 405);
    const room = url.searchParams.get("subscribe") ?? "";
    if (!SUBSCRIBER_RE.test(room)) return json({ error: "bad_subscriber" }, 400);
    const [known] = [...this.ctx.storage.sql.exec("SELECT 1 FROM subscribers WHERE room = ?", room)];
    if (!known) {
      const [count] = [...this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM subscribers")];
      if (Number(count?.n ?? 0) >= MAX_SUBSCRIBERS) return json({ error: "too_many_subscribers" }, 429);
      this.ctx.storage.sql.exec("INSERT INTO subscribers (room, pending) VALUES (?, 0)", room);
    }
    return json({ length: (await this.chainState()).length });
  }

  /** Tell subscribers the current length: all of them after an append, the
   * failed ones (`onlyPending`) from the alarm. Failures stay pending and
   * arm the alarm; the backoff resets once every push got through. */
  private async pushHead(onlyPending: boolean): Promise<void> {
    const registries = this.env.REGISTRY_ROOMS;
    if (!registries) return;
    let length: number;
    try {
      length = (await this.chainState()).length;
    } catch {
      return; // a corrupt log fails its reads loudly; nothing to announce
    }
    const rooms = [
      ...this.ctx.storage.sql.exec(
        onlyPending ? "SELECT room FROM subscribers WHERE pending = 1" : "SELECT room FROM subscribers"
      )
    ].map((row) => row.room as string);
    let failed = 0;
    await Promise.all(
      rooms.map(async (room) => {
        let ok = false;
        try {
          const response = await registries.get(registries.idFromName(room)).fetch("https://registry/trust-head", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ length })
          });
          ok = response.ok;
        } catch {
          ok = false;
        }
        if (!ok) failed += 1;
        this.ctx.storage.sql.exec(
          "UPDATE subscribers SET pending = ? WHERE room = ? AND pending != ?",
          ok ? 0 : 1,
          room,
          ok ? 0 : 1
        );
      })
    );
    const [pending] = [...this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM subscribers WHERE pending = 1")];
    if (Number(pending?.n ?? 0) === 0) {
      this.setRetry(RETRY_BASE_MS);
      return;
    }
    if (failed === 0) return; // another push's alarm is already armed
    const delay = onlyPending ? Math.min(this.retry() * 2, RETRY_CAP_MS) : this.retry();
    this.setRetry(delay);
    await this.ctx.storage.setAlarm(Date.now() + delay);
  }

  private retry(): number {
    const [row] = [...this.ctx.storage.sql.exec("SELECT value FROM kv WHERE key = 'retryMs'")];
    return row ? Number(row.value) : RETRY_BASE_MS;
  }

  private setRetry(ms: number): void {
    if (this.retry() === ms) return;
    this.ctx.storage.sql.exec(
      "INSERT INTO kv (key, value) VALUES ('retryMs', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
      String(ms)
    );
  }

  async alarm(): Promise<void> {
    await this.pushHead(true);
  }
}

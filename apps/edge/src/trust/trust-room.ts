/**
 * TrustRoom — one Durable Object per user (`t1/{userId}`): the user's device
 * trust chain and the short-lived pairing mailboxes (crates/trust).
 *
 *   GET  /chain                       → { entries }
 *   POST /chain { entry }             → append; 409 { length } if `entry.seq`
 *                                       is not the next one, 400 if invalid
 *   GET|PUT /pairing/:mailbox/:slot   → one pairing message (`join` | `welcome`)
 *   DELETE  /pairing/:mailbox
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

const readBody = async (request: Request): Promise<string | undefined> => {
  const text = await request.text();
  return new TextEncoder().encode(text).length > MAX_BODY ? undefined : text;
};

export class TrustRoom implements DurableObject {
  private readonly ctx: DurableObjectState;
  private state?: ChainState;

  constructor(ctx: DurableObjectState, env: Env) {
    this.ctx = ctx;
    void env;
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS entries (seq INTEGER PRIMARY KEY, body TEXT NOT NULL)");
    ctx.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS mailboxes (id TEXT NOT NULL, slot TEXT NOT NULL, body TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (id, slot))"
    );
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
    if (!request.headers.get(AUTH_USER_HEADER)) return json({ error: "unauthenticated" }, 401);
    const url = new URL(request.url);
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
        keysSeen: new Set(current.keysSeen)
      };
      const refused = await applyEntry(next, valid);
      if (refused) return json({ error: "invalid", reason: refused }, 400);
      this.ctx.storage.sql.exec("INSERT INTO entries (seq, body) VALUES (?, ?)", valid.seq, JSON.stringify(valid));
      this.state = next;
      return json({ length: next.length });
    });
  }
}

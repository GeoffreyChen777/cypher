/**
 * Cypher-native edge Worker (design §2, ARCHITECTURE §6): JWT auth at the
 * edge, then forwarding into per-chat, per-user registry, and per-device
 * Durable Objects. Also serves content-addressed R2 attachments (§1.2) and
 * the absorbed WorkOS auth routes (formerly apps/server).
 *
 * Routes:
 *   GET  /health
 *   POST /auth/exchange               — WorkOS code → tokens
 *   POST /auth/refresh                — WorkOS refresh → fresh tokens
 *   GET  /auth/orgs                   — caller's active org memberships
 *   POST /auth/orgs                   — create org + admin membership
 *   GET  /auth/cli/callback           — headless sign-in paste-code page
 *   GET  /auth/ios/callback           — iOS bridge → cypher://callback (query intact)
 *   GET  /registry/:orgId/ws          — workspace registry room `reg1/{orgId}/{user}` (wss)
 *   GET  /registry/:orgId/stats       — registry seq/rows/attribution
 *   GET  /registry/:orgId/rows        — registry delta/full HTTPS pull
 *   POST /registry/:orgId/push        — registry HTTPS push
 *   POST /registry/:orgId/reset       — registry operator wipe (self-healing)
 *   GET  /device/:deviceId/ws?role=   — device-room byte pipe (§8)
 *   GET  /device/:deviceId/sidecar/:name
 *   POST /device/:deviceId/sidecar/:name
 *   GET  /device/:deviceId/status
 *   PUT  /blob/:chatId/:partId        — tool-output sidecar (chat2-sync A2)
 *   GET  /blob/:chatId/:partId
 *   GET  /chat2/:chatId/ws            — chat2 log-relay room (wss, chat2-sync B)
 *   GET|POST /chat2/:chatId/checkpoint — client-built doc snapshot (Range-resumable GET)
 *   GET  /chat2/:chatId/rows           — HTTPS pull (framed state/rows)
 *   POST /chat2/:chatId/rows           — HTTPS push (batch-id deduped)
 *   GET|PUT  /chat2/:chatId/tail      — host-published sidecars, served verbatim
 *   GET|PUT  /chat2/:chatId/diff
 *   GET  /chat2/:chatId/stats
 *   POST /chat2/:chatId/reset
 */
import { authenticate } from "./auth";
import { handleAuthRoute } from "./auth-routes";
import { AUTH_USER_HEADER, type Env } from "./env";
import { SessionRoom } from "./session-room";
import { DeviceRoom } from "./device-room";
import { RegistryRoom } from "./registry-room";
import { ChatRoom } from "./chat-room";
import { PushDevice } from "./push-device";
export { APNsSender } from "./apns-sender";
import { object, readNotificationJSON } from "./notifications-model";
import installSh from "./install.sh";

export { SessionRoom, DeviceRoom, RegistryRoom, ChatRoom, PushDevice };

const ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

/** `decodeURIComponent` that answers `undefined` for malformed %-escapes. */
const safeDecode = (segment: string): string | undefined => {
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
};
/** Tool part ids are harness-minted (`tool-1`, `call_x`, `m1#c1`-style) —
 * wider than ID_RE but still no slashes, so a part id can't traverse keys. */
const PART_RE = /^[A-Za-z0-9._:#~-]{1,200}$/;
const MAX_TOOL_BLOB_BYTES = 1024 * 1024;

const json = (value: unknown, status = 200): Response =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" }
  });

/** Forward into a DO with the verified user stamped on the request. */
const forward = (
  ns: DurableObjectNamespace,
  name: string,
  request: Request,
  userId: string,
  path: string,
  search?: string
): Promise<Response> => {
  const stub = ns.get(ns.idFromName(name));
  const url = new URL(request.url);
  url.pathname = path;
  if (search !== undefined) url.search = search;
  const headers = new Headers(request.headers);
  headers.set(AUTH_USER_HEADER, userId);
  return stub.fetch(new Request(url.toString(), { ...requestInit(request), headers }));
};

const requestInit = (request: Request): RequestInit => ({
  method: request.method,
  body: request.body
});

/** Carry the dialing engine's `&device=` through to the DO (socket
 * attribution in logs — the 2026-08-04 deaf socket was only identifiable by
 * reverse-engineering rotating IPv6 privacy addresses). Validated so a
 * hand-crafted value can't inject into log lines or the DO's query. */
const deviceParam = (url: URL): string => {
  const device = url.searchParams.get("device") ?? "";
  return ID_RE.test(device) ? `&device=${device}` : "";
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);

    if (url.pathname === "/health") {
      return json({ ok: true, auth: env.AUTH_MODE === "dev" ? "dev" : "workos" });
    }

    // ── public install surface (served on the edge.letscypher.app custom
    //    domain and the workers.dev host cypher-edge.<account>.workers.dev):
    //    the `curl | sh` installer and release artifacts ────────────────────
    if (url.pathname === "/install.sh" && (request.method === "GET" || request.method === "HEAD")) {
      return new Response(request.method === "HEAD" ? null : installSh, {
        headers: {
          "content-type": "application/x-sh",
          "cache-control": "public, max-age=0, must-revalidate"
        }
      });
    }
    if (
      parts[0] === "releases" &&
      parts.length >= 2 &&
      (request.method === "GET" || request.method === "HEAD")
    ) {
      const key = decodeURIComponent(url.pathname.slice("/releases/".length));
      if (key.length === 0 || key.includes("..")) return json({ error: "bad request" }, 400);
      const object = await env.RELEASES.get(key);
      if (!object) return json({ error: "not_found" }, 404);
      // latest.txt / manifest.json flip on release; artifacts are immutable by name.
      const mutable = key.endsWith(".txt") || key.endsWith(".json");
      const headers = new Headers({
        "content-type": key.endsWith(".txt")
          ? "text/plain; charset=utf-8"
          : key.endsWith(".json")
            ? "application/json"
            : "application/octet-stream",
        "content-length": String(object.size),
        "cache-control": mutable ? "public, max-age=60" : "public, max-age=86400, immutable",
        etag: object.httpEtag,
        // The landing page fetches release metadata cross-origin.
        "access-control-allow-origin": "*"
      });
      return new Response(request.method === "HEAD" ? null : object.body, { headers });
    }

    // ── WorkOS auth routes (pre-bearer: exchange/refresh/callback have no
    //    access token yet; the org routes verify the bearer themselves) ─────
    const authRouted = await handleAuthRoute(request, env, url);
    if (authRouted) return authRouted;

    // A revocation-only capability survives account logout/expired auth.
    // The opaque binding id + random lease can only disable that registration;
    // it cannot read data, register a token or send a notification.
    if (url.pathname === "/notifications/revoke" && request.method === "POST") {
      if (!env.PUSH_DEVICES) return json({ error: "unavailable" }, 503);
      try {
        const body = object(await readNotificationJSON(request));
        if (!/^[a-f0-9]{64}$/.test(String(body.bindingId)) ||
            !/^[a-f0-9]{64}$/.test(String(body.scope)) ||
            !/^[a-f0-9-]{36}$/.test(String(body.lease)) ||
            typeof body.epoch !== "number" || !Number.isSafeInteger(body.epoch) || body.epoch < 1) {
          return json({ error: "invalid" }, 400);
        }
        return env.PUSH_DEVICES.get(env.PUSH_DEVICES.idFromString(String(body.bindingId)))
          .fetch(new Request("https://push/unregister", { method: "POST", body: JSON.stringify({
            scope: body.scope, lease: body.lease, epoch: body.epoch
          }) }));
      } catch { return json({ error: "invalid" }, 400); }
    }

    const auth = await authenticate(env, request);
    if (!auth) return json({ error: "unauthenticated" }, 401);

    // ── chat2 rooms (docs/chat2-sync.md B): dumb log relays, one per chat.
    //    Claim-on-first-join ownership enforced in the DO (chat ids are
    //    client-minted). The DO handles /ws, /checkpoint (GET Range-resumable
    //    + POST floor-guarded), host-published /tail + /diff sidecars,
    //    /stats, /reset. ──────────────────────────────────────────────────────
    if (parts[0] === "chat2" && parts[1] && ID_RE.test(parts[1]) && parts[2]) {
      const room = `chat2/${parts[1]}`;
      if (parts[2] === "ws" && parts.length === 3) {
        if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
          return json({ error: "expected websocket" }, 426);
        }
        return forward(
          env.CHAT_ROOMS,
          room,
          request,
          auth.userId,
          "/ws",
          `?chatId=${parts[1]}${deviceParam(url)}`
        );
      }
      const routes: Record<string, string[]> = {
        checkpoint: ["GET", "POST"],
        rows: ["GET", "POST"],
        tail: ["GET", "PUT"],
        diff: ["GET", "PUT"],
        stats: ["GET"],
        reset: ["POST"]
      };
      if (parts.length === 3 && routes[parts[2]]?.includes(request.method)) {
        // Query carries through (`seqCovered` on POST /checkpoint), as do
        // headers (`x-chat2-frontier`, `range`).
        return forward(env.CHAT_ROOMS, room, request, auth.userId, `/${parts[2]}`, url.search);
      }
      return json({ error: "not found" }, 404);
    }

    // ── registry rooms (docs/registry-sync.md): the row-table replacement for
    //    the Loro workspace doc. The caller's WorkOS org claim (`org_id`)
    //    must match the URL, room derived from the caller's OWN user id, DO
    //    trusts the stamped header. `reg1` = first registry generation. ─────
    if (parts[0] === "registry" && parts[1] && ID_RE.test(parts[1])) {
      const orgId = parts[1];
      if (auth.orgId !== orgId) return json({ error: "forbidden" }, 403);
      const room = `reg1/${orgId}/${auth.userId}`;
      if (parts.length === 4 && parts[2] === "notifications" &&
          ["settings", "activity", "event", "register", "unregister"].includes(parts[3] ?? "")) {
        return forward(env.REGISTRY_ROOMS, room, request, auth.userId, `/notifications/${parts[3]}`, "");
      }
      if (parts[2] === "ws") {
        if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
          return json({ error: "expected websocket" }, 426);
        }
        return forward(
          env.REGISTRY_ROOMS,
          room,
          request,
          auth.userId,
          "/ws",
          `?${deviceParam(url).replace(/^&/, "")}`
        );
      }
      if (parts[2] === "stats" && request.method === "GET") {
        return forward(env.REGISTRY_ROOMS, room, request, auth.userId, "/stats", "");
      }
      // Repair/inspection read: the full current row table.
      if (parts[2] === "rows" && request.method === "GET") {
        return forward(env.REGISTRY_ROOMS, room, request, auth.userId, "/rows", url.search);
      }
      if (parts[2] === "push" && request.method === "POST") {
        return forward(env.REGISTRY_ROOMS, room, request, auth.userId, "/push", url.search);
      }
      // Operator wipe, no recipe needed: clients
      // detect the seq regression on their next hello and re-seed the table
      // from local rows with original clocks, automatically.
      if (parts[2] === "reset" && request.method === "POST") {
        return forward(env.REGISTRY_ROOMS, room, request, auth.userId, "/reset", "");
      }
    }

    // ── device rooms ────────────────────────────────────────────────────────
    if (parts[0] === "device" && parts[1] && ID_RE.test(parts[1])) {
      const deviceId = parts[1];
      if (parts[2] === "ws") {
        if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
          return json({ error: "expected websocket" }, 426);
        }
        const role = url.searchParams.get("role") === "host" ? "host" : "client";
        const connId = url.searchParams.get("connId") ?? crypto.randomUUID();
        // `d2/` = the WorkOS staging→production identity break: a fresh
        // namespace let production user ids claim fresh rooms.
        return forward(
          env.DEVICE_ROOMS,
          `d2/${deviceId}`,
          request,
          auth.userId,
          "/ws",
          `?role=${role}&connId=${encodeURIComponent(connId)}`
        );
      }
      if (parts[2] === "sidecar" && parts[3] && /^[a-z0-9-]{1,64}$/.test(parts[3])) {
        return forward(env.DEVICE_ROOMS, `d2/${deviceId}`, request, auth.userId, `/sidecar/${parts[3]}`, "");
      }
      if (parts[2] === "status") {
        return forward(env.DEVICE_ROOMS, `d2/${deviceId}`, request, auth.userId, "/status", "");
      }
      // Durable command nudge (§7): "chat X has pending commands — open its
      // doc". Delivered live if the host is connected, else queued in the DO
      // and replayed on the host's next join.
      if (parts[2] === "nudge" && request.method === "POST") {
        return forward(env.DEVICE_ROOMS, `d2/${deviceId}`, request, auth.userId, "/nudge", "");
      }
    }

    // ── R2 tool-output sidecar (docs/chat2-sync.md A2): full tool outputs
    //    and diffs live here, keyed `{chatId}/{partId}[.diff]`; the doc keeps
    //    only a one-line summary + this key. Straight R2, no DO involvement —
    //    the doc stays thin whether or not these uploads land. Per-user
    //    prefix = owner auth. ─────────────────────────────────────────────
    if (parts[0] === "blob" && parts.length === 3 && ID_RE.test(parts[1])) {
      // Percent-decode the part segment before validating: PART_RE allows
      // `#` (`m1#c1`-style harness ids), which HTTP clients cannot send raw
      // (fragment delimiter) — the host percent-encodes it. Decode-then-
      // validate keeps traversal shut: `%2F` decodes to `/`, fails PART_RE.
      const partId = safeDecode(parts[2]);
      if (partId === undefined || !PART_RE.test(partId)) {
        return json({ error: "bad part id" }, 400);
      }
      const key = `blob/${auth.userId}/${parts[1]}/${partId}`;
      if (request.method === "PUT") {
        const body = await request.arrayBuffer();
        // Outputs are 4KiB-capped at the harness boundary; diffs can run
        // larger but a sidecar entry is one tool result, never a dump.
        if (body.byteLength > MAX_TOOL_BLOB_BYTES) return json({ error: "too_large" }, 413);
        await env.BLOBS.put(key, body, {
          httpMetadata: {
            contentType: request.headers.get("content-type") ?? "text/plain; charset=utf-8"
          }
        });
        return json({ ok: true, bytes: body.byteLength });
      }
      if (request.method === "GET" || request.method === "HEAD") {
        const object =
          request.method === "GET" ? await env.BLOBS.get(key) : await env.BLOBS.head(key);
        if (!object) return json({ error: "not_found" }, 404);
        const headers = new Headers();
        object.writeHttpMetadata(headers);
        headers.set("etag", object.httpEtag);
        // Re-resolved tool parts overwrite their key, so short-lived caching only.
        headers.set("cache-control", "private, max-age=300");
        const body =
          request.method === "GET" && "body" in object ? (object as R2ObjectBody).body : null;
        return new Response(body, { headers });
      }
    }

    // ── retired attachment mirror (clients ≤0.1.62): acknowledge and discard
    //    so old outboxes drain once instead of retrying the PUT forever ──────
    if (parts[0] === "attachments" && parts[1] && request.method === "PUT") {
      return json({ ok: true });
    }

    return json({ error: "not_found" }, 404);
  }
} satisfies ExportedHandler<Env>;

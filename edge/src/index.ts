/** Native v3 entry point. Verified account/org choose all data namespaces;
 * captured identity assertions are checked before any conversation access.
 * Retired formats have no route, reset, reseed or discard-and-ACK fallback. */
import { authenticate } from "./auth";
import { handleAuthRoute } from "./auth-routes";
import { AUTH_USER_HEADER, AUTH_ORG_HEADER, AUTH_DEADLINE_HEADER, EXPECTED_USER_HEADER, ROOM_KIND_HEADER, type Env } from "./env";
import { Sync3Room } from "./sync3-room";
import { WorkspaceHub } from "./workspace3-hub";
import { PushDevice } from "./push-device";
// Retained as non-routed Durable Object class exports while existing
// production objects drain. They are not bound by the v3 Worker and receive
// no new traffic; removing an exported class before a delete-class migration
// makes Cloudflare reject the deployment.
import { SessionRoom } from "./session-room";
import { DeviceRoom } from "./device-room";
import { RegistryRoom } from "./registry-room";
import { ChatRoom } from "./chat-room";
import { object, readNotificationJSON } from "./notifications-model";
import installSh from "./install.sh";

export { APNsSender } from "./apns-sender";
export { PushDevice, Sync3Room, WorkspaceHub, SessionRoom, DeviceRoom, RegistryRoom, ChatRoom };

const ID_RE = /^[A-Za-z0-9_-]{1,128}$/;
const json = (value: unknown, status = 200) => Response.json(value, { status });

function forward(ns: DurableObjectNamespace, name: string, request: Request,
                 user: string, org: string, path: string, deadline?: number): Promise<Response> {
  const url = new URL(request.url);
  url.pathname = path; url.search = "";
  const headers = new Headers(request.headers);
  headers.delete(ROOM_KIND_HEADER);
  headers.delete(AUTH_DEADLINE_HEADER);
  headers.set(AUTH_USER_HEADER, user);
  headers.set(AUTH_ORG_HEADER, org);
  if (deadline !== undefined) headers.set(AUTH_DEADLINE_HEADER, String(deadline));
  return ns.get(ns.idFromName(name)).fetch(new Request(url.toString(), {
    method: request.method, body: request.body, headers
  }));
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url), parts = url.pathname.split("/").filter(Boolean);
    if (url.pathname === "/health") return json({ ok: true, auth: env.AUTH_MODE === "dev" ? "dev" : "workos" });
    if (url.pathname === "/install.sh" && ["GET", "HEAD"].includes(request.method)) {
      return new Response(request.method === "HEAD" ? null : installSh, {
        headers: { "content-type": "application/x-sh", "cache-control": "public, max-age=0, must-revalidate" }
      });
    }
    if (parts[0] === "releases" && parts.length >= 2 && ["GET", "HEAD"].includes(request.method)) {
      let key: string;
      try { key = decodeURIComponent(url.pathname.slice("/releases/".length)); }
      catch { return json({ error: "bad request" }, 400); }
      if (!key || key.includes("..")) return json({ error: "bad request" }, 400);
      const blob = await env.RELEASES.get(key);
      if (!blob) return json({ error: "not_found" }, 404);
      const mutable = key.endsWith(".txt") || key.endsWith(".json");
      return new Response(request.method === "HEAD" ? null : blob.body, { headers: {
        "content-type": key.endsWith(".txt") ? "text/plain; charset=utf-8" : key.endsWith(".json") ? "application/json" : "application/octet-stream",
        "content-length": String(blob.size),
        "cache-control": mutable ? "public, max-age=60" : "public, max-age=86400, immutable",
        etag: blob.httpEtag, "access-control-allow-origin": "*"
      } });
    }
    const routed = await handleAuthRoute(request, env, url);
    if (routed) return routed;

    // This random revocation capability cannot register/read/send anything.
    if (url.pathname === "/notifications/revoke" && request.method === "POST") {
      if (!env.PUSH_DEVICES) return json({ error: "unavailable" }, 503);
      try {
        const body = object(await readNotificationJSON(request));
        if (!/^[a-f0-9]{64}$/.test(String(body.bindingId)) || !/^[a-f0-9]{64}$/.test(String(body.scope)) ||
            !/^[a-f0-9-]{36}$/.test(String(body.lease)) || typeof body.epoch !== "number" ||
            !Number.isSafeInteger(body.epoch) || body.epoch < 1) return json({ error: "invalid" }, 400);
        return env.PUSH_DEVICES.get(env.PUSH_DEVICES.idFromString(String(body.bindingId)))
          .fetch(new Request("https://push/unregister", { method: "POST", body: JSON.stringify({
            scope: body.scope, lease: body.lease, epoch: body.epoch
          }) }));
      } catch { return json({ error: "invalid" }, 400); }
    }

    const auth = await authenticate(env, request);
    if (!auth) return json({ error: "unauthenticated" }, 401);
    if (["sync3", "workspace3"].includes(parts[0]) && url.search) return json({ error: "query_not_supported" }, 400);
    if (parts[0] === "workspace3") {
      if (!env.WORKSPACE3) return json({ error: "not_enabled" }, 503);
      if (!ID_RE.test(parts[1] ?? "")) return json({ error: "not_found" }, 404);
      if (auth.orgId !== parts[1]) return json({ error: "forbidden" }, 403);
      const room = JSON.stringify(["workspace3", auth.orgId, auth.userId]);
      if (parts.length === 4 && parts[2] === "notifications" &&
          ["settings", "activity", "event", "register", "unregister"].includes(parts[3])) {
        if (request.headers.get(EXPECTED_USER_HEADER) !== auth.userId) return json({ error: "account_mismatch" }, 403);
        return forward(env.WORKSPACE3, room, request, auth.userId, auth.orgId, `/notifications/${parts[3]}`);
      }
      if (parts.length !== 3 || parts[2] !== "ws") return json({ error: "not_found" }, 404);
      const deadline = Math.min(Date.now() + 300_000, auth.expiresAtMs ?? (env.AUTH_MODE === "dev" ? Infinity : 0));
      if (!Number.isFinite(deadline) || deadline <= Date.now()) return json({ error: "reauth_required" }, 401);
      return forward(env.WORKSPACE3, room, request, auth.userId, auth.orgId, "/ws", deadline);
    }
    if (parts[0] === "sync3") {
      if (env.SYNC3_ENABLED !== "true" || !env.SYNC3_ROOMS) return json({ error: "not_enabled" }, 404);
      if (parts.length !== 5 || parts[2] !== "chats" || !ID_RE.test(parts[1]) || !ID_RE.test(parts[3]) ||
          !["init", "exchange", "ws"].includes(parts[4])) return json({ error: "not_found" }, 404);
      if (auth.orgId !== parts[1]) return json({ error: "forbidden" }, 403);
      if (request.headers.get(EXPECTED_USER_HEADER) !== auth.userId) return json({ error: "account_mismatch" }, 403);
      const deadline = Math.min(Date.now() + 300_000, auth.expiresAtMs ?? (env.AUTH_MODE === "dev" ? Infinity : 0));
      if (!Number.isFinite(deadline) || deadline <= Date.now()) return json({ error: "reauth_required" }, 401);
      return forward(env.SYNC3_ROOMS, JSON.stringify(["sync3", auth.orgId, auth.userId, parts[3]]),
        request, auth.userId, auth.orgId, `/${parts[4]}`, deadline);
    }
    if (["session", "tail", "stats", "diff", "snapshot", "append", "chat2", "blob",
         "registry", "device", "workspace", "attachments"].includes(parts[0])) {
      return json({ error: "v3_required" }, 410);
    }
    return json({ error: "not_found" }, 404);
  }
} satisfies ExportedHandler<Env>;

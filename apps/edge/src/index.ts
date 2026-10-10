/**
 * Cypher-native edge Worker (design §2, ARCHITECTURE §6): JWT auth at the
 * edge, then forwarding into per-chat, per-user registry, and per-device
 * Durable Objects.
 *
 * The fetch handler is an ordered chain; each handler answers
 * `Response | undefined` and `undefined` passes the request on:
 *
 *   public-routes.ts             /health, /install.sh, /releases/*
 *   auth-routes.ts               /auth/* (pre-bearer: exchange, refresh, callbacks, orgs)
 *   revoke-route.ts              POST /notifications/revoke (revocation-only capability)
 *   ── bearer verified below; 401 otherwise ──
 *   room-routes.ts               /chat2/*, /registry/*, /device/* → Durable Objects
 *   blob-route.ts                GET|HEAD /blob/:chatId/:partId (read-only R2)
 *   legacy/attachments-route.ts  PUT /attachments/* (retired, acknowledged)
 *
 * Durable Object classes and the APNs entrypoint are exported here only;
 * wrangler.jsonc binds them by these names.
 */
import { authenticate } from "./auth/auth";
import { handleAuthRoute } from "./auth/auth-routes";
import { handleBlobRoute } from "./blob-route";
import { json, type Env } from "./env";
import { handleRetiredAttachments } from "./legacy/attachments-route";
import { handlePublicRoute } from "./public-routes";
import { handleRevokeRoute } from "./revoke-route";
import { handleRoomRoute } from "./room-routes";
import { routeContext } from "./router";

export { APNsSender } from "./notifications/apns-sender";
export { ChatRoom } from "./chat/chat-room";
export { DeviceRoom } from "./device/device-room";
export { PushDevice } from "./notifications/push-device";
export { RegistryRoom } from "./registry/registry-room";
export { SessionRoom } from "./legacy/session-room";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const context = routeContext(request, env);
    const open =
      (await handlePublicRoute(context)) ??
      (await handleAuthRoute(request, env, context.url)) ??
      (await handleRevokeRoute(context));
    if (open) return open;

    const auth = await authenticate(env, request);
    if (!auth) return json({ error: "unauthenticated" }, 401);
    const authed = { ...context, auth };
    return (
      (await handleRoomRoute(authed)) ??
      (await handleBlobRoute(authed)) ??
      handleRetiredAttachments(authed) ??
      json({ error: "not_found" }, 404)
    );
  }
} satisfies ExportedHandler<Env>;

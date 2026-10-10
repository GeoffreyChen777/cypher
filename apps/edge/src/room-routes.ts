/**
 * Authenticated Durable Object routes, one table entry per room kind:
 *
 *   /chat2/:chatId/…      → CHAT_ROOMS     `chat2/{chatId}`
 *   /registry/:orgId/…    → REGISTRY_ROOMS `reg1/{orgId}/{userId}`
 *   /device/:deviceId/…   → DEVICE_ROOMS   `d2/{deviceId}`
 *
 * A sub-route matches the segments after the room id; the forwarded path is
 * those matched segments, and the forwarded query is empty unless the entry
 * says otherwise. The DO trusts the stamped user header (router.ts forward).
 */
import type { Verified } from "./auth/auth";
import { json } from "./env";
import { ID_RE, SIDECAR_NAME_RE } from "./identifiers";
import { deviceParam, forward, requireWebSocket, type AuthedContext } from "./router";

interface SubRoute {
  /** Segments after the room id: a literal, or a pattern for a parameter. */
  path: readonly (string | RegExp)[];
  /** Allowed methods; any method when omitted. */
  methods?: readonly string[];
  /** Match only when nothing follows `path`; trailing segments are ignored otherwise. */
  exact?: boolean;
  /** Answer 426 unless the request is a WebSocket upgrade. */
  websocket?: boolean;
  /** Query forwarded to the DO: the caller's own (`"keep"`) or a computed one. */
  search?: "keep" | ((url: URL, id: string) => string);
}

interface RoomRoute {
  prefix: string;
  binding: "CHAT_ROOMS" | "REGISTRY_ROOMS" | "DEVICE_ROOMS";
  room: (id: string, auth: Verified) => string;
  /** Checked before any sub-route; a response ends the request. */
  guard?: (id: string, auth: Verified) => Response | undefined;
  /** Own every sub-path: unmatched ones answer 404 here instead of falling
   * through, and a bare `/:prefix/:id` falls through untouched. */
  closed?: boolean;
  subs: readonly SubRoute[];
}

const ROOM_ROUTES: readonly RoomRoute[] = [
  {
    // docs/design/chat2-sync.md B: dumb log relays, one per chat. Chat ids are
    // client-minted, so ownership is claimed on first join inside the DO.
    prefix: "chat2",
    binding: "CHAT_ROOMS",
    room: (chatId) => `chat2/${chatId}`,
    closed: true,
    subs: [
      { path: ["ws"], exact: true, websocket: true, search: (url, chatId) => `?chatId=${chatId}${deviceParam(url)}` },
      // Query carries through (`seqCovered` on POST /checkpoint), as do
      // headers (`x-chat2-frontier`, `range`).
      { path: ["checkpoint"], methods: ["GET", "POST"], exact: true, search: "keep" },
      { path: ["rows"], methods: ["GET", "POST"], exact: true, search: "keep" },
      { path: ["tail"], methods: ["GET", "PUT"], exact: true, search: "keep" },
      { path: ["diff"], methods: ["GET", "PUT"], exact: true, search: "keep" },
      { path: ["stats"], methods: ["GET"], exact: true, search: "keep" },
      { path: ["reset"], methods: ["POST"], exact: true, search: "keep" }
    ]
  },
  {
    // docs/design/registry-sync.md: the caller's WorkOS org claim must match
    // the URL, and the room is derived from the caller's own user id.
    // `reg1` = first registry generation.
    prefix: "registry",
    binding: "REGISTRY_ROOMS",
    room: (orgId, auth) => `reg1/${orgId}/${auth.userId}`,
    guard: (orgId, auth) => (auth.orgId === orgId ? undefined : json({ error: "forbidden" }, 403)),
    subs: [
      { path: ["notifications", /^(?:settings|activity|event|register|unregister)$/], exact: true },
      { path: ["ws"], websocket: true, search: (url) => `?${deviceParam(url).replace(/^&/, "")}` },
      { path: ["stats"], methods: ["GET"] },
      // Repair/inspection read: the full current row table.
      { path: ["rows"], methods: ["GET"], search: "keep" },
      { path: ["push"], methods: ["POST"], search: "keep" },
      // Operator wipe: clients detect the seq regression on their next hello
      // and re-seed the table from local rows with original clocks.
      { path: ["reset"], methods: ["POST"] }
    ]
  },
  {
    // `d2/` = the WorkOS staging→production identity break: a fresh namespace
    // let production user ids claim fresh rooms.
    prefix: "device",
    binding: "DEVICE_ROOMS",
    room: (deviceId) => `d2/${deviceId}`,
    subs: [
      {
        path: ["ws"],
        websocket: true,
        search: (url) => {
          const role = url.searchParams.get("role") === "host" ? "host" : "client";
          const connId = url.searchParams.get("connId") ?? crypto.randomUUID();
          return `?role=${role}&connId=${encodeURIComponent(connId)}`;
        }
      },
      { path: ["sidecar", SIDECAR_NAME_RE] },
      { path: ["status"] },
      // Durable command nudge: delivered live if the host is connected, else
      // queued in the DO and replayed on the host's next join.
      { path: ["nudge"], methods: ["POST"] }
    ]
  }
];

const matches = (sub: SubRoute, rest: readonly string[], method: string): boolean => {
  if (sub.methods && !sub.methods.includes(method)) return false;
  if (sub.exact ? rest.length !== sub.path.length : rest.length < sub.path.length) return false;
  return sub.path.every((segment, i) => {
    const part = rest[i];
    if (part === undefined) return false;
    return typeof segment === "string" ? part === segment : segment.test(part);
  });
};

export const handleRoomRoute = ({ request, env, url, parts, auth }: AuthedContext): Response | Promise<Response> | undefined => {
  const [prefix, id, ...rest] = parts;
  const route = ROOM_ROUTES.find((candidate) => candidate.prefix === prefix);
  if (!route || id === undefined || !ID_RE.test(id)) return undefined;
  if (route.closed && rest.length === 0) return undefined;
  const denied = route.guard?.(id, auth);
  if (denied) return denied;
  const sub = route.subs.find((candidate) => matches(candidate, rest, request.method));
  if (!sub) return route.closed ? json({ error: "not found" }, 404) : undefined;
  if (sub.websocket) {
    const refused = requireWebSocket(request);
    if (refused) return refused;
  }
  const path = `/${rest.slice(0, sub.path.length).join("/")}`;
  const search = sub.search === "keep" ? url.search : (sub.search?.(url, id) ?? "");
  return forward(env[route.binding], route.room(id, auth), request, auth.userId, path, search);
};


import { describe, expect, it, vi } from "vitest";
vi.mock("./notifications/apns-sender", () => ({ APNsSender: class {} }));
vi.mock("./install.sh", () => ({ default: "" }));
import worker from "./index";
import type { Env } from "./env";
import { AUTH_USER_HEADER } from "./env";

describe("notification route boundaries", () => {
  it("authenticates and isolates settings/activity/register by both organization and user", async () => {
    const names: string[] = [], requests: Request[] = [];
    const env = { AUTH_MODE: "dev", REGISTRY_ROOMS: {
      idFromName(name: string) { names.push(name); return name; },
      get() { return { fetch(request: Request) { requests.push(request); return Response.json({ ok: true }); } }; }
    } } as unknown as Env;
    const call = (token?: string, org = "org") => worker.fetch(new Request(
      `https://edge.test/registry/${org}/notifications/settings`, { headers: {
        ...(token ? { Authorization: `Bearer ${token}` } : {}), [AUTH_USER_HEADER]: "victim"
      } }), env);
    expect((await call()).status).toBe(401);
    expect((await call("alice@other")).status).toBe(403);
    expect(names).toHaveLength(0);
    expect((await call("alice@org")).status).toBe(200);
    expect((await call("bob@org")).status).toBe(200);
    expect(names).toEqual(["reg1/org/alice", "reg1/org/bob"]);
    expect(requests.map(r => r.headers.get(AUTH_USER_HEADER))).toEqual(["alice", "bob"]);
    expect(new URL(requests[0].url).pathname).toBe("/notifications/settings");
  });
  it("the unauthenticated capability can only request revocation, never registration or sending", async () => {
    const requests: Request[] = [];
    const env = { PUSH_DEVICES: {
      idFromString: (id: string) => id,
      get: () => ({ fetch(request: Request) { requests.push(request); return Response.json({ permanent: true }); } })
    } } as unknown as Env;
    const post = (body: object) => worker.fetch(new Request("https://edge.test/notifications/revoke", {
      method: "POST", body: JSON.stringify(body)
    }), env);
    expect((await post({ bindingId: "a".repeat(64) })).status).toBe(400);
    expect(requests).toHaveLength(0);
    expect((await post({ bindingId: "a".repeat(64), scope: "b".repeat(64),
      lease: crypto.randomUUID(), epoch: 2, path: "/send", token: "forged" })).status).toBe(200);
    expect(new URL(requests[0].url).pathname).toBe("/unregister");
    const forwarded = await requests[0].json() as object;
    expect(Object.keys(forwarded).sort()).toEqual(["epoch", "lease", "scope"]);
  });
});

interface Forwarded {
  ns: string;
  room: string;
  method: string;
  path: string;
  search: string;
  user: string | null;
}

/** An env whose DO namespaces record every forward and whose buckets hold
 * one object each (`blob/alice/c1/m1#c1`, `stable/latest.txt`). */
const routingEnv = () => {
  const forwarded: Forwarded[] = [];
  const blobReads: string[] = [];
  const namespace = (ns: string) => ({
    idFromName: (name: string) => name,
    get: (room: string) => ({
      fetch(request: Request) {
        const url = new URL(request.url);
        forwarded.push({
          ns, room, method: request.method, path: url.pathname, search: url.search,
          user: request.headers.get(AUTH_USER_HEADER)
        });
        return Response.json({ ok: true });
      }
    })
  });
  const r2Object = (key: string) => ({
    key, size: 2, httpEtag: `"${key}"`, body: "ok",
    writeHttpMetadata(headers: Headers) { headers.set("content-type", "text/plain"); }
  });
  const env = {
    AUTH_MODE: "dev",
    CHAT_ROOMS: namespace("CHAT_ROOMS"),
    REGISTRY_ROOMS: namespace("REGISTRY_ROOMS"),
    DEVICE_ROOMS: namespace("DEVICE_ROOMS"),
    BLOBS: {
      get: async (key: string) => { blobReads.push(`get ${key}`); return key === "blob/alice/c1/m1#c1" ? r2Object(key) : null; },
      head: async (key: string) => { blobReads.push(`head ${key}`); return key === "blob/alice/c1/m1#c1" ? r2Object(key) : null; }
    },
    RELEASES: { get: async (key: string) => (key === "stable/latest.txt" ? r2Object(key) : null) }
  } as unknown as Env;
  return { env, forwarded, blobReads };
};

type Route = [method: string, path: string, token: string | undefined, upgrade: boolean,
  status: number, forward?: { ns: string; room: string; path: string; search: string | RegExp }];

// Golden table for the whole fetch chain: what each request answers and, when
// it reaches a Durable Object, which namespace, room, path and query it gets.
const ROUTES: Route[] = [
  ["GET", "/health", undefined, false, 200],
  ["GET", "/install.sh", undefined, false, 200],
  ["HEAD", "/install.sh", undefined, false, 200],
  ["POST", "/install.sh", undefined, false, 401],
  ["GET", "/releases/stable/latest.txt", undefined, false, 200],
  ["HEAD", "/releases/stable/latest.txt", undefined, false, 200],
  ["GET", "/releases/stable/missing.txt", undefined, false, 404],
  ["GET", "/releases/a..b", undefined, false, 400],
  ["POST", "/releases/stable/latest.txt", undefined, false, 401],
  ["POST", "/auth/exchange", undefined, false, 501],
  ["GET", "/nope", undefined, false, 401],
  ["GET", "/nope", "alice", false, 404],
  ["GET", "/chat2/c1/stats", undefined, false, 401],

  ["GET", "/chat2/c1/ws?device=dev-1", "alice", false, 426],
  ["GET", "/chat2/c1/ws?device=dev-1", "alice", true, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/ws", search: "?chatId=c1&device=dev-1" }],
  ["GET", "/chat2/c1/ws?device=bad!", "alice", true, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/ws", search: "?chatId=c1" }],
  ["GET", "/chat2/c1/ws/extra", "alice", true, 404],
  ["GET", "/chat2/c1/checkpoint?x=1", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/checkpoint", search: "?x=1" }],
  ["POST", "/chat2/c1/checkpoint?seqCovered=4", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/checkpoint", search: "?seqCovered=4" }],
  ["GET", "/chat2/c1/rows?after=2", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/rows", search: "?after=2" }],
  ["POST", "/chat2/c1/rows", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/rows", search: "" }],
  ["GET", "/chat2/c1/tail", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/tail", search: "" }],
  ["PUT", "/chat2/c1/tail", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/tail", search: "" }],
  ["GET", "/chat2/c1/diff", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/diff", search: "" }],
  ["PUT", "/chat2/c1/diff", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/diff", search: "" }],
  ["GET", "/chat2/c1/stats", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/stats", search: "" }],
  ["POST", "/chat2/c1/reset", "alice", false, 200,
    { ns: "CHAT_ROOMS", room: "chat2/c1", path: "/reset", search: "" }],
  ["GET", "/chat2/c1/reset", "alice", false, 404],
  ["POST", "/chat2/c1/stats", "alice", false, 404],
  ["GET", "/chat2/c1/unknown", "alice", false, 404],
  ["GET", "/chat2/c1/stats/extra", "alice", false, 404],
  ["GET", "/chat2/c1", "alice", false, 404],
  ["GET", "/chat2/bad!id/stats", "alice", false, 404],

  ["GET", "/registry/org/stats", "alice", false, 403],
  ["GET", "/registry/other/stats", "alice@org", false, 403],
  ["GET", "/registry/org/stats?x=1", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/stats", search: "" }],
  ["GET", "/registry/org/stats/extra", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/stats", search: "" }],
  ["POST", "/registry/org/stats", "alice@org", false, 404],
  ["GET", "/registry/org/ws", "alice@org", false, 426],
  ["GET", "/registry/org/ws?device=dev-1", "alice@org", true, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/ws", search: "?device=dev-1" }],
  ["GET", "/registry/org/ws?device=bad!", "alice@org", true, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/ws", search: "" }],
  ["GET", "/registry/org/ws/extra", "alice@org", true, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/ws", search: "" }],
  ["GET", "/registry/org/rows?after=3", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/rows", search: "?after=3" }],
  ["POST", "/registry/org/rows", "alice@org", false, 404],
  ["POST", "/registry/org/push?batch=1", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/push", search: "?batch=1" }],
  ["GET", "/registry/org/push", "alice@org", false, 404],
  ["POST", "/registry/org/reset?x=1", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/reset", search: "" }],
  ["GET", "/registry/org/reset", "alice@org", false, 404],
  ["GET", "/registry/org/notifications/settings?x=1", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/settings", search: "" }],
  ["PUT", "/registry/org/notifications/settings", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/settings", search: "" }],
  ["GET", "/registry/org/notifications/activity", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/activity", search: "" }],
  ["POST", "/registry/org/notifications/event", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/event", search: "" }],
  ["POST", "/registry/org/notifications/register", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/register", search: "" }],
  ["POST", "/registry/org/notifications/unregister", "alice@org", false, 200,
    { ns: "REGISTRY_ROOMS", room: "reg1/org/alice", path: "/notifications/unregister", search: "" }],
  ["GET", "/registry/org/notifications/bogus", "alice@org", false, 404],
  ["GET", "/registry/org/notifications/settings/extra", "alice@org", false, 404],
  ["GET", "/registry/org", "alice@org", false, 404],
  ["GET", "/registry/bad!/stats", "alice@bad!", false, 404],

  ["GET", "/device/dev1/ws?role=host&connId=c1", "alice", false, 426],
  ["GET", "/device/dev1/ws?role=host&connId=c%201", "alice", true, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/ws", search: "?role=host&connId=c%201" }],
  ["GET", "/device/dev1/ws?role=admin&connId=c1", "alice", true, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/ws", search: "?role=client&connId=c1" }],
  ["GET", "/device/dev1/ws", "alice", true, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/ws", search: /^\?role=client&connId=[0-9a-f-]{36}$/ }],
  ["GET", "/device/dev1/ws/extra?connId=c1", "alice", true, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/ws", search: "?role=client&connId=c1" }],
  ["GET", "/device/dev1/sidecar/repos?x=1", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/sidecar/repos", search: "" }],
  ["POST", "/device/dev1/sidecar/repos", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/sidecar/repos", search: "" }],
  ["PUT", "/device/dev1/sidecar/repos/extra", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/sidecar/repos", search: "" }],
  ["GET", "/device/dev1/sidecar/Repos", "alice", false, 404],
  ["GET", "/device/dev1/sidecar", "alice", false, 404],
  ["GET", "/device/dev1/status", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/status", search: "" }],
  ["DELETE", "/device/dev1/status/extra", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/status", search: "" }],
  ["POST", "/device/dev1/nudge", "alice", false, 200,
    { ns: "DEVICE_ROOMS", room: "d2/dev1", path: "/nudge", search: "" }],
  ["GET", "/device/dev1/nudge", "alice", false, 404],
  ["GET", "/device/dev1", "alice", false, 404],
  ["GET", "/device/bad!/status", "alice", false, 404],

  ["GET", "/blob/c1/m1%23c1", "alice", false, 200],
  ["HEAD", "/blob/c1/m1%23c1", "alice", false, 200],
  ["GET", "/blob/c1/missing", "alice", false, 404],
  ["GET", "/blob/c1/a%2Fb", "alice", false, 400],
  ["GET", "/blob/c1/%E0", "alice", false, 400],
  ["PUT", "/blob/c1/m1%23c1", "alice", false, 404],
  ["GET", "/blob/bad!/part", "alice", false, 404],
  ["GET", "/blob/c1/m1%23c1", undefined, false, 401],

  ["PUT", "/attachments/a1", "alice", false, 200],
  ["GET", "/attachments/a1", "alice", false, 404],
  ["PUT", "/attachments", "alice", false, 404]
];

describe("route table", () => {
  it.each(ROUTES)("%s %s (token %s, upgrade %s) → %i", async (method, path, token, upgrade, status, forward) => {
    const { env, forwarded } = routingEnv();
    const headers: Record<string, string> = { [AUTH_USER_HEADER]: "victim" };
    if (token) headers.authorization = `Bearer ${token}`;
    if (upgrade) headers.upgrade = "websocket";
    const response = await worker.fetch(new Request(`https://edge.test${path}`, { method, headers }), env);
    expect(response.status).toBe(status);
    if (!forward) {
      expect(forwarded).toEqual([]);
      return;
    }
    expect(forwarded).toHaveLength(1);
    const { search, ...rest } = forward;
    expect(forwarded[0]).toMatchObject({ ...rest, method, user: token?.split("@")[0] });
    if (typeof search === "string") expect(forwarded[0]?.search).toBe(search);
    else expect(forwarded[0]?.search).toMatch(search);
  });

  it("serves public and blob bodies with their cache policy", async () => {
    const { env, blobReads } = routingEnv();
    const get = (path: string, method = "GET", token?: string) => worker.fetch(new Request(
      `https://edge.test${path}`, { method, headers: token ? { authorization: `Bearer ${token}` } : {} }), env);

    const health = await get("/health");
    expect(await health.json()).toEqual({ ok: true, auth: "dev" });

    const install = await get("/install.sh");
    expect(install.headers.get("content-type")).toBe("application/x-sh");
    expect(install.headers.get("cache-control")).toBe("public, max-age=0, must-revalidate");
    expect(await (await get("/install.sh", "HEAD")).text()).toBe("");

    const release = await get("/releases/stable/latest.txt");
    expect(Object.fromEntries(release.headers)).toMatchObject({
      "content-type": "text/plain; charset=utf-8",
      "content-length": "2",
      "cache-control": "public, max-age=60",
      etag: "\"stable/latest.txt\"",
      "access-control-allow-origin": "*"
    });
    expect(await release.text()).toBe("ok");
    expect(await (await get("/releases/stable/latest.txt", "HEAD")).text()).toBe("");

    const blob = await get("/blob/c1/m1%23c1", "GET", "alice");
    expect(Object.fromEntries(blob.headers)).toMatchObject({
      "content-type": "text/plain",
      etag: "\"blob/alice/c1/m1#c1\"",
      "cache-control": "private, max-age=300"
    });
    expect(await blob.text()).toBe("ok");
    expect(await (await get("/blob/c1/m1%23c1", "HEAD", "alice")).text()).toBe("");
    expect(blobReads).toEqual(["get blob/alice/c1/m1#c1", "head blob/alice/c1/m1#c1"]);

    expect(await (await get("/attachments/a1", "PUT", "alice")).json()).toEqual({ ok: true });
  });
});

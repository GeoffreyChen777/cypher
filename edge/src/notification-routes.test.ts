import { describe, expect, it, vi } from "vitest";
vi.mock("./session-room", () => ({ SessionRoom: class {} }));
vi.mock("./apns-sender", () => ({ APNsSender: class {} }));
vi.mock("./install.sh", () => ({ default: "" }));
import worker from "./index";
import type { Env } from "./env";
import { AUTH_USER_HEADER } from "./env";

describe("notification route boundaries", () => {
  it("authenticates and isolates settings/activity/register by both organization and user", async () => {
    const names: string[] = [], requests: Request[] = [];
    const env = { AUTH_MODE: "dev", WORKSPACE3: {
      idFromName(name: string) { names.push(name); return name; },
      get() { return { fetch(request: Request) { requests.push(request); return Response.json({ ok: true }); } }; }
    } } as unknown as Env;
    const call = (token?: string, org = "org", expected = token?.split("@")[0] ?? "") => worker.fetch(new Request(
      `https://edge.test/workspace3/${org}/notifications/settings`, { headers: {
        ...(token ? { Authorization: `Bearer ${token}` } : {}), [AUTH_USER_HEADER]: "victim",
        "x-cypher-expected-user": expected
      } }), env);
    expect((await call()).status).toBe(401);
    expect((await call("alice@other")).status).toBe(403);
    expect((await call("bob@org", "org", "alice")).status).toBe(403);
    expect(names).toHaveLength(0);
    expect((await call("alice@org")).status).toBe(200);
    expect((await call("bob@org")).status).toBe(200);
    expect(names).toEqual(['["workspace3","org","alice"]', '["workspace3","org","bob"]']);
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

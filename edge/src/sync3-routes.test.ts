import { describe, expect, it, vi } from "vitest";
vi.mock("./session-room", () => ({ SessionRoom: class {} }));
vi.mock("./apns-sender", () => ({ APNsSender: class {} }));
vi.mock("./install.sh", () => ({ default: "" }));
import worker from "./index";
import type { Env } from "./env";

describe("v3 opt-in authenticated routing", () => {
  function fixture() {
    const rooms: string[] = [];
    const requests: Request[] = [];
    const env = {
      AUTH_MODE: "dev", SYNC3_ENABLED: "true",
      SYNC3_ROOMS: {
        idFromName(name: string) { rooms.push(name); return name; },
        get() { return { fetch(request: Request) { requests.push(request); return Response.json({ ok: true }); } }; },
      },
    } as unknown as Env;
    const call = (bearer: string, path = "/sync3/org/chats/chat/ws") =>
      worker.fetch(new Request(`https://test${path}`, {
        headers: { authorization: `Bearer ${bearer}`, "x-cypher-auth-user": "spoofed",
          "x-cypher-auth-deadline": "9999999999999", Upgrade: "websocket" },
      }), env);
    return { rooms, requests, env, call };
  }
  it("is off by default and cannot access legacy namespaces", async () => {
    const f = fixture(); delete f.env.SYNC3_ENABLED;
    expect((await f.call("user@org")).status).toBe(404);
    expect(f.rooms).toEqual([]);
  });
  it("requires a matching org claim and structurally valid scoped route", async () => {
    const f = fixture();
    expect((await f.call("user@other")).status).toBe(403);
    expect((await f.call("user")).status).toBe(403);
    expect((await f.call("user@org","/sync3/org/chats/chat/reset")).status).toBe(404);
    expect(f.rooms).toEqual([]);
  });
  it("same chat ID in two accounts is isolated and forged headers are overwritten", async () => {
    const f = fixture();
    await f.call("first@org"); await f.call("second@org");
    expect(f.rooms).toEqual(['["sync3","org","first","chat"]','["sync3","org","second","chat"]']);
    expect(f.requests.map(r=>r.headers.get("x-cypher-auth-user"))).toEqual(["first","second"]);
    expect(f.requests.map(r=>new URL(r.url).pathname)).toEqual(["/ws","/ws"]);
    for (const request of f.requests) {
      const deadline=Number(request.headers.get("x-cypher-auth-deadline"));
      expect(deadline).toBeGreaterThan(Date.now());
      expect(deadline).toBeLessThanOrEqual(Date.now()+300_000);
    }
  });
});

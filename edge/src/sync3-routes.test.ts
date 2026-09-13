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
    const call = (bearer: string, path = "/sync3/org/chats/chat/ws", expected = bearer.split("@")[0]) =>
      worker.fetch(new Request(`https://test${path}`, {
        method: path.endsWith("/ws") ? "GET" : "POST",
        headers: { authorization: `Bearer ${bearer}`, "x-cypher-auth-user": "spoofed", "x-cypher-expected-user": expected,
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
  it("workspace3 is independently account scoped and cannot inherit forged auth headers", async () => {
    const f = fixture();
    f.env.WORKSPACE3 = f.env.SYNC3_ROOMS;
    expect((await f.call("first@other", "/workspace3/org/ws")).status).toBe(403);
    await f.call("first@org", "/workspace3/org/ws");
    await f.call("second@org", "/workspace3/org/ws");
    expect(f.rooms).toEqual(['["workspace3","org","first"]', '["workspace3","org","second"]']);
    expect(f.requests.map(r => r.headers.get("x-cypher-auth-user"))).toEqual(["first", "second"]);
    expect(f.requests.every(r => Number(r.headers.get("x-cypher-auth-deadline")) <= Date.now() + 300000)).toBe(true);
  });
  it("rejects refreshed credentials from a different captured account before selecting a DO", async () => {
    const f = fixture();
    for (const operation of ["init", "exchange", "ws"]) {
      expect((await f.call("second@org", `/sync3/org/chats/chat/${operation}`, "first")).status).toBe(403);
      expect((await f.call("second@org", `/sync3/org/chats/chat/${operation}`, "")).status).toBe(403);
    }
    expect(f.rooms).toEqual([]);
    expect(f.requests).toEqual([]);
  });
  it("never forwards retired routes or acknowledges discarded attachment bytes", async () => {
    const f = fixture();
    for (const path of ["/registry/org/ws", "/device/host/nudge", "/workspace/org/ws",
                        "/chat2/chat/rows", "/session/chat/ws", "/blob/chat/tool", "/attachments/chat"]) {
      const reply = await f.call("user@org", path);
      expect(reply.status).toBe(410);
      expect(await reply.json()).toEqual({ error: "v3_required" });
    }
    expect(f.rooms).toEqual([]);
    expect(f.requests).toEqual([]);
  });
  it("does not accept query-token transport on v3 data endpoints", async () => {
    const f = fixture(); f.env.WORKSPACE3 = f.env.SYNC3_ROOMS;
    expect((await f.call("user@org", "/sync3/org/chats/chat/ws?token=user%40org")).status).toBe(400);
    expect((await f.call("user@org", "/workspace3/org/ws?token=user%40org")).status).toBe(400);
    expect(f.rooms).toEqual([]);
  });
});

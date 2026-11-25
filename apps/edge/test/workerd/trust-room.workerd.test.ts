import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import vectors from "../../../../protocol/vectors/device-trust-v1.json";
import { AUTH_USER_HEADER } from "../../src/env";
import { TrustRoom } from "../../src/trust/trust-room";

// TrustRoom on real Durable Object SQLite, fed the shared vector chain
// (protocol/vectors/device-trust-v1.json).
const room = (name: string) => env.TRUST.get(env.TRUST.idFromName(name));

const call = (stub: DurableObjectStub, method: string, path: string, body?: unknown) =>
  stub.fetch(
    new Request(`https://trust${path}`, {
      method,
      headers: { [AUTH_USER_HEADER]: "alice" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) })
    })
  );

describe("TrustRoom", () => {
  it("appends a valid chain in order and serves it back", async () => {
    const stub = room("chain-in-order");
    for (const [i, entry] of vectors.chain.entries.entries()) {
      const response = await call(stub, "POST", "/chain", { entry });
      expect(response.status).toBe(200);
      expect(await response.json()).toEqual({ length: i + 1 });
    }
    const served = (await (await call(stub, "GET", "/chain")).json()) as { entries: unknown[] };
    const strip = ({ signingBytes: _s, hash: _h, ...entry }: Record<string, unknown>) => entry;
    expect(served.entries).toEqual(vectors.chain.entries.map(strip));
  });

  it("answers 409 to a stale sequence number and 400 to an invalid entry", async () => {
    const stub = room("chain-conflicts");
    const [genesis, second] = vectors.chain.entries;
    expect((await call(stub, "POST", "/chain", { entry: genesis })).status).toBe(200);
    const stale = await call(stub, "POST", "/chain", { entry: genesis });
    expect(stale.status).toBe(409);
    expect(await stale.json()).toEqual({ error: "conflict", length: 1 });
    const outsider = vectors.invalidChains.find((c) => c.name === "signed by a device outside the group")!;
    const refused = await call(stub, "POST", "/chain", { entry: outsider.entries[2] });
    expect(refused.status).toBe(409);
    expect((await call(stub, "POST", "/chain", { entry: second })).status).toBe(200);
    const invalid = await call(stub, "POST", "/chain", { entry: outsider.entries[2] });
    expect(invalid.status).toBe(400);
    expect(await invalid.json()).toMatchObject({ reason: "signed by a device outside the group" });
    expect((await call(stub, "POST", "/chain", { entry: { seq: 2 } })).status).toBe(400);
  });

  it("refuses every invalid vector chain somewhere along the way", async () => {
    for (const [i, invalid] of vectors.invalidChains.entries()) {
      const stub = room(`invalid-${i}`);
      const statuses = [];
      for (const entry of invalid.entries) statuses.push((await call(stub, "POST", "/chain", { entry })).status);
      expect(statuses.some((s) => s !== 200), invalid.name).toBe(true);
    }
  });

  it("keeps pairing messages per mailbox until deleted", async () => {
    const stub = room("pairing");
    const mailbox = vectors.pairing.mailbox;
    expect((await call(stub, "GET", `/pairing/${mailbox}/join`)).status).toBe(404);
    expect((await call(stub, "PUT", `/pairing/${mailbox}/join`, vectors.pairing.join)).status).toBe(200);
    expect(await (await call(stub, "GET", `/pairing/${mailbox}/join`)).json()).toEqual(vectors.pairing.join);
    expect((await call(stub, "PUT", `/pairing/${mailbox}/welcome`, vectors.pairing.welcome)).status).toBe(200);
    expect((await call(stub, "PUT", `/pairing/${mailbox}/other`, {})).status).toBe(404);
    expect((await call(stub, "PUT", `/pairing/NOT-HEX/join`, {})).status).toBe(404);
    expect((await call(stub, "PUT", `/pairing/${mailbox}/join`, "x".repeat(20_000))).status).toBe(413);
    expect((await call(stub, "DELETE", `/pairing/${mailbox}`)).status).toBe(200);
    expect((await call(stub, "GET", `/pairing/${mailbox}/welcome`)).status).toBe(404);
  });

  it("requires the stamped user", async () => {
    const response = await room("anonymous").fetch(new Request("https://trust/chain"));
    expect(response.status).toBe(401);
  });
});

// Chain-head pushes (T1, docs/design/do-cost.md): an append reaches the
// user's registry sockets that declared the `trust` capability, and only
// those — older clients redial on a frame type they do not know.
describe("TrustRoom → RegistryRoom chain head", () => {
  const registry = (name: string) => env.REGISTRY_ROOMS.get(env.REGISTRY_ROOMS.idFromName(name));
  const trustRoom = (user: string) => env.TRUST_ROOMS.get(env.TRUST_ROOMS.idFromName(`t1/${user}`));

  const join = async (org: string, user: string, hello: Record<string, unknown>) => {
    const response = await registry(`reg1/${org}/${user}`).fetch(`https://registry/ws?org=${org}&device=d`, {
      headers: { Upgrade: "websocket", [AUTH_USER_HEADER]: user }
    });
    expect(response.status).toBe(101);
    const ws = response.webSocket!;
    const frames: Record<string, unknown>[] = [];
    ws.addEventListener("message", (event) => {
      frames.push(JSON.parse(event.data as string));
    });
    ws.accept();
    ws.send(JSON.stringify({ t: "hello", cursor: null, device: "d", ...hello }));
    return { frames, close: () => ws.close(1000, "bye") };
  };

  const until = async (check: () => boolean) => {
    for (let i = 0; i < 200 && !check(); i++) await new Promise((r) => setTimeout(r, 10));
    expect(check()).toBe(true);
  };

  const append = (user: string, entry: unknown) =>
    trustRoom(user).fetch(
      new Request("https://trust/chain", {
        method: "POST",
        headers: { [AUTH_USER_HEADER]: user },
        body: JSON.stringify({ entry })
      })
    );

  it("pushes an append to a capable socket and never to a plain one; state carries the length", async () => {
    const [genesis, second] = vectors.chain.entries;
    expect((await append("bob", genesis)).status).toBe(200);
    const capable = await join("org", "bob", { caps: ["trust"] });
    const plain = await join("org", "bob", {});
    await until(() => capable.frames.some((f) => f.t === "state"));
    await until(() => plain.frames.some((f) => f.t === "state"));
    // The first capable hello subscribes and then reports the length it learned.
    await until(() => capable.frames.some((f) => f.t === "trust" && f.length === 1));

    expect((await append("bob", second)).status).toBe(200);
    await until(() => capable.frames.some((f) => f.t === "trust" && f.length === 2));
    // A frame the plain socket would have redialed on never reached it.
    expect(plain.frames.filter((f) => f.t === "trust")).toEqual([]);
    expect(plain.frames.find((f) => f.t === "state")).not.toHaveProperty("trust");

    // A later capable hello finds the length in `state`; a plain one does not.
    const late = await join("org", "bob", { caps: ["trust"] });
    const latePlain = await join("org", "bob", { caps: ["other"] });
    await until(() => late.frames.some((f) => f.t === "state"));
    await until(() => latePlain.frames.some((f) => f.t === "state"));
    expect(late.frames.find((f) => f.t === "state")).toMatchObject({ trust: { length: 2 } });
    expect(latePlain.frames.find((f) => f.t === "state")).not.toHaveProperty("trust");
    for (const socket of [capable, plain, late, latePlain]) socket.close();
  });

  it("refuses the room-to-room routes when the user header is present", async () => {
    const pushed = await registry("reg1/org/carol").fetch("https://registry/trust-head", {
      method: "POST",
      headers: { [AUTH_USER_HEADER]: "carol" },
      body: JSON.stringify({ length: 9 })
    });
    expect(pushed.status).toBe(404);
    const subscribed = await trustRoom("carol").fetch("https://trust/head?subscribe=reg1/org/carol", {
      headers: { [AUTH_USER_HEADER]: "carol" }
    });
    expect(subscribed.status).toBe(404);
    // Without the header both answer, and a bad subscriber name is refused.
    expect((await trustRoom("carol").fetch("https://trust/head?subscribe=nope")).status).toBe(400);
    const ok = await trustRoom("carol").fetch("https://trust/head?subscribe=reg1/org/carol");
    expect(await ok.json()).toEqual({ length: 0 });
  });

  it("retries a failed push from the alarm with a doubling backoff", async () => {
    const calls: { room: string; length: number }[] = [];
    let failing = true;
    const registries = {
      idFromName: (name: string) => name,
      get: (room: string) => ({
        fetch: async (_url: string, init: RequestInit) => {
          calls.push({ room, length: (JSON.parse(init.body as string) as { length: number }).length });
          return new Response(null, { status: failing ? 500 : 200 });
        }
      })
    };
    const fakeEnv = { REGISTRY_ROOMS: registries } as unknown as import("../../src/env").Env;
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("trust-retry")), async (_, state) => {
      const room = new TrustRoom(state, fakeEnv);
      expect((await room.fetch(new Request("https://trust/head?subscribe=reg1/o/u"))).status).toBe(200);
      const before = Date.now();
      const response = await room.fetch(
        new Request("https://trust/chain", {
          method: "POST",
          headers: { [AUTH_USER_HEADER]: "u" },
          body: JSON.stringify({ entry: vectors.chain.entries[0] })
        })
      );
      expect(response.status).toBe(200);
      await until(() => calls.length === 1);
      await until(() => [...state.storage.sql.exec("SELECT 1 FROM subscribers WHERE pending = 1")].length === 1);
      const first = (await state.storage.getAlarm())!;
      expect(first - before).toBeGreaterThanOrEqual(5_000);
      expect(first - before).toBeLessThan(6_000);

      // Still failing: the alarm retries and doubles the wait.
      const second = Date.now();
      await room.alarm();
      expect(calls).toEqual([{ room: "reg1/o/u", length: 1 }, { room: "reg1/o/u", length: 1 }]);
      const next = (await state.storage.getAlarm())!;
      expect(next - second).toBeGreaterThanOrEqual(10_000);
      expect(next - second).toBeLessThan(11_000);

      // Recovered: delivered, nothing pending, backoff back to the base.
      failing = false;
      await room.alarm();
      expect(calls).toHaveLength(3);
      expect([...state.storage.sql.exec("SELECT 1 FROM subscribers WHERE pending = 1")]).toHaveLength(0);
      expect([...state.storage.sql.exec("SELECT value FROM kv WHERE key = 'retryMs'")][0]?.value).toBe("5000");
    });
  });
});

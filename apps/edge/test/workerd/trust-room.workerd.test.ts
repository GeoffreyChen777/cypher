import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import vectors from "../../../../protocol/vectors/device-trust-v1.json";
import { AUTH_USER_HEADER } from "../../src/env";

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

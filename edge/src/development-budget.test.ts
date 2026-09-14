import { describe, it, expect } from "vitest";
import { admit, LIMITS, type Budget } from "./development-budget";
import { verifyToken } from "./auth";
import type { Env } from "./env";

function first(): Budget {
  const result = admit(undefined, "room-1", 0);
  if (!result.allowed) throw new Error("initial admission failed");
  return result.budget;
}

describe("development isolation", () => {
  it("fails closed without a secret, with wrong tokens and with public dev identities", async () => {
    for (const secret of [undefined, "secret-for-test"]) {
      const env = { AUTH_MODE: "dev-locked", DEV_ACCESS_TOKEN: secret } as Env;
      for (const token of ["", "dev-user", "dev-user@dev-org", "wrong"]) {
        expect(await verifyToken(env, token)).toBeUndefined();
      }
    }
  });
  it("returns a fixed non-secret identity only for the configured token", async () => {
    const env = { AUTH_MODE: "dev-locked", DEV_ACCESS_TOKEN: "secret-for-test" } as Env;
    expect(await verifyToken(env, "secret-for-test")).toEqual({ userId: "dev-user", orgId: "dev-org" });
    expect(await verifyToken(env, "secret-for-test@another-org")).toBeUndefined();
  });
  it("preserves local test dev auth", async () => {
    expect(await verifyToken({ AUTH_MODE: "dev" } as Env, "alice@org")).toEqual({ userId: "alice", orgId: "org" });
  });
});

describe("development budget", () => {
  it("counts admissions including the accounting write without mutating prior state", () => {
    const old = first();
    const next = admit(old, "room-1", 0);
    expect(old.events).toBe(1);
    expect(next).toMatchObject({ allowed: true, budget: { events: 2, minuteEvents: 2, rows: 2, rooms: ["room-1"] } });
  });
  it("rate limits across callers, and does not write when rejecting", () => {
    const budget = { ...first(), minuteEvents: LIMITS.perMinute };
    expect(admit(budget, "room-2", 1000)).toMatchObject({ allowed: false });
    expect(budget.rooms).toEqual(["room-1"]);
    expect(admit(budget, "room-2", 60000)).toMatchObject({ allowed: true, budget: { minuteEvents: 1 } });
  });
  it("enforces both event and observed SQL write tripwires", () => {
    expect(admit({ ...first(), events: LIMITS.events }, "room-1", 0)).toMatchObject({ allowed: false });
    expect(admit({ ...first(), rows: LIMITS.rows }, "room-1", 0)).toMatchObject({ allowed: false });
    expect(admit({ ...first(), rows: LIMITS.rows + 500 }, "room-1", 0)).toMatchObject({ allowed: false });
  });
  it("bounds in-flight work and permits crash recovery after lease expiry", () => {
    let budget = first();
    for (let i = 1; i < LIMITS.concurrent; i++) {
      const next = admit(budget, "room-1", 0);
      if (!next.allowed) throw new Error("unexpected rejection");
      budget = next.budget;
    }
    expect(admit(budget, "room-1", 0)).toMatchObject({ allowed: false });
    expect(admit(budget, "room-1", 120001)).toMatchObject({ allowed: true });
    expect(admit({ ...first(), rows: LIMITS.rows - 100 }, "room-1", 0)).toMatchObject({ allowed: false });
  });
  it("resets daily counters at UTC midnight, not the room allowlist", () => {
    const rooms = Array.from({ length: LIMITS.rooms }, (_, i) => `room-${i}`);
    const old = { ...first(), rows: LIMITS.rows, events: LIMITS.events, rooms };
    expect(admit(old, "room-1", 86400000)).toMatchObject({ allowed: true, budget: { events: 1, rows: 1 } });
    expect(admit(old, "new-room", 86400000)).toMatchObject({ allowed: false });
  });
});

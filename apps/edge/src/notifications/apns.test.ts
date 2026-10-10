import { afterEach, describe, expect, it, vi } from "vitest";
import { exportPKCS8, generateKeyPair } from "jose";
import { APNS_TOPIC, notificationsAvailable, sendAPNs, type PushMessage } from "./apns";
import type { Env } from "./env";
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });
async function configured(): Promise<Env> {
  const key = await generateKeyPair("ES256", { extractable: true });
  return { NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456", APNS_KEY_ID: "TESTKEY001",
    APNS_PRIVATE_KEY: await exportPKCS8(key.privateKey), PUSH_DEVICES: {} } as Env;
}
const message = (): PushMessage => ({ id: crypto.randomUUID(), scope: "a".repeat(64),
  chatId: "chat", projectId: "project", kind: "completed", expires: Date.now() + 60_000 });
describe("APNs transport", () => {
  it("diagnoses provider-key failure without disclosing key material", async () => {
    const log = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    const env = await configured();
    env.APNS_PRIVATE_KEY = "invalid-sensitive-private-key";
    expect(await sendAPNs(env, "0".repeat(64), "production", message())).toBe("retry");
    expect(fetcher).not.toHaveBeenCalled();
    expect(log).toHaveBeenCalledWith("apns_delivery", JSON.stringify({ stage: "provider_token_failed", error: "TypeError" }));
    expect(JSON.stringify(log.mock.calls)).not.toContain(env.APNS_PRIVATE_KEY);
  });
  it("logs only allowlisted APNs reasons and never raw exceptions or bodies", async () => {
    const log = vi.spyOn(console, "warn").mockImplementation(() => {});
    const env = await configured();
    vi.stubGlobal("fetch", async () => Response.json({ reason: "InvalidProviderToken", token: "sensitive" }, { status: 403 }));
    await sendAPNs(env, "0".repeat(64), "production", message());
    expect(log).toHaveBeenLastCalledWith("apns_delivery",
      JSON.stringify({ stage: "rejected", status: 403, reason: "InvalidProviderToken" }));
    vi.stubGlobal("fetch", async () => Response.json({ reason: "sensitive-response" }, { status: 500 }));
    await sendAPNs(env, "0".repeat(64), "production", message());
    expect(log).toHaveBeenLastCalledWith("apns_delivery",
      JSON.stringify({ stage: "rejected", status: 500, reason: "Other" }));
    vi.stubGlobal("fetch", async () => { throw new Error("sensitive-url"); });
    await sendAPNs(env, "0".repeat(64), "production", message());
    expect(log).toHaveBeenLastCalledWith("apns_delivery", JSON.stringify({ stage: "transport_failed", error: "Error" }));
    expect(JSON.stringify(log.mock.calls)).not.toContain("sensitive");
  });
  it("is disabled unless explicitly configured", async () => {
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    expect(notificationsAvailable({} as Env)).toBe(false);
    expect(await sendAPNs({} as Env, "0".repeat(64), "production", message())).toBe("retry");
    expect(fetcher).not.toHaveBeenCalled();
  });
  it("uses the correct Apple environment and only generic lock-screen text", async () => {
    const requests: { url: string; options: RequestInit }[] = [];
    vi.stubGlobal("fetch", async (url: string, options: RequestInit) => {
      requests.push({ url, options }); return new Response(null, { status: 200 });
    });
    const env = await configured();
    expect(await sendAPNs(env, "0".repeat(64), "production", message())).toBe("sent");
    expect(await sendAPNs(env, "0".repeat(64), "development", message())).toBe("sent");
    expect(new URL(requests[0].url).hostname).toBe("api.push.apple.com");
    expect(new URL(requests[1].url).hostname).toBe("api.sandbox.push.apple.com");
    const headers = new Headers(requests[0].options.headers);
    expect(headers.get("apns-topic")).toBe(APNS_TOPIC);
    expect(headers.get("authorization")?.startsWith("bearer ")).toBe(true);
    const body = JSON.parse(requests[0].options.body as string);
    expect(body.aps.alert).toEqual({ title: "Task completed", body: "Open Cypher to view the session." });
    expect(Object.keys(body.cypher).sort()).toEqual(["chatId", "eventId", "kind", "projectId", "scope", "version"]);
    expect(headers.get("authorization") === new Headers(requests[1].options.headers).get("authorization")).toBe(true);
  });
  it("invalidates bad device tokens and retries transient/provider failures", async () => {
    const env = await configured();
    for (const [status, reason, expected] of [
      [410, "Unregistered", "invalid"], [400, "BadDeviceToken", "invalid"],
      [429, "TooManyRequests", "retry"], [503, "ServiceUnavailable", "retry"],
      [403, "InvalidProviderToken", "retry"]
    ] as const) {
      vi.stubGlobal("fetch", async () => Response.json({ reason }, { status }));
      expect(await sendAPNs(env, "0".repeat(64), "production", message())).toBe(expected);
    }
  });
  it("sends absolute badges on alerts and badge-only zero without alert or sound", async () => {
    const requests: RequestInit[] = [];
    vi.stubGlobal("fetch", async (_: string, options: RequestInit) => {
      requests.push(options); return new Response(null, { status: 200 });
    });
    const env = await configured();
    await sendAPNs(env, "0".repeat(64), "production", { ...message(), badgeCount: 3, badgeRevision: 7 });
    await sendAPNs(env, "0".repeat(64), "production", {
      id: crypto.randomUUID(), scope: "a".repeat(64), kind: "badge", expires: Date.now() + 60_000,
      badgeCount: 0, badgeRevision: 8
    });
    const alert = JSON.parse(requests[0].body as string), badge = JSON.parse(requests[1].body as string);
    expect(alert.aps.badge).toBe(3);
    expect(badge.aps).toEqual({ badge: 0 });
    expect(badge.cypher).toMatchObject({ kind: "badge", badgeCount: 0, badgeRevision: 8 });
    expect(badge.cypher.chatId).toBeUndefined();
    expect(new Headers(requests[1].headers).get("apns-push-type")).toBe("alert");
    expect(new Headers(requests[1].headers).get("apns-priority")).toBe("5");
  });
  it("rejects malformed badge counts before contacting Apple", async () => {
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    const env = await configured();
    for (const badgeCount of [-1, 1.5, Number.MAX_SAFE_INTEGER]) {
      expect(await sendAPNs(env, "0".repeat(64), "production",
        { ...message(), badgeCount, badgeRevision: 1 })).toBe("retry");
    }
    expect(fetcher).not.toHaveBeenCalled();
  });
});

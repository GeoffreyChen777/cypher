/**
 * The viewport's periodic activity refresh rides the registry room's presence
 * frame instead of its own HTTP request.
 *
 * Why it matters: presence already flows every 15s on an open socket and bills
 * 20:1, while the identical report over HTTP billed 1:1. Measured on
 * production, that HTTP heartbeat was 377 requests/hour — 56% of all billable
 * Durable Object requests.
 *
 * What must stay true: both transports land in one implementation, so stored
 * state cannot diverge by transport; a transition over HTTP still returns the
 * reply the caller needs; and a malformed piggyback can never take down the
 * socket, because presence is liveness first.
 */
import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { RegistryRoom } from "../../src/registry-room";
import { AUTH_USER_HEADER, type Env } from "../../src/env";

// notificationsAvailable() gates the whole path; it needs a complete APNs
// config, not just the flag. Public test values, never used outside workerd.
const ENV = {
  NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456",
  APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test",
  PUSH_DEVICES: {} as DurableObjectNamespace
} as unknown as Env;

/** A socket the room treats as a joined peer. */
function peer(device: string) {
  let attachment: unknown = { userId: "u", device, ready: true };
  const frames: string[] = [];
  return {
    frames,
    ws: {
      deserializeAttachment: () => attachment,
      serializeAttachment: (v: unknown) => { attachment = v; },
      send: (s: string) => frames.push(s),
      close: () => {}
    } as unknown as WebSocket
  };
}

const activity = (over: Record<string, unknown> = {}) => ({
  clientId: "desktop-1", sequence: 1, platform: "desktop",
  foreground: true, interactionAgeMs: 0, chatId: "chat-a", ...over
});

const readActivity = (state: DurableObjectState) => {
  const rows = [...state.storage.sql.exec("SELECT value FROM notify_kv WHERE key = 'activity'")];
  return rows[0] ? JSON.parse(rows[0].value as string) : [];
};
const readTarget = (state: DurableObjectState, chatId: string) => {
  const rows = [...state.storage.sql.exec(
    "SELECT value FROM notify_kv WHERE key = ?", `target:${chatId}`)];
  return rows[0] ? JSON.parse(rows[0].value as string) : undefined;
};

it("a presence frame carrying activity stores exactly what the HTTP report stores", async () => {
  const viaFrame = await runInDurableObject(
    env.TEST_LOG.get(env.TEST_LOG.idFromName("act-presence-frame")),
    async (_, state) => {
      const room = new RegistryRoom(state, ENV);
      const p = peer("device-1");
      await room.webSocketMessage(p.ws, JSON.stringify({
        t: "presence", at: Date.now(), activity: activity()
      }));
      return { list: readActivity(state), target: readTarget(state, "chat-a") };
    });

  const viaHttp = await runInDurableObject(
    env.TEST_LOG.get(env.TEST_LOG.idFromName("act-presence-http")),
    async (_, state) => {
      const room = new RegistryRoom(state, ENV);
      const response = await room.fetch(new Request(
        "https://registry/notifications/activity",
        { method: "POST", headers: { [AUTH_USER_HEADER]: "u", "content-type": "application/json" },
          body: JSON.stringify(activity()) }));
      expect(response.status).toBe(200);
      return { list: readActivity(state), target: readTarget(state, "chat-a") };
    });

  expect(viaFrame.list).toHaveLength(1);
  expect(viaFrame.list[0].chatId).toBe("chat-a");
  expect(viaFrame.list[0].foreground).toBe(true);
  expect(viaFrame.list[0].platform).toBe("desktop");
  expect(viaFrame.target?.clientId).toBe("desktop-1");

  // Same shape from both transports, ignoring the wall clock each recorded.
  const strip = (v: any) => ({ ...v, receivedAt: 0, interactionAt: 0, openedAt: 0 });
  expect(strip(viaFrame.list[0])).toEqual(strip(viaHttp.list[0]));
  expect(viaFrame.target?.platform).toBe(viaHttp.target?.platform);
});

it("the piggyback still relays presence, and costs no extra inbound message", async () => {
  await runInDurableObject(
    env.TEST_LOG.get(env.TEST_LOG.idFromName("act-presence-relay")),
    async (_, state) => {
      const a = peer("device-a"), b = peer("device-b");
      const room = new RegistryRoom(
        new Proxy(state, { get(t, k) {
          if (k === "getWebSockets") return () => [a.ws, b.ws];
          const v = Reflect.get(t, k, t);
          return typeof v === "function" ? v.bind(t) : v;
        } }) as DurableObjectState, ENV);
      // ONE frame carries both the beat and the refresh.
      await room.webSocketMessage(a.ws, JSON.stringify({
        t: "presence", at: 1234, activity: activity()
      }));
      const relayed = b.frames.map(f => JSON.parse(f)).filter(f => f.t === "presence");
      expect(relayed).toHaveLength(1);
      expect(relayed[0].device).toBe("device-a");
      expect(relayed[0].at).toBe(1234);
      // The relay must not leak the viewport's activity to other devices.
      expect(relayed[0].activity).toBeUndefined();
      expect(readActivity(state)).toHaveLength(1);
    });
});

it("a malformed piggyback leaves presence working and the socket open", async () => {
  await runInDurableObject(
    env.TEST_LOG.get(env.TEST_LOG.idFromName("act-presence-bad")),
    async (_, state) => {
      const room = new RegistryRoom(state, ENV);
      const p = peer("device-1");
      for (const bad of [
        { clientId: "x" },                                    // missing fields
        { ...activity(), sequence: -1 },                      // invalid sequence
        { ...activity(), platform: "toaster" },               // unknown platform
        { ...activity(), chatId: "../../escape" },            // invalid id
        { ...activity(), interactionAgeMs: 99_999_999_999 }   // out of range
      ]) {
        await room.webSocketMessage(p.ws, JSON.stringify({
          t: "presence", at: Date.now(), activity: bad
        }));
      }
      // Nothing stored and nothing thrown.
      expect(readActivity(state)).toHaveLength(0);
      // The room is still healthy: a well-formed refresh right after the bad
      // ones is accepted normally.
      await room.webSocketMessage(p.ws, JSON.stringify({
        t: "presence", at: Date.now(), activity: activity({ sequence: 9 })
      }));
      const list = readActivity(state);
      expect(list).toHaveLength(1);
      expect(list[0].sequence).toBe(9);
    });
});

it("an out-of-order refresh cannot replay a read or refresh a stale lease", async () => {
  await runInDurableObject(
    env.TEST_LOG.get(env.TEST_LOG.idFromName("act-presence-order")),
    async (_, state) => {
      const room = new RegistryRoom(state, ENV);
      const p = peer("device-1");
      const send = (a: Record<string, unknown>) => room.webSocketMessage(
        p.ws, JSON.stringify({ t: "presence", at: Date.now(), activity: a }));
      await send(activity({ sequence: 5, chatId: "chat-a" }));
      await send(activity({ sequence: 2, chatId: "chat-old" }));  // stale, must lose
      const list = readActivity(state);
      expect(list).toHaveLength(1);
      expect(list[0].chatId).toBe("chat-a");
      expect(list[0].sequence).toBe(5);
      expect(readTarget(state, "chat-old")).toBeUndefined();
    });
});

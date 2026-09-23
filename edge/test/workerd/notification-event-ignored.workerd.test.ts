/**
 * An ignored notification event says WHY it was ignored.
 *
 * Why it matters: a host re-reports a running chat's state on every 15s
 * heartbeat until the Worker gives it a receipt. An ignored report used to be
 * indistinguishable from a network failure, so a chat the Worker would never
 * notify for was re-reported forever -- measured at 245 requests in 30 minutes
 * from one host, the largest line on the Durable Object bill. The reason lets
 * the host back off, and lets a person reading that host's log see which rule
 * fired, instead of guessing from the outside.
 *
 * What must stay true: the checks and their ORDER are unchanged, a chat that
 * passes them is recorded exactly as before, and the reason never leaks
 * anything the caller could not already see.
 */
import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { RegistryRoom } from "../../src/registry-room";
import { AUTH_USER_HEADER, type Env } from "../../src/env";

const ENV = { NOTIFICATIONS_ENABLED: "false" } as unknown as Env;
const at = (i: number) => `1788998400000-${String(i).padStart(6, "0")}-host`;

const req = (path: string, body: unknown) => new Request(`https://registry${path}`, {
  method: "POST",
  headers: { [AUTH_USER_HEADER]: "u", "content-type": "application/json" },
  body: JSON.stringify(body)
});

async function push(room: RegistryRoom, i: number, kind: string, id: string, set: Record<string, unknown>) {
  const response = await room.fetch(req("/push?device=host", {
    batch: `b${i}`, ops: [{ kind, id, op: "upsert", hlc: at(i), set }]
  }));
  expect(response.status).toBe(200);
}

const event = (room: RegistryRoom, chatId: string, deviceId = "host") =>
  room.fetch(req("/notifications/event", {
    chatId, deviceId, status: "working", startedAt: Date.now(), updatedAt: Date.now(), subagents: []
  })).then(r => r.json() as Promise<Record<string, unknown>>);

it("each ignore rule names itself, in the original order", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("event-ignored")), async (_, state) => {
    const room = new RegistryRoom(state, ENV);
    try {
      // No row yet: the only case a quick retry can fix.
      expect(await event(room, "missing")).toEqual({ ok: true, ignored: true, reason: "chat" });

      await push(room, 1, "spaces", "space-a", { id: "space-a", path: "/tmp" });
      await push(room, 2, "chats", "archived", { id: "archived", deviceId: "host", spaceId: "space-a", archived: true });
      await push(room, 3, "chats", "elsewhere", { id: "elsewhere", deviceId: "other", spaceId: "space-a" });
      await push(room, 4, "chats", "orphan", { id: "orphan", deviceId: "host", spaceId: "gone" });
      await push(room, 5, "chats", "live", { id: "live", deviceId: "host", spaceId: "space-a" });

      expect(await event(room, "archived")).toEqual({ ok: true, ignored: true, reason: "archived" });
      expect(await event(room, "elsewhere")).toEqual({ ok: true, ignored: true, reason: "device" });
      expect(await event(room, "orphan")).toEqual({ ok: true, ignored: true, reason: "space" });
      // Archived wins over a device mismatch, as the old single condition did.
      await push(room, 6, "chats", "both", { id: "both", deviceId: "other", spaceId: "space-a", archived: true });
      expect((await event(room, "both", "host")).reason).toBe("archived");

      // A chat that passes every check is recorded, not ignored.
      const recorded = await event(room, "live");
      expect(recorded.ok).toBe(true);
      expect(recorded.ignored).toBeUndefined();
      expect(recorded.reason).toBeUndefined();
    } finally {
      await state.storage.deleteAlarm();
    }
  });
});

import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { verifyToken } from "../../src/auth/auth";
import type { BotFrame } from "../../src/bots/bot-frames";
import { AUTH_USER_HEADER, type Env } from "../../src/env";
import { handleRoomRoute } from "../../src/room-routes";
import { routeContext } from "../../src/router";

// BotRoom through the real route table, with real BotRoom and DeviceRoom
// Durable Objects behind it (dev-mode bearers: the token is the user id).

const edge = env as unknown as Env;

interface Reply {
  status: number;
  body: Record<string, unknown>;
}

const call = async (token: string, method: string, path: string, body?: unknown): Promise<Reply> => {
  const request = new Request(`https://edge.test${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) })
  });
  const auth = await verifyToken(edge, token);
  if (!auth) throw new Error("dev auth failed");
  const response = (await handleRoomRoute({ ...routeContext(request, edge), auth })) ?? Response.json({}, { status: 418 });
  return { status: response.status, body: (await response.json()) as Record<string, unknown> };
};

const uid = () => crypto.randomUUID().slice(0, 8);

/** A published bot of `user` hosted on `device`. */
const publish = async (user: string, name: string, device = `dev-${user}-${uid()}`) => {
  const room = `room-${name.toLowerCase().replace(/\W+/g, "-")}-${uid()}`;
  const reply = await call(user, "POST", `/bots/${room}/publish`, { name, ownerName: user, hostDeviceId: device });
  expect(reply.status).toBe(200);
  return { user, room, device };
};

type Bot = Awaited<ReturnType<typeof publish>>;

const inbox = async (bot: Bot): Promise<BotFrame[]> => {
  const reply = await call(bot.user, "GET", `/bots/${bot.room}/inbox`);
  expect(reply.status).toBe(200);
  return reply.body.entries as BotFrame[];
};

const invite = async (bot: Bot, body: unknown = {}) => {
  const reply = await call(bot.user, "POST", `/bots/${bot.room}/invites`, body);
  expect(reply.status).toBe(200);
  return reply.body as { inviteId: string; code: string; expiresAt: number; maxUses: number };
};

/** `a` invites, `b` redeems; both links acknowledged. */
const connect = async (a: Bot, b: Bot): Promise<string> => {
  const { code } = await invite(a);
  const reply = await call(b.user, "POST", `/bots/${b.room}/connect`, { code });
  expect(reply.status).toBe(200);
  const connectionId = reply.body.connectionId as string;
  for (const bot of [a, b]) {
    await call(bot.user, "POST", `/bots/${bot.room}/ack`, { links: [connectionId] });
  }
  return connectionId;
};

const ask = (bot: Bot, connectionId: string, text = "How is the migration going?", exchangeId = `ex-${uid()}`) =>
  call(bot.user, "POST", `/bots/${bot.room}/ask`, { exchangeId, connectionId, text });

/** Make `user` the owner of `device`'s DeviceRoom, as its host's first join does. */
const claimDevice = async (device: string, user: string) => {
  const stub = env.DEVICE_ROOMS.get(env.DEVICE_ROOMS.idFromName(`d2/${device}`));
  await runInDurableObject(stub, async (_, state) => {
    state.storage.sql.exec("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
    state.storage.sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('owner', ?)", user);
  });
};

const pendingBotNudges = async (device: string): Promise<string[]> => {
  const stub = env.DEVICE_ROOMS.get(env.DEVICE_ROOMS.idFromName(`d2/${device}`));
  return runInDurableObject(stub, async (_, state) => {
    try {
      return [...state.storage.sql.exec("SELECT room_id FROM pending_bot_nudges")].map((r) => r.room_id as string);
    } catch {
      return [];
    }
  });
};

const roomSql = <T>(bot: Bot, run: (sql: SqlStorage) => T): Promise<T> =>
  runInDurableObject(env.BOT_ROOMS.get(env.BOT_ROOMS.idFromName(`bot1/${bot.room}`)), async (_, state) =>
    run(state.storage.sql)
  );

describe("BotRoom ownership", () => {
  it("is claimed on publish and owner-only after", async () => {
    const alice = await publish("alice", "Auth bot");
    expect((await call("bob", "POST", `/bots/${alice.room}/publish`, { name: "Mine", hostDeviceId: "d1" })).status).toBe(403);
    expect((await call("bob", "GET", `/bots/${alice.room}/inbox`)).status).toBe(403);
    expect((await call("bob", "POST", `/bots/${alice.room}/invites`, {})).status).toBe(403);
    expect((await call("alice", "POST", `/bots/${alice.room}/publish`, { name: "Renamed", ownerName: "Alice", hostDeviceId: "d1" })).status).toBe(200);
    const unpublished = await call("alice", "GET", `/bots/room-never-${uid()}/inbox`);
    expect(unpublished).toMatchObject({ status: 404, body: { error: "not_published" } });
  });

  it("answers peer paths only to other rooms, never to a forwarded request", async () => {
    const alice = await publish("alice", "Auth bot");
    const stub = env.BOT_ROOMS.get(env.BOT_ROOMS.idFromName(`bot1/${alice.room}`));
    const forged = await stub.fetch(
      new Request("https://bot/peer/revoke", {
        method: "POST",
        headers: { [AUTH_USER_HEADER]: "mallory" },
        body: JSON.stringify({ connectionId: "x", room: "y" })
      })
    );
    expect(forged.status).toBe(404);
  });
});

describe("BotRoom invites", () => {
  it("connects both sides with profiles, once per invite", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const carol = await publish("carol", "Ops bot");
    const created = await invite(alice);
    expect(created.code.startsWith(`${alice.room}.`)).toBe(true);
    expect(created.maxUses).toBe(1);
    expect(created.expiresAt - Date.now()).toBeGreaterThan(6.9 * 24 * 3_600_000);
    const stored = await roomSql(alice, (sql) => [...sql.exec("SELECT hash FROM invites")].map((r) => r.hash as string));
    expect(stored).toHaveLength(1);
    expect(stored[0]).toMatch(/^[0-9a-f]{64}$/);
    expect(created.code).not.toContain(stored[0]!);

    const redeemed = await call("bob", "POST", `/bots/${bob.room}/connect`, { code: created.code });
    expect(redeemed.status).toBe(200);
    const connectionId = redeemed.body.connectionId as string;
    expect(redeemed.body.peer).toEqual({ botRoomId: alice.room, name: "Auth bot", ownerName: "alice" });

    expect(await inbox(alice)).toEqual([
      { t: "link", connectionId, op: "added", peer: { botRoomId: bob.room, name: "Release bot", ownerName: "bob" }, inviteId: created.inviteId }
    ]);
    expect(await inbox(bob)).toEqual([
      { t: "link", connectionId, op: "added", peer: { botRoomId: alice.room, name: "Auth bot", ownerName: "alice" } }
    ]);
    await call("alice", "POST", `/bots/${alice.room}/ack`, { links: [connectionId] });
    expect(await inbox(alice)).toEqual([]);

    const again = await call("carol", "POST", `/bots/${carol.room}/connect`, { code: created.code });
    expect(again).toMatchObject({ status: 404, body: { error: "invite_invalid" } });
    const twice = await call("bob", "POST", `/bots/${bob.room}/connect`, { code: (await invite(alice)).code });
    expect(twice).toMatchObject({ status: 409, body: { error: "already_connected" } });
  });

  it("refuses expired, revoked, forged and same-owner codes", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const short = await invite(alice, { ttlMs: 10 * 60_000, maxUses: 2 });
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 11 * 60_000);
    try {
      const late = await call("bob", "POST", `/bots/${bob.room}/connect`, { code: short.code });
      expect(late).toMatchObject({ status: 404, body: { error: "invite_invalid" } });
    } finally {
      clock.mockRestore();
    }
    const revoked = await invite(alice);
    expect((await call("alice", "DELETE", `/bots/${alice.room}/invites/${revoked.inviteId}`)).status).toBe(200);
    expect((await call("bob", "POST", `/bots/${bob.room}/connect`, { code: revoked.code })).status).toBe(404);
    const forged = `${alice.room}.${"A".repeat(43)}`;
    expect((await call("bob", "POST", `/bots/${bob.room}/connect`, { code: forged })).status).toBe(404);
    expect((await call("bob", "POST", `/bots/${bob.room}/connect`, { code: "not a code" })).status).toBe(400);
    const second = await publish("alice", "Second bot");
    const own = await call("alice", "POST", `/bots/${second.room}/connect`, { code: (await invite(alice)).code });
    expect(own).toMatchObject({ status: 403, body: { error: "same_owner" } });
    expect(await inbox(bob)).toEqual([]);
  });
});

describe("BotRoom exchanges", () => {
  it("delivers a question and its answer across rooms, with states", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const connectionId = await connect(alice, bob);
    const sent = await ask(bob, connectionId, "Is the auth migration done?", "ex-1");
    expect(sent.status).toBe(200);
    const sentAt = sent.body.sentAt as number;
    expect(await inbox(alice)).toEqual([{ t: "ask", exchangeId: "ex-1", connectionId, text: "Is the auth migration done?", sentAt }]);
    expect(await inbox(bob)).toEqual([{ t: "state", exchangeId: "ex-1", state: "queued" }]);
    // A retried ask is the same exchange.
    expect((await ask(bob, connectionId, "Is the auth migration done?", "ex-1")).body.sentAt).toBe(sentAt);

    await call("alice", "POST", `/bots/${alice.room}/ack`, { asks: ["ex-1"] });
    expect(await inbox(alice)).toEqual([]);
    expect(await inbox(bob)).toEqual([{ t: "state", exchangeId: "ex-1", state: "delivered" }]);
    expect((await call("alice", "POST", `/bots/${alice.room}/state`, { exchangeId: "ex-1", state: "working" })).status).toBe(200);
    expect(await inbox(bob)).toEqual([{ t: "state", exchangeId: "ex-1", state: "working" }]);
    await call("bob", "POST", `/bots/${bob.room}/ack`, { states: [{ exchangeId: "ex-1", state: "working" }] });
    expect(await inbox(bob)).toEqual([]);

    const answered = await call("alice", "POST", `/bots/${alice.room}/answer`, {
      exchangeId: "ex-1",
      status: "answered",
      text: "Yes, shipped Tuesday."
    });
    expect(answered).toMatchObject({ status: 200, body: { delivered: true } });
    const [frame] = await inbox(bob);
    expect(frame).toMatchObject({ t: "answer", exchangeId: "ex-1", connectionId, status: "answered", text: "Yes, shipped Tuesday." });
    expect((await call("alice", "POST", `/bots/${alice.room}/answer`, { exchangeId: "ex-1", status: "declined" })).status).toBe(409);
    await call("bob", "POST", `/bots/${bob.room}/ack`, { states: [{ exchangeId: "ex-1", state: "answered" }] });
    expect(await inbox(bob)).toEqual([]);
    // Bodies leave once the host has them.
    expect(await roomSql(bob, (sql) => [...sql.exec("SELECT text FROM outbox")].map((r) => r.text))).toEqual([null]);
    expect(await roomSql(alice, (sql) => [...sql.exec("SELECT text FROM inbox")].map((r) => r.text))).toEqual([null]);

    // The other direction, declined.
    await ask(alice, connectionId, "Can I ship?", "ex-2");
    expect(await inbox(bob)).toMatchObject([{ t: "ask", exchangeId: "ex-2" }]);
    await call("bob", "POST", `/bots/${bob.room}/answer`, { exchangeId: "ex-2", status: "declined" });
    expect(await inbox(alice)).toMatchObject([{ t: "answer", exchangeId: "ex-2", status: "declined" }]);
  });

  it("refuses oversized questions and answers and unknown connections", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const connectionId = await connect(alice, bob);
    expect((await ask(bob, connectionId, "é".repeat(4_097))).status).toBe(413);
    expect((await ask(bob, "no-such-connection")).status).toBe(404);
    await ask(bob, connectionId, "q", "ex-big");
    const big = await call("alice", "POST", `/bots/${alice.room}/answer`, { exchangeId: "ex-big", status: "answered", text: "x".repeat(65 * 1024) });
    expect(big.status).toBe(413);
  });

  it("revokes on both sides and expires what was in flight", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const connectionId = await connect(alice, bob);
    await ask(bob, connectionId, "Still there?", "ex-r");
    expect((await call("bob", "DELETE", `/bots/${bob.room}/connections/${connectionId}`)).status).toBe(200);
    expect(await inbox(alice)).toEqual([{ t: "link", connectionId, op: "revoked" }]);
    expect(await inbox(bob)).toMatchObject([{ t: "answer", exchangeId: "ex-r", status: "expired" }]);
    expect((await ask(alice, connectionId)).status).toBe(404);
    expect((await ask(bob, connectionId)).status).toBe(404);
    expect((await call("bob", "DELETE", `/bots/${bob.room}/connections/${connectionId}`)).status).toBe(200);
  });

  it("limits questions per connection on both sides", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const connectionId = await connect(alice, bob);
    for (let i = 0; i < 3; i++) expect((await ask(bob, connectionId)).status).toBe(200);
    expect(await ask(bob, connectionId)).toMatchObject({ status: 429, body: { error: "too_many_in_flight" } });
    // Settle them, then fill the hour from history on the asking side.
    const now = Date.now();
    await roomSql(bob, (sql) => {
      sql.exec("UPDATE outbox SET state = 'answered', acked_state = 'answered'");
      for (let i = 0; i < 27; i++) {
        sql.exec("INSERT INTO outbox (exchange_id, connection_id, state, sent_at, acked_state) VALUES (?, ?, 'answered', ?, 'answered')", `old-${i}`, connectionId, now - 60_000);
      }
    });
    expect(await ask(bob, connectionId)).toMatchObject({ status: 429, body: { error: "rate_limited" } });
    // The answering side counts too: a clean asking side is still refused.
    await roomSql(bob, (sql) => sql.exec("DELETE FROM outbox"));
    await roomSql(alice, (sql) => {
      sql.exec("UPDATE inbox SET state = 'answered', acked = 1");
      for (let i = 0; i < 200; i++) {
        sql.exec("INSERT INTO inbox (exchange_id, connection_id, state, sent_at, acked) VALUES (?, ?, 'answered', ?, 1)", `day-${i}`, connectionId, now - 2 * 3_600_000);
      }
    });
    expect(await ask(bob, connectionId)).toMatchObject({ status: 429, body: { error: "rate_limited" } });
    expect(await roomSql(bob, (sql) => [...sql.exec("SELECT COUNT(*) AS n FROM outbox")][0]?.n)).toBe(0);
  });

  it("expires questions nobody settled within 7 days, on both sides", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const connectionId = await connect(alice, bob);
    await ask(bob, connectionId, "Anyone?", "ex-old");
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 7 * 24 * 3_600_000 + 1_000);
    try {
      expect(await inbox(alice)).toEqual([]);
      expect(await inbox(bob)).toMatchObject([{ t: "answer", exchangeId: "ex-old", status: "expired" }]);
      const late = await call("alice", "POST", `/bots/${alice.room}/answer`, { exchangeId: "ex-old", status: "declined" });
      expect([404, 409]).toContain(late.status);
    } finally {
      clock.mockRestore();
    }
  });
});

describe("BotRoom nudges", () => {
  it("nudges the host device's DeviceRoom when it queues an entry, as the owner", async () => {
    const aliceDevice = `dev-alice-${uid()}`;
    const bobDevice = `dev-bob-${uid()}`;
    await claimDevice(aliceDevice, "alice");
    await claimDevice(bobDevice, "bob");
    const alice = await publish("alice", "Auth bot", aliceDevice);
    const bob = await publish("bob", "Release bot", bobDevice);
    const connectionId = await connect(alice, bob);
    expect(await pendingBotNudges(aliceDevice)).toEqual([alice.room]);
    await ask(bob, connectionId, "Ping?", "ex-n");
    expect(await pendingBotNudges(aliceDevice)).toEqual([alice.room]);
    expect(await pendingBotNudges(bobDevice)).toEqual([]);
    await call("alice", "POST", `/bots/${alice.room}/answer`, { exchangeId: "ex-n", status: "answered", text: "Pong." });
    expect(await pendingBotNudges(bobDevice)).toEqual([bob.room]);

    // A bot published onto someone else's device can't nudge it.
    await runInDurableObject(env.DEVICE_ROOMS.get(env.DEVICE_ROOMS.idFromName(`d2/${aliceDevice}`)), async (_, state) => {
      state.storage.sql.exec("DELETE FROM pending_bot_nudges");
    });
    const mallory = await publish("mallory", "Noisy bot", aliceDevice);
    const dave = await publish("dave", "Dave bot");
    const malloryConnection = await connect(mallory, dave);
    await ask(dave, malloryConnection, "Wake up");
    expect(await inbox(mallory)).toMatchObject([{ t: "ask" }]);
    expect(await pendingBotNudges(aliceDevice)).toEqual([]);
  });

  it("carries owners' photos, and a changed profile reaches connected rooms", async () => {
    const alice = await publish("alice", "Auth bot");
    const bob = await publish("bob", "Release bot");
    const photo = "https://avatars.githubusercontent.com/u/1?v=4";
    const republish = (bot: Bot, ownerAvatarUrl: string) =>
      call(bot.user, "POST", `/bots/${bot.room}/publish`, {
        name: bot.user === "alice" ? "Auth bot" : "Release bot",
        ownerName: bot.user,
        ownerAvatarUrl,
        hostDeviceId: bot.device
      });
    expect((await republish(alice, photo)).status).toBe(200);
    // Off the avatar hosts: published without a photo, not refused.
    expect((await republish(bob, "https://evil.example/pixel.png")).status).toBe(200);
    const connectionId = await connect(alice, bob);
    const peers = await roomSql(bob, (sql) =>
      [...sql.exec("SELECT peer_avatar FROM connections WHERE id = ?", connectionId)].map((r) => r.peer_avatar)
    );
    expect(peers).toEqual([photo]);
    expect(
      await roomSql(alice, (sql) => [...sql.exec("SELECT peer_avatar FROM connections")].map((r) => r.peer_avatar))
    ).toEqual([null]);

    const newer = "https://lh3.googleusercontent.com/a/new";
    expect((await republish(alice, newer)).status).toBe(200);
    expect(await inbox(bob)).toEqual([
      {
        t: "link",
        connectionId,
        op: "added",
        peer: { botRoomId: alice.room, name: "Auth bot", ownerName: "alice", ownerAvatarUrl: newer }
      }
    ]);
    // Republishing the same profile announces nothing.
    await call("bob", "POST", `/bots/${bob.room}/ack`, { links: [connectionId] });
    expect((await republish(alice, newer)).status).toBe(200);
    expect(await inbox(bob)).toEqual([]);
  });
});

import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { verifyToken } from "../../src/auth/auth";
import type { BotFrame } from "../../src/bots/bot-frames";
import { BotRoom } from "../../src/bots/bot-room";
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
  expect(reply.body.push).toBe(true);
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

// B1 (docs/design/do-cost.md): a nudge the host may not have received is
// repeated from the alarm until the host acknowledges. These drive a BotRoom
// directly with fake DeviceRoom and BotRoom namespaces, so each nudge's
// answer can be chosen and the clock moved.
describe("BotRoom nudge repeats", () => {
  type Answer = "delivered" | "queued" | "throw" | "503" | "404";

  const harness = async (name: string, run: (h: Harness) => Promise<void>) => {
    let answer: Answer = "delivered";
    const nudges: string[] = [];
    const devices = {
      idFromName: (n: string) => n,
      get: (n: string) => ({
        fetch: async (request: Request) => {
          nudges.push(`${n} ${((await request.json()) as { botRoomId: string }).botRoomId}`);
          if (answer === "throw") throw new Error("device room unreachable");
          if (answer === "503") return new Response("down", { status: 503 });
          if (answer === "404") return Response.json({ error: "forbidden" }, { status: 404 });
          return Response.json(answer === "queued" ? { delivered: false, queued: true } : { delivered: true });
        }
      })
    };
    const peers = { idFromName: (n: string) => n, get: () => ({ fetch: async () => Response.json({ ok: true }) }) };
    const fakeEnv = { DEVICE_ROOMS: devices, BOT_ROOMS: peers } as unknown as Env;
    await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName(`renudge-${name}-${uid()}`)), async (_, state) => {
      const room = new BotRoom(state, fakeEnv);
      const send = async (path: string, body: unknown, user: string | null = "alice", method = "POST") => {
        const response = await room.fetch(
          new Request(`https://bot${path}${user ? "?room=r1" : ""}`, {
            method,
            headers: { ...(user ? { [AUTH_USER_HEADER]: user } : {}), "content-type": "application/json" },
            ...(method === "GET" ? {} : { body: JSON.stringify(body) })
          })
        );
        return { status: response.status, body: (await response.json()) as Record<string, unknown> };
      };
      expect((await send("/publish", { name: "Auth bot", ownerName: "alice", hostDeviceId: "dev-a" })).status).toBe(200);
      state.storage.sql.exec(
        "INSERT INTO connections (id, peer_room, peer_name, peer_owner_name, created_at) VALUES ('c1', 'r2', 'Peer', 'bob', ?)",
        Date.now()
      );
      const meta = (key: string) =>
        [...state.storage.sql.exec("SELECT value FROM meta WHERE key = ?", key)][0]?.value as string | undefined;
      await run({
        room,
        state,
        nudges,
        answer: (next) => {
          answer = next;
        },
        send,
        peerAsk: (exchangeId) => send("/peer/ask", { connectionId: "c1", room: "r2", exchangeId, text: "Ready?" }, null),
        renudgeAt: () => (meta("renudge_at") === undefined ? undefined : Number(meta("renudge_at"))),
        at: async (time, body) => {
          const clock = vi.spyOn(Date, "now").mockReturnValue(time);
          try {
            await body();
          } finally {
            clock.mockRestore();
          }
        }
      });
    });
  };

  interface Harness {
    room: BotRoom;
    state: DurableObjectState;
    nudges: string[];
    answer: (next: Answer) => void;
    send: (path: string, body: unknown, user?: string | null, method?: string) => Promise<Reply>;
    peerAsk: (exchangeId: string) => Promise<Reply>;
    renudgeAt: () => number | undefined;
    at: (time: number, body: () => Promise<void>) => Promise<void>;
  }

  const DAY = 24 * 3_600_000;

  it("repeats a delivered nudge after 15 s while unacknowledged, and the ack stops it", async () => {
    await harness("delivered", async (h) => {
      const before = Date.now();
      expect((await h.peerAsk("ex-1")).status).toBe(200);
      expect(h.nudges).toEqual(["d2/dev-a r1"]);
      const first = h.renudgeAt()!;
      expect(first - before).toBeGreaterThanOrEqual(15_000);
      expect(first - before).toBeLessThan(16_000);
      expect(await h.state.storage.getAlarm()).toBe(first);

      // Still unacknowledged at 15 s: the alarm nudges again, next in 30 s.
      await h.at(first, () => h.room.alarm());
      expect(h.nudges).toHaveLength(2);
      expect(h.renudgeAt()! - first).toBe(30_000);
      expect(await h.state.storage.getAlarm()).toBe(h.renudgeAt());

      // The host pulls and acknowledges: no repeat, and only the expiry alarm.
      expect((await h.send("/inbox", undefined, "alice", "GET")).body).toMatchObject({ push: true, entries: [{ t: "ask" }] });
      await h.send("/ack", { asks: ["ex-1"] });
      expect(h.renudgeAt()).toBeUndefined();
      expect((await h.state.storage.getAlarm())! - Date.now()).toBeGreaterThan(6 * DAY);
      await h.at(first + 30_000, () => h.room.alarm());
      expect(h.nudges).toHaveLength(2);
    });
  });

  it("stops when the inbox is empty at the repeat", async () => {
    await harness("drained", async (h) => {
      await h.peerAsk("ex-1");
      const first = h.renudgeAt()!;
      // Acked by some other route (an old repeat timer left behind).
      h.state.storage.sql.exec("UPDATE inbox SET acked = 1, text = NULL");
      await h.at(first, () => h.room.alarm());
      expect(h.nudges).toHaveLength(1);
      expect(h.renudgeAt()).toBeUndefined();
    });
  });

  it("leaves a queued nudge to the DeviceRoom's replay", async () => {
    await harness("queued", async (h) => {
      h.answer("queued");
      await h.peerAsk("ex-1");
      expect(h.nudges).toHaveLength(1);
      expect(h.renudgeAt()).toBeUndefined();
      expect((await h.state.storage.getAlarm())! - Date.now()).toBeGreaterThan(6 * DAY);
      await h.at(Date.now() + 15_000, () => h.room.alarm());
      expect(h.nudges).toHaveLength(1);
    });
  });

  it("repeats a failed nudge call, and a refused one not at all", async () => {
    await harness("failed", async (h) => {
      h.answer("throw");
      await h.peerAsk("ex-1");
      const first = h.renudgeAt()!;
      expect(first).toBeDefined();
      h.answer("503");
      await h.at(first, () => h.room.alarm());
      expect(h.nudges).toHaveLength(2);
      const second = h.renudgeAt()!;
      expect(second - first).toBe(30_000);
      // The repeat finds the host offline: queued for its join, ladder ends.
      h.answer("queued");
      await h.at(second, () => h.room.alarm());
      expect(h.nudges).toHaveLength(3);
      expect(h.renudgeAt()).toBeUndefined();

      // No DeviceRoom owner (the device never joined): nothing to repeat.
      h.answer("404");
      await h.peerAsk("ex-2");
      expect(h.renudgeAt()).toBeUndefined();
    });
  });

  it("walks 15 s, 30 s, 60 s, ten more minutes, then 15 min, and gives up after about a day", async () => {
    await harness("ladder", async (h) => {
      await h.peerAsk("ex-1");
      let at = h.renudgeAt()!;
      const waits: number[] = [];
      for (let i = 0; i < 200 && at !== undefined; i++) {
        await h.at(at, () => h.room.alarm());
        const next = h.renudgeAt();
        if (next !== undefined) waits.push(next - at);
        at = next!;
      }
      const minute = 60_000;
      // The first repeat came 15 s after the nudge; then 30 s and 11 × 60 s.
      expect(waits.slice(0, 12)).toEqual([30_000, ...Array<number>(11).fill(minute)]);
      expect(new Set(waits.slice(12))).toEqual(new Set([15 * minute]));
      // The first nudge and 109 repeats, the last of which ends the ladder.
      expect(h.nudges).toHaveLength(1 + 13 + 96);
      expect(h.renudgeAt()).toBeUndefined();
    });
  });

  it("nudges the asking host on progress states, once per change", async () => {
    await harness("state", async (h) => {
      h.state.storage.sql.exec(
        "INSERT INTO outbox (exchange_id, connection_id, state, sent_at) VALUES ('ex-out', 'c1', 'queued', ?)",
        Date.now()
      );
      const state = (value: string) =>
        h.send("/peer/state", { connectionId: "c1", room: "r2", exchangeId: "ex-out", state: value }, null);
      await state("delivered");
      expect(h.nudges).toEqual(["d2/dev-a r1"]);
      await state("delivered");
      expect(h.nudges).toHaveLength(1);
      await state("working");
      expect(h.nudges).toHaveLength(2);
      expect((await h.send("/inbox", undefined, "alice", "GET")).body.entries).toEqual([
        { t: "state", exchangeId: "ex-out", state: "working" }
      ]);
      await h.send("/ack", { states: [{ exchangeId: "ex-out", state: "working" }] });
      expect(h.renudgeAt()).toBeUndefined();
    });
  });
});

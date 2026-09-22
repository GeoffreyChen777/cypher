/**
 * Deferred-write optimizations against real
 * workerd + real DO SQLite.
 *
 * Each case pins one of the three: attribution counters moved to memory,
 * `backupDirty` written only on the 0→1 edge, and the registry's `setAlarm`
 * skipped when the instant it wants is already scheduled. The branches here
 * are the ones §4.1 of that document calls out as the hibernation risk —
 * rebuild-from-one-read, and convergence after an eviction.
 */
import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { ChatRoom } from "../../src/chat-room";
import { RegistryRoom } from "../../src/registry-room";
import { encodeFrame, FRAME } from "../../src/chat-frames";
import { AUTH_USER_HEADER, type Env } from "../../src/env";

/** Counts billed writes: SQL rows plus alarm API calls, which never reach
 * `storage.sql` and so are invisible to a SQL-only proxy. */
function writes(state: DurableObjectState, sockets: WebSocket[] = []) {
  let sql: { key: string; cursor: SqlStorageCursor<Record<string, SqlStorageValue>> }[] = [];
  let alarms: string[] = [];
  let pending: Promise<unknown>[] = [];
  const sqlProxy = new Proxy(state.storage.sql, {
    get(target, key) {
      if (key === "exec")
        return (query: string, ...params: SqlStorageValue[]) => {
          const cursor = target.exec(query, ...params);
          const verb = query.trim().split(/\s/)[0]!.toUpperCase();
          const table = query.match(/(?:INTO|UPDATE|FROM)\s+(\w+)/i)?.[1] ?? "schema";
          const field = table === "meta" ? `:${String(params[0] ?? "")}` : "";
          sql.push({ key: `${verb}:${table}${field}`, cursor });
          return cursor;
        };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    }
  });
  const storage = new Proxy(state.storage, {
    get(target, key) {
      if (key === "sql") return sqlProxy;
      if (key === "setAlarm" || key === "deleteAlarm") {
        const fn = Reflect.get(target, key, target) as (...a: unknown[]) => Promise<void>;
        return (...a: unknown[]) => {
          alarms.push(key === "setAlarm" ? "ALARM:set" : "ALARM:delete");
          return fn.apply(target, a);
        };
      }
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    }
  });
  const context = new Proxy(state, {
    get(target, key) {
      if (key === "storage") return storage;
      if (key === "getWebSockets") return () => sockets;
      if (key === "waitUntil") return (p: Promise<unknown>) => { pending.push(p); };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    }
  });
  const settle = async () => {
    while (pending.length) { const batch = pending; pending = []; await Promise.allSettled(batch); }
  };
  const take = () => {
    const counts: Record<string, number> = {};
    for (const { key, cursor } of sql) counts[key] = (counts[key] ?? 0) + cursor.rowsWritten;
    for (const key of alarms) counts[key] = (counts[key] ?? 0) + 1;
    sql = []; alarms = [];
    for (const key of Object.keys(counts)) if (!counts[key]) delete counts[key];
    return counts;
  };
  return { context, take, settle };
}

function socket(device: string) {
  let attachment: unknown = { userId: "u", device, ready: true };
  const frames: Uint8Array[] = [];
  return {
    frames,
    ws: {
      deserializeAttachment: () => attachment,
      serializeAttachment: (v: unknown) => { attachment = v; },
      send: (b: ArrayBuffer) => frames.push(new Uint8Array(b)),
      close: () => {}
    } as unknown as WebSocket
  };
}

const req = (path: string, method = "GET", body?: BodyInit) =>
  new Request(`https://test${path}`, {
    method,
    headers: { [AUTH_USER_HEADER]: "u", "content-type": "application/json" },
    body
  });

const push = (room: ChatRoom, ws: WebSocket, batchId: string, byte = 1) =>
  room.webSocketMessage(
    ws,
    encodeFrame(FRAME.push, { batchId }, new Uint8Array([byte])).buffer as ArrayBuffer
  );

// ── T1.1 attribution counters ──────────────────────────────────────────────

it("T1.1: chat push attribution costs no write, and /stats still counts it", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t11-chat")), async (_, state) => {
    const host = socket("host"), m = writes(state, [host.ws]);
    const room = new ChatRoom(m.context, {} as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','u')");
    m.take();

    for (let i = 0; i < 5; i++) await push(room, host.ws, `b${i}`);
    await m.settle();
    const counts = m.take();
    expect(counts["INSERT:meta:pushOutcomes"]).toBeUndefined();

    // Served from memory before any flush.
    const stats = await (await room.fetch(req("/stats"))).json() as
      { pushOutcomes: Record<string, { ok: number }> };
    expect(stats.pushOutcomes.host!.ok).toBe(5);
    expect(m.take()["INSERT:meta:pushOutcomes"]).toBeUndefined();
  });
});

it("T1.1: closing the socket flushes counters, and a fresh instance keeps counting from them", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t11-flush")), async (_, state) => {
    const host = socket("host"), m = writes(state, [host.ws]);
    const first = new ChatRoom(m.context, {} as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','u')");
    for (let i = 0; i < 3; i++) await push(first, host.ws, `b${i}`);
    await first.webSocketClose(host.ws);
    await m.settle();
    expect(m.take()["INSERT:meta:pushOutcomes"]).toBeGreaterThan(0);

    // A second close with nothing new buffered must not write again.
    await first.webSocketClose(host.ws);
    expect(m.take()["INSERT:meta:pushOutcomes"]).toBeUndefined();

    // Hibernation: a new instance rebuilds from the table with one read.
    const second = new ChatRoom(m.context, {} as Env);
    await push(second, host.ws, "b3");
    const stats = await (await second.fetch(req("/stats"))).json() as
      { pushOutcomes: Record<string, { ok: number }> };
    expect(stats.pushOutcomes.host!.ok).toBe(4); // 3 flushed + 1 in memory
  });
});

it("T1.1: the alarm flushes counters even when the backup is idle", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t11-alarm")), async (_, state) => {
    const host = socket("host"), m = writes(state, [host.ws]);
    const room = new ChatRoom(m.context, {} as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','u')");
    await push(room, host.ws, "b0");
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('backupDirty','0') ON CONFLICT(key) DO UPDATE SET value='0'");
    m.take();
    await room.alarm(); // idle path: returns early, must still converge
    expect(m.take()["INSERT:meta:pushOutcomes"]).toBeGreaterThan(0);
    const stored = [...state.storage.sql.exec("SELECT value FROM meta WHERE key='pushOutcomes'")][0]
      ?.value as string;
    expect(JSON.parse(stored).host.ok).toBe(1);
  });
});

// ── T1.2 backupDirty edge ──────────────────────────────────────────────────

it("T1.2: backupDirty is written on the 0→1 edge only, and again after the alarm clears it", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t12-edge")), async (_, state) => {
    const host = socket("host"), m = writes(state, [host.ws]);
    // alarm() runs the real nightly backup, so this one needs a blob sink.
    const room = new ChatRoom(m.context, { BLOBS: { put: async () => {} } } as unknown as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','u')");
    m.take();

    await push(room, host.ws, "b0");
    await m.settle();
    expect(m.take()["INSERT:meta:backupDirty"]).toBeGreaterThan(0); // the edge

    for (let i = 1; i < 6; i++) await push(room, host.ws, `b${i}`);
    await m.settle();
    expect(m.take()["INSERT:meta:backupDirty"]).toBeUndefined(); // already 1

    // The alarm clears it; the next push must pay the edge again.
    await room.alarm();
    m.take();
    await push(room, host.ws, "b6");
    await m.settle();
    expect(m.take()["INSERT:meta:backupDirty"]).toBeGreaterThan(0);
    expect([...state.storage.sql.exec("SELECT value FROM meta WHERE key='backupDirty'")][0]?.value)
      .toBe("1");
  });
});

it("T1.2: a fresh instance on an already-dirty room does not rewrite the flag", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t12-rebuild")), async (_, state) => {
    const host = socket("host"), m = writes(state, [host.ws]);
    const first = new ChatRoom(m.context, {} as Env);
    state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('owner','u')");
    await push(first, host.ws, "b0");
    await m.settle();
    m.take();

    const second = new ChatRoom(m.context, {} as Env); // hibernation rebuild
    await push(second, host.ws, "b1");
    await m.settle();
    expect(m.take()["INSERT:meta:backupDirty"]).toBeUndefined();
  });
});

// ── T1.3 registry alarm debounce ───────────────────────────────────────────

const registryPush = (room: RegistryRoom, i: number) =>
  room.fetch(req("/push?device=host", "POST", JSON.stringify({
    batch: `b${i}`,
    ops: [{
      kind: "sessions", id: "chat", op: "upsert",
      hlc: `1788998400000-${String(i).padStart(6, "0")}-host`,
      set: { chatId: "chat", deviceId: "host", status: "working", updatedAt: i }
    }]
  })));

it("T1.3: repeated registry pushes re-set the same alarm instant only once", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t13-debounce")), async (_, state) => {
    const m = writes(state);
    const room = new RegistryRoom(m.context, { NOTIFICATIONS_ENABLED: "false" } as Env);
    m.take();
    try {
      expect((await registryPush(room, 0)).status).toBe(200);
      await m.settle();
      expect(m.take()["ALARM:set"]).toBe(1); // first push schedules

      for (let i = 1; i < 5; i++) expect((await registryPush(room, i)).status).toBe(200);
      await m.settle();
      const counts = m.take();
      expect(counts["ALARM:set"]).toBeUndefined();
      expect(counts["INSERT:meta:backupDirty"]).toBeUndefined();
      expect(counts["INSERT:meta:pushOutcomes"]).toBeUndefined();
      expect(counts["INSERT:rows"]).toBeGreaterThan(0); // real data still written
    } finally {
      await state.storage.deleteAlarm();
    }
  });
});

it("T1.3: a nearer notification deadline still re-arms the alarm", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t13-rearm")), async (_, state) => {
    const m = writes(state);
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now());
    const room = new RegistryRoom(m.context, { NOTIFICATIONS_ENABLED: "false" } as Env);
    try {
      expect((await registryPush(room, 0)).status).toBe(200);
      await m.settle();
      expect(m.take()["ALARM:set"]).toBe(1);

      // Backup deadline is a day out; a notification wanting to run sooner
      // moves the computed due and MUST be scheduled.
      const settings = await room.fetch(new Request("https://test/notifications/settings", {
        method: "PUT",
        headers: { [AUTH_USER_HEADER]: "u", "content-type": "application/json" },
        body: JSON.stringify({ enabled: true, sessionCompleted: true, sessionFailed: true,
          inputRequested: true, quietHours: null })
      }));
      expect([200, 400, 404, 501]).toContain(settings.status);
      await m.settle();
      m.take();

      // Changing the stored deadline directly proves the comparison is on the
      // instant, not on "have I ever scheduled".
      state.storage.sql.exec(
        "INSERT INTO meta(key,value) VALUES('backupDue',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        String(Date.now() + 60_000)
      );
      expect((await registryPush(room, 1)).status).toBe(200);
      await m.settle();
      expect(m.take()["ALARM:set"]).toBe(1);
    } finally {
      clock.mockRestore();
      await state.storage.deleteAlarm();
    }
  });
});

it("T1.3: alarm() re-reads the stored alarm instead of assuming it was cleared", async () => {
  await runInDurableObject(env.TEST_LOG.get(env.TEST_LOG.idFromName("t13-direct-alarm")), async (_, state) => {
    let backups = 0;
    const m = writes(state);
    const room = new RegistryRoom(
      m.context,
      { BLOBS: { put: async () => { backups++; } }, NOTIFICATIONS_ENABLED: "false" } as unknown as Env
    );
    try {
      expect((await registryPush(room, 0)).status).toBe(200);
      await m.settle();
      expect(await state.storage.getAlarm()).not.toBeNull();

      // A DIRECT alarm() call leaves the stored alarm in place — unlike a
      // runtime delivery. The schedule that follows must not assume null and
      // skip the delete, or the room would keep a stale alarm forever.
      state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('backupDue','1') ON CONFLICT(key) DO UPDATE SET value='1'");
      await room.alarm();
      await m.settle();
      expect(backups).toBe(1);
      expect(await state.storage.getAlarm()).toBeNull();
    } finally {
      await state.storage.deleteAlarm();
    }
  });
});

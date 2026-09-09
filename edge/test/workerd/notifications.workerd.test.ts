import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { Notifications } from "../../src/notifications";
import { PushDevice } from "../../src/push-device";
import { defaultNotificationSettings } from "../../src/notifications-model";
import type { Row } from "../../src/registry-core";
import { AUTH_USER_HEADER, type Env } from "../../src/env";
import { RegistryRoom } from "../../src/registry-room";

const row = (kind: string, id: string, fields: Row["fields"]): Row =>
  ({ kind, id, seq: 1, deleted: false, fields, clocks: {} });

describe("notification outbox on real Durable Object SQLite", () => {
  it("falls back from an expired desktop session target to iOS recipients", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-desktop-fallback"));
    await runInDurableObject(stub, async (_, state) => {
      const start = Date.now(), sends: unknown[] = [];
      const rows = new Map<string, Row>([
        ["chats/chat", row("chats", "chat", { deviceId: "host", spaceId: "project" })],
        ["spaces/project", row("spaces", "project", {})]
      ]);
      const ns = { idFromString: (id: string) => id, get: () => ({ fetch: async (r: Request) => {
        const body = await r.json() as { message: { kind: string } };
        if (body.message.kind !== "badge") sends.push(body);
        return Response.json({ sent: true });
      } }) };
      const config = { NOTIFICATIONS_ENABLED: "true", PUSH_DEVICES: ns,
        APNS_TEAM_ID: "TEAM123456", APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test" } as unknown as Env;
      const service = new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
        JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
        JSON.stringify({ clientId: "desktop", platform: "desktop", at: start }));
      const running = row("sessions", "chat", { chatId: "chat", deviceId: "host", status: "working",
        startedAt: start - 60_000, updatedAt: start });
      const done = row("sessions", "chat", { ...running.fields, status: "idle" });
      rows.set("sessions/chat", done);
      service.observe([{ before: running, after: done }], "host");
      const clock = vi.spyOn(Date, "now").mockReturnValue(start + 11_000);
      try {
        await service.flush(); expect(sends).toHaveLength(0);
        clock.mockReturnValue(start + 60_000);
        await service.flush(); expect(sends).toHaveLength(1);
      } finally { clock.mockRestore(); }
    });
  });
  it.each(["done", "error"] as const)("aggregates async children (%s) without announcing the parent's launch acknowledgement", async status => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName(`notifications-async-${status}`));
    await runInDurableObject(stub, async (_, state) => {
      const start = Date.now(), sent: { message: { kind: string } }[] = [];
      const rows = new Map<string, Row>([
        ["chats/chat", row("chats", "chat", { deviceId: "host", spaceId: "project" })],
        ["spaces/project", row("spaces", "project", {})]
      ]);
      const config = { NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456",
        APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test", PUSH_DEVICES: {
          idFromString: (id: string) => id, get: () => ({ fetch: async (r: Request) => {
            const body = await r.json() as { message: { kind: string } };
            if (body.message.kind !== "badge") sent.push(body);
            return Response.json({ sent: true });
          } })
        } } as unknown as Env;
      const service = new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
        JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
        JSON.stringify({ clientId: "phone", platform: "ios", at: start }));
      const running = row("sessions", "chat", { chatId: "chat", deviceId: "host", status: "working",
        startedAt: start - 60_000, updatedAt: start, subagents: [{ mode: "async", status: "running", updatedAt: start }] });
      const launched = row("sessions", "chat", { ...running.fields, status: "idle" });
      service.observe([{ before: undefined, after: running }], "host");
      service.observe([{ before: running, after: launched }], "host");
      expect(service.nextDue()).toBeUndefined();
      const finishedAt = start + 3 * 3_600_000;
      const finished = row("sessions", "chat", { ...launched.fields,
        subagents: [{ mode: "async", status, updatedAt: finishedAt }] });
      const clock = vi.spyOn(Date, "now").mockReturnValue(finishedAt);
      try {
        rows.set("sessions/chat", finished);
        service.observe([{ before: launched, after: finished }], "host");
        await service.flush(); expect(sent).toHaveLength(0);
        clock.mockReturnValue(finishedAt + 11_000);
        await service.flush();
        expect(sent).toHaveLength(1);
        expect(sent[0].message.kind).toBe(status === "done" ? "completed" : "failed");
      } finally { clock.mockRestore(); }
    });
  });
  it("notification requests cannot keep postponing a legacy dirty registry backup", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-backup-migration"));
    await runInDurableObject(stub, async (_, state) => {
      let backups = 0;
      const room = new RegistryRoom(state, { BLOBS: { put: async () => { backups++; } } } as unknown as Env);
      state.storage.sql.exec("INSERT INTO meta(key,value) VALUES('backupDirty','1'),('seq','1')");
      const request = () => room.fetch(new Request("https://registry/notifications/settings",
        { method: "PUT", headers: { [AUTH_USER_HEADER]: "user" }, body: JSON.stringify(defaultNotificationSettings()) }));
      const settled = () => (room as unknown as { alarmScheduling: Promise<void> }).alarmScheduling;
      const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now());
      try {
        expect((await request()).status).toBe(200); await settled();
        const deadline = [...state.storage.sql.exec("SELECT value FROM meta WHERE key='backupDue'")][0]?.value;
        expect(Number(deadline)).toBeGreaterThan(0);
        clock.mockReturnValue(Date.now() + 5000);
        await request(); await settled();
        expect([...state.storage.sql.exec("SELECT value FROM meta WHERE key='backupDue'")][0]?.value).toBe(deadline);
        await room.alarm();
        expect(backups).toBe(1);
        expect(await state.storage.getAlarm()).toBeNull();
      } finally { clock.mockRestore(); await state.storage.deleteAlarm(); }
    });
  });
  it("persists the delay across reconstruction without backfilling an already sent event", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-suppression"));
    await runInDurableObject(stub, async (_, state) => {
      const start = Date.now(), sends: unknown[] = [];
      const rows = new Map<string, Row>();
      rows.set("chats/chat", row("chats", "chat", { deviceId: "host", spaceId: "project" }));
      rows.set("spaces/project", row("spaces", "project", { deviceId: "host" }));
      const ns = { idFromString: (id: string) => id, get: () => ({ fetch: async (request: Request) => {
        const body = await request.json() as { message: { kind: string } };
        if (body.message.kind !== "badge") sends.push(body);
        return Response.json({ sent: true });
      } }) };
      const config = { NOTIFICATIONS_ENABLED: "true", PUSH_DEVICES: ns,
        APNS_TEAM_ID: "TEAM123456", APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test-only" } as unknown as Env;
      const make = () => new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
      let service = make();
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?)", "recipients",
        JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
        JSON.stringify({ clientId: "phone", platform: "ios", at: start }));
      const running = row("sessions", "chat", { chatId: "chat", deviceId: "host", status: "working", startedAt: start - 60_000, updatedAt: start });
      service.observe([{ before: undefined, after: running }], "host");
      const done = row("sessions", "chat", { ...running.fields, status: "idle" });
      rows.set("sessions/chat", done);
      service.observe([{ before: running, after: done }], "host");
      expect(service.nextDue()).toBeGreaterThan(start);
      service = make(); // same SQL storage, new instance (hibernation/restart)
      await service.flush();
      expect(sends).toHaveLength(0);
      await service.fetch(new Request("https://test", { method: "POST", body: JSON.stringify({
        clientId: "desktop", sequence: 1, platform: "desktop", foreground: true, interactionAgeMs: 0, chatId: null
      }) }), "activity");
      const clock = vi.spyOn(Date, "now").mockReturnValue(start + 11_000);
      try {
        await service.flush();
        expect(sends).toHaveLength(1);
        expect(service.nextDue()).toBeUndefined();
        clock.mockReturnValue(start + 300_000);
        await service.flush();
        expect(sends).toHaveLength(1);
      } finally { clock.mockRestore(); }
    });
  });

  it("delivers the selected session target once and cancels resolved questions", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-input"));
    await runInDurableObject(stub, async (_, state) => {
      const start = Date.now(), sends: unknown[] = [];
      const rows = new Map<string, Row>([
        ["chats/chat", row("chats", "chat", { deviceId: "host", spaceId: "project" })],
        ["spaces/project", row("spaces", "project", {})]
      ]);
      const ns = { idFromString: (id: string) => id, get: () => ({ fetch: async (request: Request) => {
        const body = await request.json() as { message: { kind: string } };
        if (body.message.kind !== "badge") sends.push(body);
        return Response.json({ sent: true });
      } }) };
      const config = { NOTIFICATIONS_ENABLED: "true", PUSH_DEVICES: ns,
        APNS_TEAM_ID: "TEAM123456", APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test-only" } as unknown as Env;
      const service = new Notifications(state, config, (kind, id) => rows.get(`${kind}/${id}`), () => {});
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?)", "recipients",
        JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
        JSON.stringify({ clientId: "phone", platform: "ios", at: start }));
      const running = row("sessions", "chat", { chatId: "chat", deviceId: "host", status: "working", startedAt: start - 60_000, updatedAt: start });
      const input = row("sessions", "chat", { ...running.fields, status: "awaitingInput" });
      service.observe([{ before: undefined, after: running }], "host");
      rows.set("sessions/chat", input);
      service.observe([{ before: running, after: input }], "host");
      await service.fetch(new Request("https://test", { method: "POST", body: JSON.stringify({
        clientId: "desktop", sequence: 1, platform: "desktop", foreground: true, interactionAgeMs: 0, chatId: null
      }) }), "activity");
      const clock = vi.spyOn(Date, "now").mockReturnValue(start + 11_000);
      try {
        await service.flush(); expect(sends).toHaveLength(1);
        expect(service.nextDue()).toBeUndefined();
        clock.mockReturnValue(start + 50_000);
        await service.flush(); expect(sends).toHaveLength(1);
        await service.flush(); expect(sends).toHaveLength(1);
        service.observe([{ before: running, after: input }], "host");
        rows.set("sessions/chat", running);
        clock.mockReturnValue(start + 65_000);
        await service.flush();
        expect(sends).toHaveLength(1);
      } finally { clock.mockRestore(); }
    });
  });

  it("ignores baseline snapshots, non-host replication and short runs", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-baseline"));
    await runInDurableObject(stub, async (_, state) => {
      const now = Date.now();
      const rows = new Map<string, Row>([
        ["chats/chat", row("chats", "chat", { deviceId: "host", spaceId: "project" })],
        ["spaces/project", row("spaces", "project", {})]
      ]);
      const service = new Notifications(state, { NOTIFICATIONS_ENABLED: "true", PUSH_DEVICES: {},
        APNS_TEAM_ID: "TEAM123456", APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test" } as Env,
        (kind, id) => rows.get(`${kind}/${id}`), () => {});
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
        JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
      state.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
        JSON.stringify({ clientId: "phone", platform: "ios", at: now }));
      const a = row("sessions", "chat", { deviceId: "host", chatId: "chat", status: "working", startedAt: now - 1000, updatedAt: now });
      const b = row("sessions", "chat", { ...a.fields, status: "idle" });
      service.observe([{ before: undefined, after: b }], "host");
      service.observe([{ before: a, after: b }], "phone");
      service.observe([{ before: a, after: b }], "host");
      expect(service.nextDue()).toBeUndefined();
      const bad = await service.fetch(new Request("https://test", {
        method: "PUT", body: JSON.stringify({ ...defaultNotificationSettings(), mutedProjects: ["../bad"] })
      }), "settings");
      expect(bad.status).toBe(400);
      const event = await service.fetch(new Request("https://test", { method: "POST", body: JSON.stringify({
        chatId: "chat", deviceId: "host", status: "working", updatedAt: now
      }) }), "event");
      expect(event.status).toBe(200);
      const done = await service.fetch(new Request("https://test", { method: "POST", body: JSON.stringify({
        chatId: "chat", deviceId: "host", status: "idle", startedAt: now - 60_000, updatedAt: now + 1
      }) }), "event");
      expect(done.status).toBe(200);
      expect(service.nextDue()).toBeDefined();
    });
  });
});

describe("global APNs token ownership", () => {
  it("does not serialize the external send behind the DO input gate", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-send-gate"));
    await runInDurableObject(stub, async (_, state) => {
      const device = new PushDevice(state, { NOTIFICATIONS_ENABLED: "false" } as Env);
      const request = new Request("https://test/send", { method: "POST",
        body: JSON.stringify({ scope: "a".repeat(64), lease: "bad", message: {
          id: crypto.randomUUID(), scope: "a".repeat(64), chatId: "chat", projectId: "project",
          kind: "completed", expires: Date.now() + 60_000
        } }) });
      // The send route must be entered directly; a malformed/stale request
      // returns immediately rather than waiting behind a DO gate.
      const response = await device.fetch(request);
      expect(response.status).toBe(200);
    });
  });
  it("rejects stale registration and old-account logout without touching the current owner", async () => {
    const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName("notifications-token-owner"));
    await runInDurableObject(stub, async (_, state) => {
      const device = new PushDevice(state, {} as Env);
      const scopeA = "a".repeat(64), scopeB = "b".repeat(64);
      const call = (path: string, body: object) => device.fetch(new Request(`https://test${path}`, {
        method: "POST", body: JSON.stringify(body)
      }));
      const registration = { installationId: "phone", token: "0".repeat(64), environment: "production" };
      const first = await (await call("/register", { ...registration, scope: scopeA, epoch: 1 })).json() as { lease: string };
      const again = await (await call("/register", { ...registration, scope: scopeA, epoch: 1 })).json() as { lease: string };
      expect(again.lease).toBe(first.lease);
      const second = await (await call("/register", { ...registration, scope: scopeB, epoch: 3 })).json() as { lease: string };
      expect(first.lease).not.toBe(second.lease);
      expect((await call("/register", { ...registration, scope: scopeA, epoch: 1 })).status).toBe(409);
      await call("/unregister", { scope: scopeA, lease: first.lease, epoch: 2 });
      expect((await state.storage.get<{ scope: string; active: boolean }>("registration"))?.scope).toBe(scopeB);
      expect((await state.storage.get<{ active: boolean }>("registration"))?.active).toBe(true);
      await call("/unregister", { scope: scopeB, lease: second.lease, epoch: 4 });
      expect((await state.storage.get<{ token: string }>("registration"))?.token).toBe("");
      expect((await call("/register", { ...registration, scope: scopeB, epoch: 3 })).status).toBe(409);
      expect((await call("/register", { ...registration, scope: scopeB, epoch: 5 })).status).toBe(200);
      for (let index = 0; index < 66; index++) {
        expect((await call("/register", { ...registration, installationId: `fresh-${index}`,
          scope: scopeB, epoch: 1 })).status).toBe(200);
      }
      expect((await call("/register", { ...registration, scope: scopeA, epoch: 1000 })).status).toBe(409);
    });
  });
});

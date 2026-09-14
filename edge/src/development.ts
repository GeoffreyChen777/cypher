/** Development-only entrypoint. Never use this config for production. */
import application from "./index";
import { ChatRoom as BaseChatRoom } from "./chat-room";
import { DeviceRoom as BaseDeviceRoom } from "./device-room";
import { RegistryRoom as BaseRegistryRoom } from "./registry-room";
import { SessionRoom as BaseSessionRoom } from "./session-room";
import { PushDevice as BasePushDevice } from "./push-device";
import { authenticate } from "./auth";
import type { Env } from "./env";
import { admit, type Budget } from "./development-budget";

interface DevelopmentEnv extends Env { DEV_GUARD: DurableObjectNamespace }
const response = (value: unknown, status = 200) => Response.json(value, { status });
const gate = (env: DevelopmentEnv) => env.DEV_GUARD.get(env.DEV_GUARD.idFromName("budget"));
const maxBytes = 64 * 1024;

/** One global gate for this development Worker, not one budget per client. */
export class DevelopmentGuard implements DurableObject {
  constructor(private ctx: DurableObjectState) {
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS budget (id INTEGER PRIMARY KEY, value TEXT NOT NULL)");
  }
  async fetch(request: Request): Promise<Response> {
    const sql = this.ctx.storage.sql;
    const path = new URL(request.url).pathname;
    // Parse before reading state: DO events can interleave across awaits.
    const body = path === "/status" ? {} : await request.json() as { room?: string; rows?: number; lease?: string };
    const old = [...sql.exec("SELECT value FROM budget WHERE id=1")][0];
    const current: Budget | undefined = old ? JSON.parse(old.value as string) : undefined;
    if (path === "/status") return response({ budget: current ?? null });
    if (path === "/admit" && typeof body.room === "string") {
      const decision = admit(current, body.room, Date.now());
      if (!decision.allowed) return response({ error: decision.reason }, 429);
      sql.exec("INSERT INTO budget(id,value) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
        JSON.stringify(decision.budget));
      return response({ ok: true, lease: decision.lease });
    }
    if (path === "/settle" && current && typeof body.lease === "string" && Number.isSafeInteger(body.rows) && body.rows! >= 0) {
      // Include this accounting write. SQL rowsWritten includes index writes;
      // this is a development tripwire, not a Cloudflare invoice measurement.
      current.rows += body.rows! + 1;
      delete (current.leases ??= {})[body.lease];
      sql.exec("UPDATE budget SET value=? WHERE id=1", JSON.stringify(current));
      return response({ ok: true });
    }
    return response({ error: "invalid gate request" }, 400);
  }
}

type RoomConstructor = new (ctx: DurableObjectState, env: Env) => DurableObject;

function guarded(Base: RoomConstructor) {
  return class implements DurableObject {
    private inner?: DurableObject;
    private writes = 0;
    private reported = 0;
    constructor(private ctx: DurableObjectState, private env: DevelopmentEnv) {}

    private instance(): DurableObject {
      if (this.inner) return this.inner;
      const sql = this.ctx.storage.sql;
      // Forward native methods with their original receiver (brand checks).
      const trackedSql = new Proxy(sql, { get: (target, key) => {
        if (key === "exec") return (query: string, ...args: SqlStorageValue[]) => {
          const cursor = target.exec(query, ...args);
          this.writes += cursor.rowsWritten;
          return cursor;
        };
        const value = Reflect.get(target, key, target);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      const storage = new Proxy(this.ctx.storage, { get: (target, key) => {
        if (key === "sql") return trackedSql;
        const value = Reflect.get(target, key, target);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      const context = new Proxy(this.ctx, { get: (target, key) => {
        if (key === "storage") return storage;
        const value = Reflect.get(target, key, target);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      return this.inner = new Base(context, this.env);
    }

    private async run<T>(operation: (room: DurableObject) => Promise<T>, denied: () => T): Promise<T> {
      if (this.env.AUTH_MODE !== "dev-locked" || !this.env.DEV_ACCESS_TOKEN) return denied();
      const stub = gate(this.env);
      const permit = await stub.fetch("https://guard/admit", { method: "POST",
        body: JSON.stringify({ room: `${Base.name}:${this.ctx.id}` }) });
      if (!permit.ok) return denied();
      const { lease } = await permit.json() as { lease: string };
      try { return await operation(this.instance()); }
      finally {
        const rows = this.writes - this.reported;
        this.reported = this.writes;
        const settled = await stub.fetch("https://guard/settle", { method: "POST", body: JSON.stringify({ rows, lease }) });
        if (!settled.ok) {
          this.reported -= rows;
          throw new Error("Development write accounting failed");
        }
      }
    }
    async fetch(request: Request): Promise<Response> {
      return this.run(async room => room.fetch!(request), () => new Response("Development budget exhausted", {
        status: 429, headers: { "retry-after": "60" }
      }));
    }
    async webSocketMessage(ws: WebSocket, message: ArrayBuffer | string): Promise<void> {
      if ((typeof message === "string" ? new TextEncoder().encode(message).length : message.byteLength) > maxBytes) {
        ws.close(1009, "Development frame limit"); return;
      }
      await this.run(async room => { await room.webSocketMessage?.(ws, message); },
        () => { ws.close(1013, "Development budget exhausted"); });
    }
    async webSocketClose(ws: WebSocket, code: number, reason: string, clean: boolean): Promise<void> {
      await this.run(async room => { await room.webSocketClose?.(ws, code, reason, clean); }, () => {});
    }
    async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
      await this.run(async room => { await room.webSocketError?.(ws, error); }, () => {});
    }
    async alarm(): Promise<void> {
      await this.run(async room => { await room.alarm?.(); }, () => {});
    }
  };
}

export const ChatRoom = guarded(BaseChatRoom);
export const RegistryRoom = guarded(BaseRegistryRoom);
export const DeviceRoom = guarded(BaseDeviceRoom);
export const SessionRoom = guarded(BaseSessionRoom);
export const PushDevice = guarded(BasePushDevice);

export default {
  async fetch(request: Request, env: DevelopmentEnv): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/health") return response({ ok: true, environment: "development", auth: "dev-locked" });
    if (env.AUTH_MODE !== "dev-locked" || !await authenticate(env, request)) return response({ error: "unauthorized" }, 401);
    if (path === "/dev/budget" && request.method === "GET") return gate(env).fetch("https://guard/status");
    // No installer, release promotion, WorkOS account management or APNs in dev.
    if (/^\/(auth|notifications|releases)(\/|$)/.test(path) || path === "/install.sh") return response({ error: "disabled in development" }, 404);
    if (request.body) {
      const reader = request.body.getReader();
      const chunks: Uint8Array[] = []; let size = 0;
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > maxBytes) { await reader.cancel(); return response({ error: "development payload limit" }, 413); }
        chunks.push(value);
      }
      const body = new Uint8Array(size); let offset = 0;
      for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
      request = new Request(request, { body });
    }
    // R2 blob routes bypass room objects, so meter their admission here too.
    if (path.startsWith("/blob/")) {
      const permit = await gate(env).fetch("https://guard/admit", { method: "POST", body: JSON.stringify({ room: "r2-blobs" }) });
      if (!permit.ok) return response({ error: "development budget exhausted" }, 429);
      const { lease } = await permit.json() as { lease: string };
      try { return await application.fetch(request, env); }
      finally {
        const settled = await gate(env).fetch("https://guard/settle", { method: "POST", body: JSON.stringify({ rows: 0, lease }) });
        if (!settled.ok) throw new Error("Development R2 admission accounting failed");
      }
    }
    return application.fetch(request, env);
  }
} satisfies ExportedHandler<DevelopmentEnv>;

import { DurableObject } from "cloudflare:workers";
import { ChatRoom } from "../../src/chat-room";
import { createDevelopmentPreview } from "../../src/development-preview";
import type { Env } from "../../src/env";

/** Public fixture credentials, never used outside the local workerd pool. */
export const previewTestEnv = { AUTH_MODE: "dev-locked", DEV_ACCESS_TOKEN: "local-preview-viewer",
  DEV_PREVIEW_ENABLED: "true", DEV_PREVIEW_PUBLISH_TOKEN: "a".repeat(64) };

/** Real sockets and real SQL counters through the actual ChatRoom handlers. */
export class TestPreviewRoom extends DurableObject<unknown> {
  room: ChatRoom;
  writes = 0;
  sqlCalls = 0;
  private context: DurableObjectState;
  constructor(ctx: DurableObjectState, env: unknown) {
    super(ctx, env);
    const sql = new Proxy(ctx.storage.sql, { get: (target, key) => {
      if (key === "exec") return (query: string, ...args: SqlStorageValue[]) => {
        const cursor = target.exec(query, ...args);
        this.writes += cursor.rowsWritten; this.sqlCalls++;
        return cursor;
      };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    const storage = new Proxy(ctx.storage, { get(target, key) {
      if (key === "sql") return sql;
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    this.context = new Proxy(ctx, { get(target, key) {
      if (key === "storage") return storage;
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    } });
    this.room = this.make();
  }
  private make(): ChatRoom {
    return new ChatRoom(this.context, previewTestEnv as unknown as Env, createDevelopmentPreview(this.context, previewTestEnv));
  }
  /** Simulates loss of application memory with the same surviving native WSs.
   * This is not proof of a Cloudflare production eviction schedule. */
  cold(): void { this.room = this.make(); }
  fetch(request: Request): Promise<Response> { return this.room.fetch(request); }
  webSocketMessage(ws: WebSocket, data: ArrayBuffer | string): Promise<void> { return this.room.webSocketMessage(ws, data); }
  webSocketClose(ws: WebSocket): Promise<void> { return this.room.webSocketClose(ws); }
  webSocketError(ws: WebSocket): Promise<void> { return this.room.webSocketError(ws); }
}

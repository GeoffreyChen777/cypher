/**
 * Preserve deployed DO class identities without preserving their protocols.
 *
 * Cloudflare requires the old exports while their objects exist. Exporting
 * the old implementation would also preserve hibernating WebSocket writers
 * and alarms despite the outer Worker's 410 routes. These implementations
 * never import Loro, parse old payloads, read private data or acknowledge a
 * discarded write. Historical storage is left untouched.
 */
class RetiredRoom implements DurableObject {
  constructor(private readonly ctx: DurableObjectState) {
    ctx.setWebSocketAutoResponse(); // Retire saved ping/pong configuration too.
    this.closeSockets();
    ctx.blockConcurrencyWhile(async () => { await ctx.storage.deleteAlarm(); });
  }

  private close(socket: WebSocket): void {
    try { socket.close(1008, "v3_required"); } catch { /* Already closed. */ }
  }
  private closeSockets(): void {
    for (const socket of this.ctx.getWebSockets()) this.close(socket);
  }
  fetch(_request: Request): Response {
    this.closeSockets();
    return Response.json({ error: "v3_required" }, {
      status: 410, headers: { "cache-control": "no-store" },
    });
  }
  webSocketMessage(socket: WebSocket, _message: string | ArrayBuffer): void {
    this.close(socket);
  }
  webSocketClose(socket: WebSocket, _code: number, _reason: string, _wasClean: boolean): void {
    this.close(socket);
  }
  webSocketError(socket: WebSocket, _error: unknown): void {
    this.close(socket);
  }
  async alarm(): Promise<void> {
    this.closeSockets();
    await this.ctx.storage.deleteAlarm();
  }
}

export class SessionRoom extends RetiredRoom {}
export class ChatRoom extends RetiredRoom {}
export class RegistryRoom extends RetiredRoom {}
export class DeviceRoom extends RetiredRoom {}

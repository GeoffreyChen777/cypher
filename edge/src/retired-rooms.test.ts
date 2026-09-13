import { describe, it, expect, vi } from "vitest";
import { ChatRoom, SessionRoom, RegistryRoom, DeviceRoom } from "./retired-rooms";

describe.each([ChatRoom, SessionRoom, RegistryRoom, DeviceRoom])("retired %s", Room => {
  it("rejects HTTP and closes old sockets without parsing, sending ACKs or touching data", async () => {
    const send = vi.fn();
    const close = vi.fn();
    const socket = { send, close } as unknown as WebSocket;
    const deleteAlarm = vi.fn(async () => {});
    const setWebSocketAutoResponse = vi.fn();
    const startup: Promise<unknown>[] = [];
    const storage = new Proxy({ deleteAlarm }, {
      get(target, key) {
        if (key === "deleteAlarm") return target.deleteAlarm;
        throw Error(`retired protocol touched storage.${String(key)}`);
      },
    });
    const state = {
      storage, getWebSockets: () => [socket], setWebSocketAutoResponse,
      blockConcurrencyWhile: (action: () => Promise<unknown>) => { const task = action(); startup.push(task); return task; },
    } as unknown as DurableObjectState;
    const room = new Room(state);
    await Promise.all(startup);
    expect(setWebSocketAutoResponse).toHaveBeenCalledWith();
    expect(close).toHaveBeenCalledWith(1008, "v3_required");
    for (const method of ["GET", "POST", "PUT", "DELETE"]) {
      const request = new Request("https://retired/push", {
        method, headers: { Upgrade: "websocket" }, ...(method === "GET" ? {} : { body: "old private data" }),
      });
      const reply = room.fetch(request);
      expect(reply.status).toBe(410);
      expect(reply.headers.get("cache-control")).toBe("no-store");
      expect(await reply.json()).toEqual({ error: "v3_required" });
      expect(request.bodyUsed).toBe(false);
    }
    room.webSocketMessage(socket, '{"type":"push","ops":[');
    room.webSocketMessage(socket, new Uint8Array([0, 1, 255]).buffer);
    room.webSocketMessage(socket, "ping");
    room.webSocketClose(socket, 1000, "", true);
    room.webSocketError(socket, Error("old socket"));
    await room.alarm();
    expect(send).not.toHaveBeenCalled();
    expect(deleteAlarm).toHaveBeenCalledTimes(2);
  });
});

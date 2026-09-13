import { env, runInDurableObject } from "cloudflare:test";
import { it, expect } from "vitest";
import { ChatRoom, SessionRoom, RegistryRoom, DeviceRoom } from "../../src/retired-rooms";

it.each([ChatRoom, SessionRoom, RegistryRoom, DeviceRoom])("retired class %s leaves SQLite/KV intact and fences an already-accepted socket", async Room => {
  const stub = env.TEST_LOG.get(env.TEST_LOG.idFromName(crypto.randomUUID()));
  await runInDurableObject(stub, async (_instance, state) => {
    state.storage.sql.exec("CREATE TABLE historic (id INTEGER PRIMARY KEY, body TEXT)");
    state.storage.sql.exec("INSERT INTO historic VALUES(1,'original history')");
    await state.storage.put("original-snapshot", "unchanged");
    await state.storage.setAlarm(Date.now() + 600_000);
    state.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
    const pair = new WebSocketPair();
    state.acceptWebSocket(pair[1]);
    pair[0].accept();
    pair[1].serializeAttachment({ protocol: "old", authenticated: true, cursor: 42 });
    let acked = false;
    pair[0].addEventListener("message", () => { acked = true; });
    const closed = new Promise<number>(resolve => {
      pair[0].addEventListener("close", event => { pair[0].close(); resolve(event.code); }, { once: true });
    });
    // Simulate the first activation of the replacement class with retained
    // platform sockets/storage, rather than merely testing outer HTTP routing.
    const retired = new Room(state);
    expect(await closed).toBe(1008);
    retired.webSocketMessage(pair[1], '{"ops":[{"set":{"title":"must not commit"}}]}');
    await retired.alarm();
    expect(acked).toBe(false);
    expect(await state.storage.getAlarm()).toBeNull();
    expect(state.getWebSocketAutoResponse()).toBeNull();
    expect(await state.storage.get("original-snapshot")).toBe("unchanged");
    expect([...state.storage.sql.exec("SELECT * FROM historic")]).toEqual([{ id: 1, body: "original history" }]);
  });
});

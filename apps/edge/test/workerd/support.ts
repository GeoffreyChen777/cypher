import type { Env } from "../../src/env";
import type { Row } from "../../src/registry/registry-core";

export const row = (kind: string, id: string, fields: Row["fields"]): Row =>
  ({ kind, id, seq: 1, deleted: false, fields, clocks: {} });

/** notificationsAvailable() gates the whole notification path; it needs a
 * complete APNs config, not just the flag. Public test values, never used
 * outside workerd. */
export const APNS_TEST_ENV = {
  NOTIFICATIONS_ENABLED: "true", APNS_TEAM_ID: "TEAM123456",
  APNS_KEY_ID: "TESTKEY001", APNS_PRIVATE_KEY: "test"
};

/** APNs-enabled env whose PUSH_DEVICES stub answers every push with `send`. */
export const pushEnv = (send: (request: Request) => Promise<Response>): Env =>
  ({ ...APNS_TEST_ENV, PUSH_DEVICES: { idFromString: (id: string) => id, get: () => ({ fetch: send }) } }) as unknown as Env;

/** pushEnv that records every non-badge push body and reports it sent. */
export const recordingPushEnv = (sends: unknown[]): Env => pushEnv(async request => {
  const body = await request.json() as { message: { kind: string } };
  if (body.message.kind !== "badge") sends.push(body);
  return Response.json({ sent: true });
});

/** One iOS recipient plus `chat`'s session target (owned by `clientId`). */
export function seedRecipient(sql: SqlStorage, at: number, clientId = "phone", platform = "ios") {
  sql.exec("INSERT INTO notify_kv(key,value) VALUES('recipients',?)",
    JSON.stringify([{ id: "b".repeat(64), lease: crypto.randomUUID(), installationId: "phone", epoch: 1 }]));
  sql.exec("INSERT INTO notify_kv(key,value) VALUES('target:chat',?)",
    JSON.stringify({ clientId, platform, at }));
}

/** A socket the room treats as a joined peer; `frames` collects what it is
 * sent (text frames for the registry room). */
export function peer(device: string) {
  let attachment: unknown = { userId: "u", device, ready: true };
  /** Text frames only; binary sends (ChatRoom rows) are accepted and dropped. */
  const frames: string[] = [];
  return {
    frames,
    ws: {
      deserializeAttachment: () => attachment,
      serializeAttachment: (v: unknown) => { attachment = v; },
      send: (s: string | ArrayBuffer) => {
        if (typeof s === "string") frames.push(s);
      },
      close: () => {}
    } as unknown as WebSocket
  };
}

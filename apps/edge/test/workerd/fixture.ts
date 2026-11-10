import { DurableObject } from "cloudflare:workers";

export { BotRoom } from "../../src/bots/bot-room";
export { DeviceRoom } from "../../src/device/device-room";

/** Bare SQLite-backed DO; tests reach its real `ctx.storage.sql` via
 * `runInDurableObject` (the cloudflare-os TEST_OVERSEER pattern). */
export class TestLogRoom extends DurableObject {}

export { TrustRoom } from "../../src/trust/trust-room";

export default {
  fetch(): Response {
    return new Response("test fixture", { status: 404 });
  }
};

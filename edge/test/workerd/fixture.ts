import { DurableObject } from "cloudflare:workers";
export { TestPreviewRoom } from "./preview-fixture";

/** Bare SQLite-backed DO; tests reach its real `ctx.storage.sql` via
 * `runInDurableObject` (the cloudflare-os TEST_OVERSEER pattern). */
export class TestLogRoom extends DurableObject {}

export default {
  fetch(): Response {
    return new Response("test fixture", { status: 404 });
  }
};

import { WorkerEntrypoint } from "cloudflare:workers";
import type { Env } from "./env";
import { sendAPNs, type PushMessage } from "./apns";

/** Internal RPC entrypoint in this same Worker. No public HTTP send route. */
export class APNsSender extends WorkerEntrypoint<Env> {
  async send(token: string, environment: "development" | "production", message: PushMessage) {
    return sendAPNs(this.env, token, environment, message);
  }
}

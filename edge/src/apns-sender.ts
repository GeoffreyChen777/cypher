import { WorkerEntrypoint } from "cloudflare:workers";
import type { Env } from "./env";
import { sendAPNs, type PushMessage } from "./apns";

/** Internal RPC entrypoint in this same Worker. No public HTTP send route. */
export class APNsSender extends WorkerEntrypoint<Env> {
  async fetch(request: Request): Promise<Response> {
    if (new URL(request.url).pathname !== "/send" || request.method !== "POST") {
      return new Response("not found", { status: 404 });
    }
    try {
      const body = await request.json() as {
        token: string; environment: "development" | "production"; message: PushMessage
      };
      const result = await sendAPNs(this.env, body.token, body.environment, body.message);
      return Response.json({ result });
    } catch {
      return Response.json({ result: "retry" });
    }
  }
  async send(token: string, environment: "development" | "production", message: PushMessage) {
    return sendAPNs(this.env, token, environment, message);
  }
}

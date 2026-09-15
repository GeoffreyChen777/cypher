/** LOCAL TEST ONLY. No deployment config/account/routes. Real ChatRoom/SQLite
 * with 180ms delayed durable rows to force the native preview overlay path. */
import { ChatRoom as Base } from "../src/chat-room";
import { createDevelopmentPreview } from "../src/development-preview";
import { AUTH_USER_HEADER, type Env } from "../src/env";
const settings = { AUTH_MODE: "dev-locked", DEV_ACCESS_TOKEN: "local-preview-viewer", DEV_PREVIEW_ENABLED: "true", DEV_PREVIEW_PUBLISH_TOKEN: "a".repeat(64) };
export class ChatRoom extends Base {
  constructor(ctx: DurableObjectState, env: Env) { super(ctx, env, createDevelopmentPreview(ctx, settings)); }
}
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const u = new URL(request.url);
    if (!["127.0.0.1", "localhost"].includes(u.hostname)) return new Response("local fixture only", { status: 403 });
    if (u.pathname === "/health") return Response.json({ localPreviewFixture: true });
    if ((request.headers.get("authorization")?.replace(/^Bearer /i, "") ?? u.searchParams.get("token")) !== settings.DEV_ACCESS_TOKEN) return new Response("unauthorized", { status: 401 });
    const match = u.pathname.match(/^\/chat2\/([a-zA-Z0-9_-]{1,128})\/(ws|rows|checkpoint|tail|diff|stats)$/);
    if (!match) return new Response("local fixture supports chat2 only", { status: 404 });
    const headers = new Headers(request.headers); headers.set(AUTH_USER_HEADER, "dev-user");
    u.pathname = `/${match[2]}`; u.searchParams.set("chatId", match[1]!); u.searchParams.delete("token");
    const stub = env.CHAT_ROOMS.get(env.CHAT_ROOMS.idFromName(`chat2/${match[1]}`));
    const response = await stub.fetch(new Request(u, { method: request.method, headers, body: request.body }));
    if (!response.webSocket || request.headers.has("x-cypher-preview-publisher")) return response;
    const remote = response.webSocket; remote.binaryType = "arraybuffer"; remote.accept();
    const pair = new WebSocketPair(); pair[1].binaryType = "arraybuffer"; pair[1].accept();
    pair[1].addEventListener("message", e => remote.send(e.data));
    remote.addEventListener("message", e => {
      const first = typeof e.data === "string" ? -1 : new Uint8Array(e.data)[0];
      if (first === 4 || first === 5) setTimeout(() => { try { pair[1].send(e.data); } catch {} }, 180);
      else { try { pair[1].send(e.data); } catch {} }
    });
    remote.addEventListener("close", e => { console.log("fixture upstream close", e.code, e.reason); try { pair[1].close(1000); } catch {} });
    pair[1].addEventListener("close", e => { console.log("fixture viewer close", e.code, e.reason); try { remote.close(1000); } catch {} });
    return new Response(null, { status: 101, webSocket: pair[0] });
  }
};

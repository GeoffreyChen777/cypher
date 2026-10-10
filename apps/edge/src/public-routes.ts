/**
 * Unauthenticated public surface: the health probe, the `curl | sh` installer
 * and release artifacts. Served on the edge.letscypher.app custom domain and
 * the workers.dev host.
 */
import { json } from "./env";
import installSh from "./install.sh";
import type { RouteContext } from "./router";

const readable = (request: Request): boolean => request.method === "GET" || request.method === "HEAD";

export const handlePublicRoute = async ({
  request,
  env,
  url,
  parts
}: RouteContext): Promise<Response | undefined> => {
  if (url.pathname === "/health") {
    return json({ ok: true, auth: env.AUTH_MODE === "dev" ? "dev" : "workos" });
  }

  if (url.pathname === "/install.sh" && readable(request)) {
    return new Response(request.method === "HEAD" ? null : installSh, {
      headers: {
        "content-type": "application/x-sh",
        "cache-control": "public, max-age=0, must-revalidate"
      }
    });
  }

  if (parts[0] === "releases" && parts.length >= 2 && readable(request)) {
    const key = decodeURIComponent(url.pathname.slice("/releases/".length));
    if (key.length === 0 || key.includes("..")) return json({ error: "bad request" }, 400);
    const object = await env.RELEASES.get(key);
    if (!object) return json({ error: "not_found" }, 404);
    // latest.txt / manifest.json flip on release; artifacts are immutable by name.
    const mutable = key.endsWith(".txt") || key.endsWith(".json");
    const headers = new Headers({
      "content-type": key.endsWith(".txt")
        ? "text/plain; charset=utf-8"
        : key.endsWith(".json")
          ? "application/json"
          : "application/octet-stream",
      "content-length": String(object.size),
      "cache-control": mutable ? "public, max-age=60" : "public, max-age=86400, immutable",
      etag: object.httpEtag,
      // The landing page fetches release metadata cross-origin.
      "access-control-allow-origin": "*"
    });
    return new Response(request.method === "HEAD" ? null : object.body, { headers });
  }

  return undefined;
};

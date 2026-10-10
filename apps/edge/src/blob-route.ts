/**
 * R2 tool-output sidecar (docs/design/chat2-sync.md A2), read-only: older
 * chats reference full tool outputs and diffs stored under
 * `blob/{userId}/{chatId}/{partId}[.diff]`; clients no longer upload any.
 * The per-user prefix is the owner check.
 */
import { json } from "./env";
import { ID_RE, PART_RE } from "./identifiers";
import { safeDecode, type AuthedContext } from "./router";

export const handleBlobRoute = async ({ request, env, parts, auth }: AuthedContext): Promise<Response | undefined> => {
  const [prefix, chatId, partSegment] = parts;
  if (prefix !== "blob" || parts.length !== 3 || chatId === undefined || partSegment === undefined) return undefined;
  if (!ID_RE.test(chatId)) return undefined;
  // Percent-decode the part segment before validating: PART_RE allows `#`
  // (`m1#c1`-style harness ids), which HTTP clients cannot send raw
  // (fragment delimiter) — the host percent-encodes it. Decode-then-validate
  // keeps traversal shut: `%2F` decodes to `/`, fails PART_RE.
  const partId = safeDecode(partSegment);
  if (partId === undefined || !PART_RE.test(partId)) {
    return json({ error: "bad part id" }, 400);
  }
  if (request.method !== "GET" && request.method !== "HEAD") return undefined;
  const key = `blob/${auth.userId}/${chatId}/${partId}`;
  const object = request.method === "GET" ? await env.BLOBS.get(key) : await env.BLOBS.head(key);
  if (!object) return json({ error: "not_found" }, 404);
  const headers = new Headers();
  object.writeHttpMetadata(headers);
  headers.set("etag", object.httpEtag);
  // Re-resolved tool parts overwrite their key, so short-lived caching only.
  headers.set("cache-control", "private, max-age=300");
  const body = request.method === "GET" && "body" in object ? (object as R2ObjectBody).body : null;
  return new Response(body, { headers });
};

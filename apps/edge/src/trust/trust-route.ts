/**
 * `/trust/*` → the caller's own TrustRoom (`t1/{userId}`). The room is always
 * derived from the verified token, never from the URL.
 */
import { json } from "../env";
import { forward, type AuthedContext } from "../router";

export const handleTrustRoute = ({ request, env, parts, auth }: AuthedContext): Promise<Response> | Response | undefined => {
  if (parts[0] !== "trust") return undefined;
  if (!env.TRUST_ROOMS) return json({ error: "unavailable" }, 503);
  const rest = parts.slice(1);
  if (rest.length === 0) return json({ error: "not_found" }, 404);
  return forward(env.TRUST_ROOMS, `t1/${auth.userId}`, request, auth.userId, `/${rest.join("/")}`, "");
};

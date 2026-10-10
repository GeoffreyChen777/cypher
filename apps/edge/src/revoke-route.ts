/**
 * `POST /notifications/revoke`: a revocation-only capability that survives
 * account logout and expired auth. The opaque binding id + random lease can
 * only disable that registration; they cannot read data, register a token or
 * send a notification.
 */
import { json } from "./env";
import { object, readNotificationJSON } from "./notifications-model";
import type { RouteContext } from "./router";

export const handleRevokeRoute = async ({ request, env, url }: RouteContext): Promise<Response | undefined> => {
  if (url.pathname !== "/notifications/revoke" || request.method !== "POST") return undefined;
  if (!env.PUSH_DEVICES) return json({ error: "unavailable" }, 503);
  try {
    const body = object(await readNotificationJSON(request));
    if (!/^[a-f0-9]{64}$/.test(String(body.bindingId)) ||
        !/^[a-f0-9]{64}$/.test(String(body.scope)) ||
        !/^[a-f0-9-]{36}$/.test(String(body.lease)) ||
        typeof body.epoch !== "number" || !Number.isSafeInteger(body.epoch) || body.epoch < 1) {
      return json({ error: "invalid" }, 400);
    }
    return env.PUSH_DEVICES.get(env.PUSH_DEVICES.idFromString(String(body.bindingId)))
      .fetch(new Request("https://push/unregister", { method: "POST", body: JSON.stringify({
        scope: body.scope, lease: body.lease, epoch: body.epoch
      }) }));
  } catch { return json({ error: "invalid" }, 400); }
};

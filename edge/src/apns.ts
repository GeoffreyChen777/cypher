import { importPKCS8, SignJWT } from "jose";
import type { Env } from "./env";
import { noticeText, type NoticeKind } from "./notifications-model";

export const APNS_TOPIC = "ai.mvp-lab.cypher.ios";
export function notificationsAvailable(env: Env): boolean {
  return env.NOTIFICATIONS_ENABLED === "true" && !!env.PUSH_DEVICES &&
    /^[A-Z0-9]{10}$/.test(env.APNS_TEAM_ID ?? "") &&
    /^[A-Z0-9]{10}$/.test(env.APNS_KEY_ID ?? "") && !!env.APNS_PRIVATE_KEY;
}
let cached: { key: string; keyId: string; team: string; jwt: string; until: number } | undefined;
async function providerToken(env: Env): Promise<string> {
  const now = Date.now();
  if (cached && cached.until > now && cached.key === env.APNS_PRIVATE_KEY &&
      cached.keyId === env.APNS_KEY_ID && cached.team === env.APNS_TEAM_ID) return cached.jwt;
  const key = await importPKCS8(env.APNS_PRIVATE_KEY!.replaceAll("\\n", "\n"), "ES256");
  const jwt = await new SignJWT({}).setProtectedHeader({ alg: "ES256", kid: env.APNS_KEY_ID! })
    .setIssuer(env.APNS_TEAM_ID!).setIssuedAt(Math.floor(now / 1000)).sign(key);
  cached = { key: env.APNS_PRIVATE_KEY!, keyId: env.APNS_KEY_ID!, team: env.APNS_TEAM_ID!, jwt, until: now + 50 * 60_000 };
  return jwt;
}
export interface PushMessage {
  id: string; scope: string; chatId: string; projectId: string; kind: NoticeKind; expires: number;
}
export async function sendAPNs(
  env: Env, token: string, environment: "development" | "production", message: PushMessage
): Promise<"sent" | "invalid" | "retry"> {
  if (!notificationsAvailable(env)) return "retry";
  try {
    const host = environment === "production" ? "api.push.apple.com" : "api.sandbox.push.apple.com";
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`${message.scope}/${message.chatId}`));
    const collapse = [...new Uint8Array(digest)].map(x => x.toString(16).padStart(2, "0")).join("").slice(0, 48);
    const response = await fetch(`https://${host}/3/device/${token}`, {
      method: "POST", redirect: "error", signal: AbortSignal.timeout(10_000),
      headers: {
        authorization: `bearer ${await providerToken(env)}`, "content-type": "application/json",
        "apns-topic": APNS_TOPIC, "apns-push-type": "alert", "apns-priority": "10",
        "apns-id": message.id, "apns-collapse-id": collapse,
        "apns-expiration": String(Math.floor(Math.min(message.expires, Date.now() + 300_000) / 1000))
      },
      body: JSON.stringify({
        aps: { alert: noticeText(message.kind), sound: "default", "thread-id": collapse },
        cypher: { version: 1, scope: message.scope, chatId: message.chatId,
          projectId: message.projectId, kind: message.kind, eventId: message.id }
      })
    });
    if (response.status === 200) return "sent";
    const body = await response.json().catch(() => ({})) as { reason?: string };
    if (response.status === 410 || ["BadDeviceToken", "DeviceTokenNotForTopic", "Unregistered"].includes(body.reason ?? "")) {
      return "invalid";
    }
    if (["ExpiredProviderToken", "InvalidProviderToken"].includes(body.reason ?? "")) cached = undefined;
    // Never log tokens, JWTs, private keys, response bodies or request URLs.
    return "retry";
  } catch { return "retry"; }
}

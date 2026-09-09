import { importPKCS8, SignJWT } from "jose";
import type { Env } from "./env";
import { noticeText, type NoticeKind } from "./notifications-model";

export const APNS_TOPIC = "ai.mvp-lab.cypher.ios";
const SAFE_APNS_REASONS = new Set([
  "BadDeviceToken", "DeviceTokenNotForTopic", "Unregistered",
  "ExpiredProviderToken", "InvalidProviderToken", "MissingProviderToken",
  "Forbidden", "TopicDisallowed", "BadTopic", "MissingTopic",
  "BadEnvironmentKeyIdInToken", "TooManyProviderTokenUpdates",
  "TooManyRequests", "InternalServerError", "ServiceUnavailable", "Shutdown",
  "BadExpirationDate", "BadMessageId", "BadPriority", "BadCollapseId",
  "BadPath", "PayloadEmpty", "PayloadTooLarge", "MethodNotAllowed"
]);
function diagnostic(stage: string, status?: number, reason?: unknown, error?: unknown): void {
  // Never interpolate error messages, URLs, tokens, payloads or identities.
  console.warn("apns_delivery", JSON.stringify({
    stage, ...(status === undefined ? {} : { status }),
    ...(reason === undefined ? {} : {
      reason: typeof reason === "string" && SAFE_APNS_REASONS.has(reason) ? reason : "Other"
    }),
    ...(error === undefined ? {} : {
      error: error instanceof DOMException ? error.name :
        error instanceof Error ? error.name : "Unknown"
    })
  }));
}
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
export interface BadgeSnapshot { badgeCount: number; badgeRevision: number }
export interface AlertPushMessage extends Partial<BadgeSnapshot> {
  id: string; scope: string; chatId: string; projectId: string; kind: NoticeKind; expires: number;
}
export interface BadgePushMessage extends BadgeSnapshot {
  id: string; scope: string; kind: "badge"; expires: number;
}
export type PushMessage = AlertPushMessage | BadgePushMessage;
export function validBadge(snapshot: Partial<BadgeSnapshot>): boolean {
  return Number.isSafeInteger(snapshot.badgeCount) && snapshot.badgeCount! >= 0 &&
    snapshot.badgeCount! <= 2_147_483_647 &&
    Number.isSafeInteger(snapshot.badgeRevision) && snapshot.badgeRevision! >= 0;
}
export async function sendAPNs(
  env: Env, token: string, environment: "development" | "production", message: PushMessage
): Promise<"sent" | "invalid" | "retry"> {
  if (!notificationsAvailable(env)) { diagnostic("not_configured"); return "retry"; }
  let stage = "prepare";
  try {
    const host = environment === "production" ? "api.push.apple.com" : "api.sandbox.push.apple.com";
    const badgeOnly = message.kind === "badge";
    if ((badgeOnly || message.badgeCount !== undefined || message.badgeRevision !== undefined) && !validBadge(message)) {
      return "retry";
    }
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(
      badgeOnly ? `badge/${message.scope}` : `${message.scope}/${message.chatId}`));
    const collapse = [...new Uint8Array(digest)].map(x => x.toString(16).padStart(2, "0")).join("").slice(0, 48);
    stage = "provider_token";
    const authorization = `bearer ${await providerToken(env)}`;
    stage = "transport";
    const response = await fetch(`https://${host}/3/device/${token}`, {
      method: "POST",
      headers: {
        authorization, "content-type": "application/json",
        "apns-topic": APNS_TOPIC, "apns-push-type": "alert", "apns-priority": badgeOnly ? "5" : "10",
        "apns-id": message.id, "apns-collapse-id": collapse,
        "apns-expiration": String(Math.floor(Math.min(message.expires, Date.now() + 300_000) / 1000))
      },
      body: JSON.stringify({
        aps: {
          ...(!badgeOnly ? { alert: noticeText(message.kind), sound: "default", "thread-id": collapse } : {}),
          ...(message.badgeCount === undefined ? {} : { badge: message.badgeCount })
        },
        cypher: { version: 1, scope: message.scope, kind: message.kind, eventId: message.id,
          ...(!badgeOnly ? { chatId: message.chatId, projectId: message.projectId } : {}),
          ...(message.badgeCount === undefined ? {} :
            { badgeCount: message.badgeCount, badgeRevision: message.badgeRevision })
        }
      })
    });
    if (response.status === 200) { diagnostic("accepted", 200); return "sent"; }
    stage = "response";
    const body = await response.json().catch(() => ({})) as { reason?: string };
    diagnostic("rejected", response.status, body?.reason ?? "Other");
    if (response.status === 410 || ["BadDeviceToken", "DeviceTokenNotForTopic", "Unregistered"].includes(body?.reason ?? "")) {
      return "invalid";
    }
    if (["ExpiredProviderToken", "InvalidProviderToken"].includes(body?.reason ?? "")) cached = undefined;
    // Never log tokens, JWTs, private keys, response bodies or request URLs.
    return "retry";
  } catch (error) { diagnostic(`${stage}_failed`, undefined, undefined, error); return "retry"; }
}

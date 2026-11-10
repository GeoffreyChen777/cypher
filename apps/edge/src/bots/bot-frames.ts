/**
 * Bot network wire shapes (docs/design/agent-bots.md §11.5): the frames a
 * BotRoom hands its host engine on `GET /bots/:id/inbox`, the limits both
 * sides enforce, and invite codes. Mirrored by crates/proto/src/bot_wire.rs;
 * both run protocol/vectors/bot-frames-v1.json (protocol/README.md).
 * Import-free, like the other wire codecs.
 */

export type WireState =
  | "queued"
  | "delivered"
  | "working"
  | "awaiting_review"
  | "answered"
  | "declined"
  | "expired";
export type AnswerStatus = "answered" | "declined" | "expired";
export type LinkOp = "added" | "revoked";

export interface WirePeer {
  botRoomId: string;
  name: string;
  ownerName: string;
  /** The owner's photo, from an identity provider's avatar host. */
  ownerAvatarUrl?: string;
}

/** Hosts an owner's photo may come from (the Rust side's `AVATAR_HOSTS`):
 * another person's devices load it, so nothing else is passed on. */
const AVATAR_HOSTS = ["avatars.githubusercontent.com", "googleusercontent.com", "gravatar.com", "workoscdn.com"];

/** `value` when it may be shown as a connected owner's photo: https, no
 * credentials or port, bounded, on an avatar host; else `undefined`. */
export const avatarUrl = (value: unknown): string | undefined => {
  if (typeof value !== "string" || value.length === 0 || value.length > 2048) return undefined;
  if (/[\s\u0000-\u001f\u007f\\]/.test(value) || !value.startsWith("https://")) return undefined;
  const rest = value.slice("https://".length);
  const end = rest.search(/[/?#]/);
  const host = (end === -1 ? rest : rest.slice(0, end)).toLowerCase();
  if (host.length === 0 || host.includes("@") || host.includes(":")) return undefined;
  return AVATAR_HOSTS.some((ok) => host === ok || host.endsWith(`.${ok}`)) ? value : undefined;
};

export type BotFrame =
  | { t: "ask"; exchangeId: string; connectionId: string; text: string; sentAt: number }
  | {
      t: "answer";
      exchangeId: string;
      connectionId: string;
      status: AnswerStatus;
      text?: string;
      answeredAt: number;
    }
  | { t: "state"; exchangeId: string; state: WireState }
  | { t: "link"; connectionId: string; op: LinkOp; peer?: WirePeer; inviteId?: string };

/** Longest question and answer, in UTF-8 bytes. */
export const QUESTION_MAX_BYTES = 8 * 1024;
export const ANSWER_MAX_BYTES = 64 * 1024;
/** Per connection, checked by both rooms. */
export const ASKS_PER_HOUR = 30;
export const ASKS_PER_DAY = 200;
export const ASKS_IN_FLIGHT = 3;
export const HOUR_MS = 3_600_000;
export const DAY_MS = 24 * HOUR_MS;
export const INVITE_TTL_MS = 7 * DAY_MS;
export const INVITE_MAX_USES = 1;
/** An exchange not settled this long after it was sent expires. */
export const EXCHANGE_TTL_MS = 7 * DAY_MS;

const WIRE_STATES: readonly WireState[] = [
  "queued",
  "delivered",
  "working",
  "awaiting_review",
  "answered",
  "declined",
  "expired"
];
const ANSWER_STATUSES: readonly AnswerStatus[] = ["answered", "declined", "expired"];

export const isWireState = (value: unknown): value is WireState =>
  typeof value === "string" && (WIRE_STATES as readonly string[]).includes(value);

export const isAnswerStatus = (value: unknown): value is AnswerStatus =>
  typeof value === "string" && (ANSWER_STATUSES as readonly string[]).includes(value);

export const isFinal = (state: string): boolean =>
  state === "answered" || state === "declined" || state === "expired";

export const utf8Bytes = (text: string): number => new TextEncoder().encode(text).length;

const isString = (value: unknown): value is string => typeof value === "string";
const isTime = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);

const parsePeer = (value: unknown): WirePeer | undefined => {
  if (typeof value !== "object" || value === null) return undefined;
  const peer = value as Record<string, unknown>;
  if (!isString(peer.botRoomId) || !isString(peer.name)) return undefined;
  const ownerName = peer.ownerName ?? "";
  if (!isString(ownerName)) return undefined;
  const ownerAvatarUrl = avatarUrl(peer.ownerAvatarUrl);
  return { botRoomId: peer.botRoomId, name: peer.name, ownerName, ...(ownerAvatarUrl ? { ownerAvatarUrl } : {}) };
};

/** A frame in its wire shape, or `undefined` when it is malformed or of a
 * kind this side does not know (the Rust client skips unknown kinds). */
export const parseBotFrame = (value: unknown): BotFrame | undefined => {
  if (typeof value !== "object" || value === null) return undefined;
  const frame = value as Record<string, unknown>;
  switch (frame.t) {
    case "ask":
      return isString(frame.exchangeId) && isString(frame.connectionId) && isString(frame.text) && isTime(frame.sentAt)
        ? { t: "ask", exchangeId: frame.exchangeId, connectionId: frame.connectionId, text: frame.text, sentAt: frame.sentAt }
        : undefined;
    case "answer": {
      if (!isString(frame.exchangeId) || !isString(frame.connectionId) || !isAnswerStatus(frame.status) || !isTime(frame.answeredAt)) {
        return undefined;
      }
      if (frame.text !== undefined && !isString(frame.text)) return undefined;
      return {
        t: "answer",
        exchangeId: frame.exchangeId,
        connectionId: frame.connectionId,
        status: frame.status,
        ...(frame.text === undefined ? {} : { text: frame.text }),
        answeredAt: frame.answeredAt
      };
    }
    case "state":
      return isString(frame.exchangeId) && isWireState(frame.state)
        ? { t: "state", exchangeId: frame.exchangeId, state: frame.state }
        : undefined;
    case "link": {
      if (!isString(frame.connectionId) || (frame.op !== "added" && frame.op !== "revoked")) return undefined;
      const peer = frame.peer === undefined ? undefined : parsePeer(frame.peer);
      if (frame.peer !== undefined && !peer) return undefined;
      if (frame.inviteId !== undefined && !isString(frame.inviteId)) return undefined;
      return {
        t: "link",
        connectionId: frame.connectionId,
        op: frame.op,
        ...(peer ? { peer } : {}),
        ...(isString(frame.inviteId) ? { inviteId: frame.inviteId } : {})
      };
    }
    default:
      return undefined;
  }
};

const CODE_PART_RE = /^[A-Za-z0-9_-]{1,128}$/;

/** `<botRoomId>.<secret>` → `[botRoomId, secret]`. */
export const splitInviteCode = (code: string): [room: string, secret: string] | undefined => {
  const trimmed = code.trim();
  const dot = trimmed.indexOf(".");
  if (dot < 0) return undefined;
  const room = trimmed.slice(0, dot);
  const secret = trimmed.slice(dot + 1);
  return CODE_PART_RE.test(room) && CODE_PART_RE.test(secret) ? [room, secret] : undefined;
};

import { ID_RE } from "../identifiers";

/** Notification data intentionally excludes prompts, titles and model output. */
export const NOTICE_DELAY_MS = 10_000;
export const ACTIVITY_LEASE_MS = 45_000;
/** Clock tolerance between the device that stamped a chat's synced seen
 * marker and the host that stamped the event (both are device clocks). */
export const SEEN_SLACK_MS = 2_000;
const INTERACTION_MS = 120_000;
export const SHORT_RUN_MS = 30_000;
export type NoticeKind = "completed" | "failed" | "input";
export interface NotificationSettings {
  mode: "smart" | "actionable" | "always" | "off";
  completed: boolean;
  failed: boolean;
  input: boolean;
  subagents: boolean;
  mutedProjects: string[];
}
export const defaultNotificationSettings = (): NotificationSettings => ({
  mode: "always", completed: true, failed: true, input: true, subagents: false, mutedProjects: []
});
export interface Activity {
  clientId: string;
  platform: "desktop" | "ios";
  foreground: boolean;
  interactionAt: number;
  receivedAt: number;
  openedAt: number;
  chatId: string | null;
  sequence: number;
}
export interface Notice {
  id: string;
  chatId: string;
  projectId: string;
  kind: NoticeKind;
  child: boolean;
  run: string;
  at: number;
  expires: number;
  recipients: { id: string; lease: string }[];
  attempt: number;
  sessionStatus?: string;
  source?: "event";
  /** Host timestamp of the transition; the synced seen marker is compared
   * against it. Absent on events queued before this field existed. */
  eventAt?: number;
}
export function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("invalid object");
  return value as Record<string, unknown>;
}
export function identifier(value: unknown): string {
  if (typeof value !== "string" || !ID_RE.test(value)) throw new Error("invalid identifier");
  return value;
}
export function parseSettings(value: unknown): NotificationSettings {
  const v = object(value);
  for (const key of ["completed", "failed", "input", "subagents"]) {
    if (typeof v[key] !== "boolean") throw new Error("invalid setting");
  }
  if (!Array.isArray(v.mutedProjects) || v.mutedProjects.length > 128) throw new Error("invalid project list");
  return {
    mode: "always", completed: v.completed as boolean,
    failed: v.failed as boolean, input: v.input as boolean, subagents: v.subagents as boolean,
    mutedProjects: [...new Set(v.mutedProjects.map(identifier))]
  };
}
export function parseActivity(value: unknown, previous: Activity | undefined, now: number): Activity {
  const v = object(value);
  const clientId = identifier(v.clientId);
  if (!["desktop", "ios"].includes(String(v.platform)) || typeof v.foreground !== "boolean" ||
      typeof v.sequence !== "number" || !Number.isSafeInteger(v.sequence) || v.sequence < 1 ||
      typeof v.interactionAgeMs !== "number" || !Number.isFinite(v.interactionAgeMs) ||
      v.interactionAgeMs < 0 || v.interactionAgeMs > 86_400_000) throw new Error("invalid activity");
  const chatId = v.chatId === null ? null : identifier(v.chatId);
  if (previous && previous.sequence >= v.sequence) return previous;
  return {
    clientId, sequence: v.sequence, platform: v.platform as Activity["platform"], foreground: v.foreground,
    interactionAt: now - v.interactionAgeMs, receivedAt: now, chatId,
    openedAt: v.foreground && (!previous?.foreground || previous.chatId !== chatId) ? now : previous?.openedAt ?? now
  };
}
export function active(activity: Activity, now: number): boolean {
  return activity.foreground && now - activity.receivedAt <= ACTIVITY_LEASE_MS &&
    now - activity.interactionAt <= INTERACTION_MS;
}
/** Which of a chat's events reach the owner's phones — the desktop's rule for
 * its chimes and Dock badge (`Chat::is_user_facing`, `AppState::attention_count`):
 * - `all`: a conversation the owner follows (a root session, a workspace's own chat);
 * - `input`: a session a workspace started — what it finished is its
 *   workspace's to report, only its questions are the owner's;
 * - `none`: hidden threads (work, routine, peer, routine-session and unknown
 *   kinds) and memory passes — nothing on any device would show or clear them.
 * Archived, deleted and project-less chats are the callers' own checks. */
export type ChatAttention = "all" | "input" | "none";
export function chatAttention(fields: Record<string, unknown>): ChatAttention {
  const present = (value: unknown) => value !== undefined && value !== null;
  if (present(fields.startedBy)) return "input";
  if (present(fields.memory)) return "none";
  const bot = fields.bot;
  if (!present(bot)) return "all";
  if (typeof bot !== "object" || Array.isArray(bot)) return "none";
  // `BotChat`: a missing origin is the owner, a missing thread is `main`.
  const { origin, thread } = bot as Record<string, unknown>;
  const owner = !present(origin) || origin === "owner";
  const main = !present(thread) ||
    (typeof thread === "object" && !Array.isArray(thread) && (thread as Record<string, unknown>).kind === "main");
  return owner && main ? "all" : "none";
}
/** The stricter of a chat's and its parent's attention: a subagent of a
 * hidden thread is hidden too. */
export function narrower(a: ChatAttention, b: ChatAttention): ChatAttention {
  return a === "none" || b === "none" ? "none" : a === "input" || b === "input" ? "input" : "all";
}
export function attends(attention: ChatAttention, kind: NoticeKind): boolean {
  return attention === "all" || (attention === "input" && kind === "input");
}
export function iosViewingChat(chatId: string, activities: Activity[], now: number): boolean {
  return activities.some(a => a.platform === "ios" && a.chatId === chatId && active(a, now));
}
export function notificationDecision(
  notice: Notice, settings: NotificationSettings, activities: Activity[], now: number
): "send" | "drop" | "defer" {
  if (now >= notice.expires || !settings[notice.kind] ||
      settings.mutedProjects.includes(notice.projectId) ||
      (notice.child && notice.kind !== "input" && !settings.subagents)) return "drop";
  // Opening the session counts as reading it; loading/scroll position is not
  // part of this policy. A fresh foreground viewer can also see events that
  // arrive while it stays on the page. Historical or background views cannot
  // suppress a later run.
  if (iosViewingChat(notice.chatId, activities, now)) return "drop";
  return "send";
}
export function noticeText(kind: NoticeKind): { title: string; body: string } {
  const title = { completed: "Task completed", failed: "Task failed", input: "Your input is needed" }[kind];
  return { title, body: "Open Cypher to view the session." };
}
export async function readNotificationJSON(request: Request): Promise<unknown> {
  if (!request.body) throw new Error("missing body");
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  while (true) {
    const result = await reader.read();
    if (result.done) break;
    length += result.value.byteLength;
    if (length > 16_384) { await reader.cancel(); throw new Error("body too large"); }
    chunks.push(result.value);
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return JSON.parse(new TextDecoder().decode(bytes));
}
export const notificationJSON = (value: unknown, status = 200): Response =>
  Response.json(value, { status, headers: { "cache-control": "no-store" } });

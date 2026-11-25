import { ID_RE } from "../identifiers";

/** Notification data intentionally excludes prompts, titles and model output. */
export const NOTICE_DELAY_MS = 10_000;
export const ACTIVITY_LEASE_MS = 45_000;
/** Clock tolerance between the device that stamped a chat's synced seen
 * marker and the host that stamped the event (both are device clocks). */
export const SEEN_SLACK_MS = 2_000;
const INTERACTION_MS = 120_000;
export const SHORT_RUN_MS = 30_000;
/** How long an unanswered question (or Needs you item) stays deliverable. */
export const INPUT_EXPIRY_MS = 8 * 3_600_000;
export type NoticeKind = "completed" | "failed" | "input";
/** One item on a workspace's **Needs you** card (`cypher_proto::NeedsYou`):
 * a plan awaiting approval (work id), an action awaiting approval (approval
 * id), a peer answer drafted for review (exchange id), or one of the
 * workspace's hidden threads asking (that thread's chat id). */
export type NeedsKind = "plan" | "approval" | "review" | "asking";
export interface NeedsItem { kind: NeedsKind; id: string }
/** The registry row kinds behind the record items (`bot_records.rs`). */
export const NEEDS_RECORD_KINDS: Readonly<Record<string, Exclude<NeedsKind, "asking">>> = {
  botWork: "plan", botApprovals: "approval", botExchanges: "review"
};
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
  /** `projectId` names a workspace (its `bots` row), not a project: the
   * notice belongs to the workspace's own chat. */
  workspace?: boolean;
  /** The Needs you item this notice announces; absent for a session's own
   * status notice. */
  item?: NeedsItem;
  /** For a plan's question (`item` names the plan's session): the chat
   * that asked, when it is a subagent's chat under that session. */
  from?: string;
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
/** The workspace (bot id) a chat's marker names, whatever thread it is. */
export function botIdOf(fields: Record<string, unknown>): string | undefined {
  const bot = fields.bot;
  if (!bot || typeof bot !== "object" || Array.isArray(bot)) return undefined;
  const id = (bot as Record<string, unknown>).botId;
  return typeof id === "string" && ID_RE.test(id) ? id : undefined;
}
/** The workspace a hidden thread's questions belong to: a plan's work
 * thread, a routine's thread or a project routine's session, whose questions
 * wait in that workspace's Needs you. Peer threads, threads from a newer
 * engine, memory passes and sessions a workspace started (those notify as
 * themselves) are not. */
export function askingThreadOf(fields: Record<string, unknown>): string | undefined {
  const present = (value: unknown) => value !== undefined && value !== null;
  if (present(fields.startedBy) || present(fields.memory)) return undefined;
  const botId = botIdOf(fields);
  if (!botId) return undefined;
  const { origin, thread } = fields.bot as Record<string, unknown>;
  if (present(origin) && origin !== "owner" && origin !== "routine") return undefined;
  if (!thread || typeof thread !== "object" || Array.isArray(thread)) return undefined;
  const kind = (thread as Record<string, unknown>).kind;
  return kind === "work" || kind === "routine" || kind === "routineSession" ? botId : undefined;
}
/** The open question on a session row or a host's event report
 * (`Session.asking`, docs/design/plan-asks.md §6): present only while one is
 * open, absent on old hosts. `fromChatId` names the child chat a root's
 * question was mirrored from (the child's own row carries it too). */
export interface Question { requestId: string; fromChatId?: string }
export function questionOf(value: unknown): Question | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const { requestId, fromChatId } = value as Record<string, unknown>;
  if (typeof requestId !== "string" || !requestId || requestId.length > 256) return undefined;
  return typeof fromChatId === "string" && ID_RE.test(fromChatId) ? { requestId, fromChatId } : { requestId };
}
/** A session's status as notices read it: an open question is awaiting
 * input whatever the status says (a run that died under it leaves the row
 * idle; view.rs `effective_indicator`). Rows of old hosts: their status. */
export function askingStatus(status: unknown, question: Question | undefined): unknown {
  return question ? "awaitingInput" : status;
}
/** The workspace whose plan started this chat (`startedBy`), if any. */
export function startedByOf(fields: Record<string, unknown>): string | undefined {
  const by = fields.startedBy;
  if (!by || typeof by !== "object" || Array.isArray(by)) return undefined;
  const id = (by as Record<string, unknown>).botId;
  return typeof id === "string" && ID_RE.test(id) ? id : undefined;
}
/** How many chats up a subagent's question is looked for (plan_asks.rs
 * `MAX_CHILD_DEPTH`). */
export const MAX_CHILD_DEPTH = 8;
/** Does this record row wait on its owner (`BotWork::awaiting_approval`,
 * `BotApproval::awaiting`, `BotExchange::awaiting_review`)? Missing fields
 * read as the Rust defaults: a work row's `state` is `proposed`, an
 * approval's `pending`. */
export function needsOwner(rowKind: string, fields: Record<string, unknown>): boolean {
  const absent = (value: unknown) => value === undefined || value === null;
  switch (NEEDS_RECORD_KINDS[rowKind]) {
    case "plan": return (absent(fields.state) || fields.state === "proposed") && absent(fields.decision);
    case "approval": return (absent(fields.state) || fields.state === "pending") && absent(fields.decision);
    case "review": return fields.direction === "incoming" && fields.state === "awaitingReview" && absent(fields.review);
    default: return false;
  }
}
/** When a record item was created, on its host's clock. */
export function needsStamp(rowKind: string, fields: Record<string, unknown>): number | undefined {
  const stamp = NEEDS_RECORD_KINDS[rowKind] === "review" ? fields.askedAt : fields.createdAt;
  return typeof stamp === "number" ? stamp : undefined;
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
export function noticeText(kind: NoticeKind, workspace = false): { title: string; body: string } {
  const title = { completed: "Task completed", failed: "Task failed", input: "Your input is needed" }[kind];
  return { title, body: workspace ? "Open Cypher to view the workspace." : "Open Cypher to view the session." };
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

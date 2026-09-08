/** Notification data intentionally excludes prompts, titles and model output. */
export const NOTICE_DELAY_MS = 10_000;
export const ACTIVITY_LEASE_MS = 45_000;
export const INTERACTION_MS = 120_000;
export const SHORT_RUN_MS = 30_000;
export const ID = /^[A-Za-z0-9_-]{1,128}$/;
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
}
export function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("invalid object");
  return value as Record<string, unknown>;
}
export function identifier(value: unknown): string {
  if (typeof value !== "string" || !ID.test(value)) throw new Error("invalid identifier");
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
export function notificationDecision(
  notice: Notice, settings: NotificationSettings, _activities: Activity[], now: number
): "send" | "drop" | "defer" {
  if (now >= notice.expires || !settings[notice.kind] ||
      settings.mutedProjects.includes(notice.projectId) ||
      (notice.child && notice.kind !== "input" && !settings.subagents)) return "drop";
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

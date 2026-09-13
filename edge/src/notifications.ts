import type { Env } from "./env";
import type { Row } from "./workspace3-model";
import { notificationsAvailable } from "./apns";
import type { BadgeSnapshot } from "./apns";
import {
  defaultNotificationSettings, identifier, object, parseActivity, parseSettings,
  readNotificationJSON, notificationJSON as json, notificationDecision, iosViewingChat,
  NOTICE_DELAY_MS, SHORT_RUN_MS, ACTIVITY_LEASE_MS, type Activity, type Notice, type NoticeKind, type NotificationSettings
} from "./notifications-model";

interface Recipient { id: string; lease: string; installationId: string; epoch: number }
interface SessionTarget { clientId: string; platform: "desktop" | "ios"; at: number }
interface BadgeJob extends BadgeSnapshot { id: string; due: number; expires: number; attempt: number }
type UnreadEvent = Pick<Notice, "id" | "chatId" | "projectId" | "kind" | "child">;

function asyncChildren(fields: Row["fields"] | undefined) {
  const runs = (Array.isArray(fields?.subagents) ? fields.subagents : []).slice(0, 32)
    .flatMap(value => value && typeof value === "object" && !Array.isArray(value) && value.mode === "async" ? [value] : []);
  return {
    live: runs.some(run => run.status === "running"),
    failed: runs.some(run => run.status === "error"),
    latest: Math.max(0, ...runs.map(run => typeof run.updatedAt === "number" ? run.updatedAt : 0))
  };
}

/** Per-user, durable policy/outbox. Writes run inside the registry's row
 * mutation event, not a best-effort network callback after committing it. */
export class Notifications {
  constructor(
    private ctx: DurableObjectState, private env: Env,
    private row: (kind: string, id: string) => Row | undefined,
    private schedule: () => void
  ) {
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notify_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notify_events (id TEXT PRIMARY KEY, due INTEGER NOT NULL, value TEXT NOT NULL)");
    // Delivered events leave the outbox but remain unread until the session
    // is opened. One row per chat, never one row per notification or retry.
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notify_unread (chat_id TEXT PRIMARY KEY, value TEXT NOT NULL)");
  }
  private get<T>(key: string): T | undefined {
    const row = [...this.ctx.storage.sql.exec("SELECT value FROM notify_kv WHERE key = ?", key)][0];
    return row ? JSON.parse(row.value as string) as T : undefined;
  }
  private set(key: string, value: unknown): void {
    this.ctx.storage.sql.exec("INSERT INTO notify_kv(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, JSON.stringify(value));
  }
  private recipients(): Recipient[] { return this.get<Recipient[]>("recipients") ?? []; }
  private settings(): NotificationSettings { return this.get<NotificationSettings>("settings") ?? defaultNotificationSettings(); }
  private target(chatId: string): SessionTarget | undefined { return this.get<SessionTarget>(`target:${chatId}`); }
  private scope(): string { return this.ctx.id.toString(); }
  private badge(): BadgeSnapshot {
    return {
      badgeCount: Number([...this.ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM notify_unread")][0].count),
      badgeRevision: this.get<number>("badgeRevision") ?? 0
    };
  }
  private queueBadge(due = Date.now()): void {
    const snapshot = this.badge(), previous = this.get<BadgeJob>("badgeJob");
    this.set("badgeJob", { ...snapshot, id: crypto.randomUUID(), due: Math.min(due, previous?.due ?? due),
      expires: Date.now() + 600_000, attempt: 0 } satisfies BadgeJob);
  }
  private badgeChanged(due = Date.now()): void {
    this.set("badgeRevision", (this.get<number>("badgeRevision") ?? 0) + 1);
    this.queueBadge(due);
  }
  private markUnread(notice: Notice): void {
    if (notificationDecision(notice, this.settings(), [], Date.now()) === "drop") return;
    const unread: UnreadEvent = { id: notice.id, chatId: notice.chatId, projectId: notice.projectId,
      kind: notice.kind, child: notice.child };
    this.ctx.storage.sql.exec("INSERT INTO notify_unread(chat_id,value) VALUES(?,?) ON CONFLICT(chat_id) DO UPDATE SET value=excluded.value",
      notice.chatId, JSON.stringify(unread));
    this.badgeChanged(Date.now() + NOTICE_DELAY_MS);
  }
  private clearUnread(chatId: string, eventId?: string): string | undefined {
    const row = [...this.ctx.storage.sql.exec("SELECT value FROM notify_unread WHERE chat_id=?", chatId)][0];
    if (!row) return;
    const notice = JSON.parse(row.value as string) as UnreadEvent;
    if (eventId && notice.id !== eventId) return;
    this.ctx.storage.sql.exec("DELETE FROM notify_unread WHERE chat_id=?", chatId);
    this.badgeChanged();
    return notice.id;
  }
  private pruneUnread(): void {
    const prefs = this.settings();
    for (const row of [...this.ctx.storage.sql.exec("SELECT value FROM notify_unread")]) {
      const notice = JSON.parse(row.value as string) as UnreadEvent;
      const chat = this.row("chats", notice.chatId), project = this.row("spaces", notice.projectId);
      if (!chat || chat.deleted || chat.fields.archived || !project || project.deleted ||
          !prefs[notice.kind] || prefs.mutedProjects.includes(notice.projectId) ||
          (notice.child && notice.kind !== "input" && !prefs.subagents)) {
        this.clearUnread(notice.chatId);
      }
    }
  }

  async fetch(request: Request, path: string): Promise<Response> {
    if (path === "settings" && request.method === "GET") {
      this.pruneUnread();
      this.schedule();
      return json({ available: notificationsAvailable(this.env), scope: this.scope(), settings: this.settings(), ...this.badge() });
    }
    try {
      const body = object(await readNotificationJSON(request));
      if (path === "settings" && request.method === "PUT") {
        this.set("settings", parseSettings(body));
        this.pruneUnread();
        this.schedule();
        return json({ settings: this.settings(), scope: this.scope(), ...this.badge() });
      }
      if (path === "activity" && request.method === "POST") {
        return this.ctx.storage.transactionSync(() => {
        if (!notificationsAvailable(this.env)) return json({ ok: true, available: false });
        const now = Date.now(), id = identifier(body.clientId);
        const current = this.get<Activity[]>("activity") ?? [];
        const previous = current.find(a => a.clientId === id);
        const activity = parseActivity(body, previous, now);
        // Duplicate/reordered reports must not replay a historical read,
        // refresh its lease or route a new event to an old page.
        if (activity === previous) return json({ ok: true, available: true, scope: this.scope(), readEventIds: [], ...this.badge() });
        const remaining = current.filter(a => a.clientId !== id && now - a.receivedAt < 600_000);
        this.set("activity", [...remaining.slice(-63), activity]);
        const readEventIds: string[] = [];
        if (activity.chatId && activity.foreground) {
          this.set(`target:${activity.chatId}`, { clientId: activity.clientId, platform: activity.platform, at: now });
          if (activity.platform === "ios") {
            // Atomically retire only events that already exist for this chat.
            // Keep enqueued:<chat> intact so mirror replay cannot recreate them.
            for (const { notice } of this.events()) {
              if (notice.chatId !== activity.chatId) continue;
              this.remove(notice.id);
              readEventIds.push(notice.id);
            }
            const unreadId = this.clearUnread(activity.chatId);
            if (unreadId && !readEventIds.includes(unreadId)) readEventIds.push(unreadId);
          }
        }
        if (readEventIds.length) this.schedule();
        return json({ ok: true, available: true, scope: this.scope(), readEventIds, ...this.badge() });
        });
      }
      if (path === "event" && request.method === "POST") {
        return this.ctx.storage.transactionSync(() => {
        const chatId = identifier(body.chatId), deviceId = identifier(body.deviceId);
        const status = String(body.status);
        if (!["idle", "working", "awaitingInput", "errored"].includes(status) ||
            typeof body.updatedAt !== "number" || !Number.isSafeInteger(body.updatedAt) ||
            Math.abs(Date.now() - body.updatedAt) > 300_000) throw new Error();
        const chat = this.row("chats", chatId);
        if (!chat || chat.deleted || chat.fields.archived || chat.fields.deviceId !== deviceId ||
            typeof chat.fields.spaceId !== "string" || !this.row("spaces", chat.fields.spaceId)) {
          return json({ ok: true, ignored: true });
        }
        const subagents = (Array.isArray(body.subagents) ? body.subagents : []).slice(0, 32).map(value => {
          const run = object(value);
          if (!["async", "sync", "message"].includes(String(run.mode)) ||
              !["running", "done", "error"].includes(String(run.status)) ||
              typeof run.updatedAt !== "number") throw new Error();
          return { mode: String(run.mode), status: String(run.status), updatedAt: run.updatedAt };
        });
        const eventFields = { status, subagents } as Row["fields"];
        const last = this.get<Row["fields"]>(`eventState:${chatId}`);
        if (last && typeof last.updatedAt === "number" && last.updatedAt >= body.updatedAt) {
          return json({ ok: true, stale: true });
        }
        this.set(`eventState:${chatId}`, { ...eventFields, updatedAt: body.updatedAt });
        const previous = this.get<string>(`eventStatus:${chatId}`);
        const children = asyncChildren(eventFields);
        const childrenSettled = status === "idle" && previous === "idle" &&
          asyncChildren(last).live && !children.live;
        if (previous === status && !childrenSettled) return json({ ok: true, duplicate: true });
        this.set(`eventStatus:${chatId}`, status);
        if (status === "working") {
          this.set(`run:${chatId}`, typeof body.startedAt === "number" ? String(body.startedAt) : crypto.randomUUID());
          return json({ ok: true });
        }
        let kind: NoticeKind | undefined;
        if (status === "awaitingInput") kind = "input";
        else if (status === "errored") kind = "failed";
        else if (status === "idle" && !children.live &&
          (previous === "working" || previous === "awaitingInput" || childrenSettled)) {
          kind = children.failed ? "failed" : "completed";
        }
        if (!kind) return json({ ok: true });
        const run = this.get<string>(`run:${chatId}`) ?? crypto.randomUUID();
        const startedAt = typeof body.startedAt === "number" ? body.startedAt : Number(run);
        if (kind === "completed" && Number.isFinite(startedAt) && Date.now() - startedAt < SHORT_RUN_MS) {
          return json({ ok: true, short: true });
        }
        const recipients = this.recipients().map(({ id, lease }) => ({ id, lease }));
        if (!recipients.length) return json({ ok: true, noRecipients: true });
        const notice: Notice = {
          id: crypto.randomUUID(), chatId, projectId: chat.fields.spaceId, kind,
          child: !!chat.fields.child, run, at: Date.now(),
          expires: Date.now() + (kind === "input" ? 8 * 3_600_000 : 600_000),
          recipients, attempt: 0, sessionStatus: status, source: "event"
        };
        if (this.get<string>(`enqueued:${chatId}`) === `${run}/${kind}`) return json({ ok: true, duplicate: true });
        this.set(`enqueued:${chatId}`, `${run}/${kind}`);
        for (const pending of this.events()) if (pending.notice.chatId === chatId) this.remove(pending.notice.id);
        // An event arriving while the session is already visible is read too.
        // Remember the dedupe marker even if the viewer leaves before flush.
        if (iosViewingChat(chatId, this.get<Activity[]>("activity") ?? [], Date.now())) {
          this.clearUnread(chatId);
          this.schedule();
          return json({ ok: true, read: true });
        }
        if (this.events().length >= 256) this.remove(this.events()[0].notice.id);
        this.put(notice, Date.now() + NOTICE_DELAY_MS);
        this.markUnread(notice);
        this.schedule();
        return json({ ok: true, queued: true });
        });
      }
      if (path === "register" && request.method === "POST") {
        if (!notificationsAvailable(this.env)) return json({ error: "push_unavailable" }, 503);
        const installationId = identifier(body.installationId);
        const epoch = body.epoch;
        if (typeof epoch !== "number" || !Number.isSafeInteger(epoch) || epoch < 1 ||
            typeof body.token !== "string" || !/^(?:[a-f0-9]{2}){16,128}$/.test(body.token) ||
            !["development", "production"].includes(String(body.environment))) throw new Error();
        const key = `epoch:${installationId}`, lastEpoch = this.get<number>(key) ?? 0;
        if (epoch < lastEpoch) return json({ error: "stale" }, 409);
        if (this.recipients().filter(r => r.installationId !== installationId).length >= 16) {
          return json({ error: "too_many_devices" }, 409);
        }
        this.set(key, epoch);
        const ns = this.env.PUSH_DEVICES!;
        const id = ns.idFromName(`apns/${body.environment}/${body.token}`);
        const response = await ns.get(id).fetch(new Request("https://push/register", {
          method: "POST", body: JSON.stringify({ ...body, scope: this.scope() })
        }));
        if (!response.ok) return json({ error: "registration_failed" }, 409);
        const reply = await response.json() as { lease: string };
        if (this.get<number>(key) !== epoch) return json({ error: "stale" }, 409);
        const recipient: Recipient = { id: id.toString(), lease: reply.lease, installationId, epoch };
        this.set("recipients", [...this.recipients().filter(r => r.installationId !== installationId), recipient]);
        this.queueBadge();
        this.schedule();
        return json({ scope: this.scope(), bindingId: recipient.id, lease: recipient.lease });
      }
      if (path === "unregister" && request.method === "POST") {
        const installationId = identifier(body.installationId);
        const epoch = body.epoch;
        if (typeof epoch !== "number" || !Number.isSafeInteger(epoch) || epoch < 1) throw new Error();
        const key = `epoch:${installationId}`;
        if (epoch <= (this.get<number>(key) ?? 0)) return json({ error: "stale" }, 409);
        this.set(key, epoch);
        const targets = this.recipients().filter(r => r.installationId === installationId);
        this.set("recipients", this.recipients().filter(r => r.installationId !== installationId));
        for (const recipient of targets) {
          await this.deviceCall(recipient, "/unregister", { epoch }).catch(() => undefined);
        }
        return json({ ok: true });
      }
    } catch { return json({ error: "invalid_request" }, 400); }
    return json({ error: "not_found" }, 404);
  }

  observe(changes: { before: Row | undefined; after: Row }[], sourceDevice: string): void {
    if (!notificationsAvailable(this.env)) return;
    const now = Date.now();
    for (const { before, after } of changes) {
      // The execution engine does publish its session row to the registry,
      // including iOS-started runs. Only that host's mutations are eligible.
      if (after.kind !== "sessions" || after.deleted || after.fields.deviceId !== sourceDevice) continue;
      const chatId = after.fields.chatId;
      if (typeof chatId !== "string") continue;
      const status = after.fields.status;
      const previousStatus = before?.fields.status;
      const children = asyncChildren(after.fields);
      const childrenSettled = status === "idle" && previousStatus === "idle" &&
        asyncChildren(before?.fields).live && !children.live;
      const started = after.fields.startedAt;
      const key = `run:${chatId}`;
      let run = this.get<string>(key);
      if (status === "working" && (previousStatus === "idle" || previousStatus === "errored" || !run)) {
        run = typeof started === "number" ? String(started) : crypto.randomUUID();
        this.set(key, run);
      }
      // Baseline seeds, unchanged heartbeats, stale replay and non-host
      // replication are not fresh notification events.
      const eventAt = childrenSettled ? children.latest : after.fields.updatedAt;
      if (!before || before.deleted || (previousStatus === status && !childrenSettled) ||
          typeof eventAt !== "number" || Math.abs(now - eventAt) > 300_000) continue;
      let kind: NoticeKind;
      if (status === "awaitingInput") kind = "input";
      else if (status === "errored") kind = "failed";
      else if (status === "idle" && (childrenSettled || ["working", "awaitingInput"].includes(String(previousStatus)))) {
        // An async launch acknowledgement is not the whole task finishing.
        // The parent's later snapshot transition produces one fresh aggregate
        // result, even if the parent's own status has remained idle for hours.
        if (children.live) continue;
        kind = childrenSettled && children.failed ? "failed" : "completed";
        const began = typeof before.fields.startedAt === "number" ? before.fields.startedAt : Number(run);
        if (kind === "completed" && Number.isFinite(began) && now - began < SHORT_RUN_MS) continue;
      } else continue;
      const chat = this.row("chats", chatId);
      if (!chat || chat.deleted || chat.fields.archived || typeof chat.fields.spaceId !== "string" ||
          chat.fields.deviceId !== after.fields.deviceId) continue;
      const project = this.row("spaces", chat.fields.spaceId);
      if (!project || project.deleted) continue;
      if (!run) { run = crypto.randomUUID(); this.set(key, run); }
      const recipients = this.recipients().map(({ id, lease }) => ({ id, lease }));
      if (!recipients.length) continue;
      if (this.get<string>(`enqueued:${chatId}`) === `${run}/${kind}`) continue;
      this.set(`enqueued:${chatId}`, `${run}/${kind}`);
      const notice: Notice = {
        id: crypto.randomUUID(), chatId, projectId: chat.fields.spaceId, kind,
        child: !!chat.fields.child, run, at: now, expires: now + (kind === "input" ? 8 * 3_600_000 : 600_000),
        recipients, attempt: 0, sessionStatus: String(status)
      };
      // Replace superseded state for this chat, rather than accumulating
      // completion/input/error alerts during flapping.
      for (const pending of this.events()) {
        if (pending.notice.chatId === chatId) this.remove(pending.notice.id);
      }
      if (iosViewingChat(chatId, this.get<Activity[]>("activity") ?? [], now)) { this.clearUnread(chatId); continue; }
      if (this.events().length >= 256) {
        const oldest = this.events()[0]; if (oldest) this.remove(oldest.notice.id);
      }
      this.put(notice, now + NOTICE_DELAY_MS);
      this.markUnread(notice);
    }
    this.pruneUnread();
    this.schedule();
  }

  private events(): { notice: Notice; due: number }[] {
    return [...this.ctx.storage.sql.exec("SELECT value,due FROM notify_events ORDER BY due")]
      .map(r => ({ notice: JSON.parse(r.value as string) as Notice, due: r.due as number }));
  }
  private put(notice: Notice, due: number): void {
    this.ctx.storage.sql.exec("INSERT INTO notify_events(id,due,value) VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET due=excluded.due,value=excluded.value",
      notice.id, due, JSON.stringify(notice));
  }
  private remove(id: string): void { this.ctx.storage.sql.exec("DELETE FROM notify_events WHERE id=?", id); }
  clearPending(): void {
    this.ctx.storage.sql.exec("DELETE FROM notify_events");
    this.set("badgeJob", null);
  }
  nextDue(): number | undefined {
    const next = Math.min(this.events()[0]?.due ?? Infinity, this.get<BadgeJob>("badgeJob")?.due ?? Infinity);
    return Number.isFinite(next) ? next : undefined;
  }
  private async deviceCall(recipient: { id: string; lease: string }, path: string, extra: Record<string, unknown>): Promise<Response> {
    const ns = this.env.PUSH_DEVICES!;
    return ns.get(ns.idFromString(recipient.id)).fetch(new Request(`https://push${path}`, {
      method: "POST", body: JSON.stringify({ ...extra, scope: this.scope(), lease: recipient.lease })
    }));
  }

  async flush(): Promise<void> {
    if (!notificationsAvailable(this.env)) { this.clearPending(); return; }
    this.pruneUnread();
    const now = Date.now();
    // At most 32 recipient calls per alarm, also within the free-tier
    // subrequest budget. Remaining due work gets another alarm.
    for (const { notice, due } of this.events().slice(0, 2)) {
      if (due > now) break;
      const chat = this.row("chats", notice.chatId), project = this.row("spaces", notice.projectId);
      const session = notice.source === "event"
        ? (() => {
          const state = this.get<{ status: string; updatedAt: number }>(`eventState:${notice.chatId}`);
          return state ? { fields: state, deleted: false } : undefined;
        })()
        : this.row("sessions", notice.chatId);
      const expected = notice.sessionStatus ?? { completed: "idle", failed: "errored", input: "awaitingInput" }[notice.kind];
      if (!chat || chat.deleted || chat.fields.archived || !project || project.deleted ||
          !session || session.deleted || session.fields.status !== expected ||
          this.get<string>(`run:${notice.chatId}`) !== notice.run) {
        this.remove(notice.id); this.clearUnread(notice.chatId, notice.id); continue;
      }
      if (expected === "idle" && asyncChildren(session.fields).live) {
        this.remove(notice.id); this.clearUnread(notice.chatId, notice.id); continue;
      }
      const decision = notificationDecision(notice, this.settings(), this.get<Activity[]>("activity") ?? [], Date.now());
      if (decision === "drop") {
        this.remove(notice.id);
        // Alert expiry is not "read". Delivered/unread badges survive the
        // short APNs retry window; pruning handles disabled/muted/deleted rows.
        if (iosViewingChat(notice.chatId, this.get<Activity[]>("activity") ?? [], Date.now())) {
          this.clearUnread(notice.chatId, notice.id);
        }
        continue;
      }
      if (decision === "defer") { this.put(notice, now + 15_000); continue; }
      const target = this.target(notice.chatId);
      const recipients = target?.platform === "ios"
        ? this.recipients().filter(r => r.installationId === target.clientId)
        : target?.platform === "desktop" && now - target.at > ACTIVITY_LEASE_MS
          ? this.recipients()
          : [];
      if (!recipients.length) {
        if (target?.platform === "desktop" && now - target.at <= ACTIVITY_LEASE_MS) {
          this.put(notice, now + 15_000);
        } else {
          this.remove(notice.id);
        }
        continue;
      }
      let retry = false;
      for (const recipient of recipients) {
        // A read may arrive while the previous recipient/event is awaiting
        // APNs. Never send a retired event to another device, or resurrect it
        // for retry. A request already accepted by APNs cannot be recalled.
        if (!this.events().some(e => e.notice.id === notice.id)) break;
        if (notificationDecision(notice, this.settings(), this.get<Activity[]>("activity") ?? [], Date.now()) === "drop") {
          this.remove(notice.id);
          if (iosViewingChat(notice.chatId, this.get<Activity[]>("activity") ?? [], Date.now())) {
            this.clearUnread(notice.chatId, notice.id);
          }
          break;
        }
        // Logout/rotation removes the old local receipt. Cross-account
        // rebinding is additionally checked by the global token DO.
        if (!this.recipients().some(r => r.id === recipient.id && r.lease === recipient.lease)) continue;
        try {
          const response = await this.deviceCall(recipient, "/send", { message: {
            id: notice.id, scope: this.scope(), chatId: notice.chatId, projectId: notice.projectId,
            kind: notice.kind, expires: notice.expires, ...this.badge()
          } });
          const outcome = await response.json() as { sent?: boolean; permanent?: boolean };
          if (!outcome.sent && !outcome.permanent) retry = true;
          if (outcome.permanent) this.set("recipients", this.recipients().filter(r => r.id !== recipient.id || r.lease !== recipient.lease));
        } catch { retry = true; }
      }
      // A status transition may have superseded this notice while awaiting APNs.
      if (!this.events().some(e => e.notice.id === notice.id)) continue;
      if (retry && notice.attempt < 5) {
        notice.attempt += 1;
        this.put(notice, now + Math.min(120_000, 5_000 * 2 ** notice.attempt));
      } else this.remove(notice.id);
    }
    await this.flushBadge();
  }

  private async flushBadge(): Promise<void> {
    const job = this.get<BadgeJob>("badgeJob"), now = Date.now();
    if (!job || job.due > now) return;
    if (job.expires <= now) { this.set("badgeJob", null); return; }
    let retry = false;
    // Badge-only updates reach every registration in the account, including
    // background phones that did not receive the session's targeted alert.
    // Together with 32 alert calls above, this stays below 50 subrequests.
    for (const recipient of this.recipients().slice(0, 16)) {
      if (this.get<BadgeJob>("badgeJob")?.id !== job.id) return;
      if (!this.recipients().some(r => r.id === recipient.id && r.lease === recipient.lease)) continue;
      try {
        const response = await this.deviceCall(recipient, "/send", { message: {
          id: job.id, scope: this.scope(), kind: "badge", expires: job.expires,
          badgeCount: job.badgeCount, badgeRevision: job.badgeRevision
        } });
        const outcome = await response.json() as { sent?: boolean; permanent?: boolean };
        if (!outcome.sent && !outcome.permanent) retry = true;
        if (outcome.permanent) this.set("recipients", this.recipients().filter(r => r.id !== recipient.id || r.lease !== recipient.lease));
      } catch { retry = true; }
    }
    if (this.get<BadgeJob>("badgeJob")?.id !== job.id) return;
    if (retry && job.attempt < 5) {
      this.set("badgeJob", { ...job, attempt: job.attempt + 1,
        due: now + Math.min(120_000, 5_000 * 2 ** (job.attempt + 1)) });
    } else {
      this.set("badgeJob", null);
    }
  }
}

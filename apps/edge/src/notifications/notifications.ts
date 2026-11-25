import type { Env } from "../env";
import type { Row } from "../registry/registry-core";
import { notificationsAvailable } from "./apns";
import type { BadgeSnapshot } from "./apns";
import {
  defaultNotificationSettings, identifier, object, parseActivity, parseSettings,
  readNotificationJSON, notificationJSON as json, notificationDecision, iosViewingChat,
  chatAttention, narrower, attends, type ChatAttention, botIdOf, askingThreadOf, needsOwner, needsStamp,
  NEEDS_RECORD_KINDS, INPUT_EXPIRY_MS, type NeedsItem, questionOf, askingStatus, startedByOf, MAX_CHILD_DEPTH,
  NOTICE_DELAY_MS, SHORT_RUN_MS, ACTIVITY_LEASE_MS, SEEN_SLACK_MS, type Activity, type Notice, type NoticeKind, type NotificationSettings
} from "./notifications-model";

interface Recipient { id: string; lease: string; installationId: string; epoch: number }
interface SessionTarget { clientId: string; platform: "desktop" | "ios"; at: number }
interface BadgeJob extends BadgeSnapshot { id: string; due: number; expires: number; attempt: number }
type UnreadEvent = Pick<Notice, "id" | "chatId" | "projectId" | "kind" | "child" | "eventAt" | "workspace" | "item" | "from">;

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
  /** Whose events this chat's are ([`chatAttention`]): a subagent chat is
   * no more visible than the session it belongs to. */
  private attention(chat: Row): ChatAttention {
    const own = chatAttention(chat.fields);
    const child = chat.fields.child;
    const parentId = child && typeof child === "object" && !Array.isArray(child) ? child.parentChatId : undefined;
    const parent = typeof parentId === "string" ? this.row("chats", parentId) : undefined;
    return parent && !parent.deleted ? narrower(own, chatAttention(parent.fields)) : own;
  }
  /** A workspace's session counts only while it asks, as on the desktop: once
   * it moves on, its question was answered wherever the owner answered it
   * (usually the workspace, which never opens the session itself). */
  private settle(chatId: string): void {
    for (const { notice } of this.events()) if (notice.chatId === chatId) this.remove(notice.id);
    this.clearUnread(chatId);
  }
  /** False once every status source for the chat has moved past its
   * question; unknown counts as still asking. An open question on the row
   * (`asking`) is asking whatever its status says. */
  private mayBeAsking(chatId: string): boolean {
    const sources = [this.get<Row["fields"]>(`eventState:${chatId}`), this.row("sessions", chatId)?.fields]
      .filter((fields): fields is Row["fields"] => !!fields && typeof fields.status === "string");
    return !sources.length ||
      sources.some(fields => askingStatus(fields.status, questionOf(fields.asking)) === "awaitingInput");
  }
  private parentChat(chat: Row): Row | undefined {
    const child = chat.fields.child;
    const parentId = child && typeof child === "object" && !Array.isArray(child) ? child.parentChatId : undefined;
    const parent = typeof parentId === "string" ? this.row("chats", parentId) : undefined;
    return parent && !parent.deleted ? parent : undefined;
  }
  /** A plan's question (docs/design/plan-asks.md): `chat` is a session one
   * of a workspace's plans started, or a subagent's chat under one, and that
   * workspace stands. It waits in the workspace's Needs you, as the plan's
   * session: the item names that session whichever chat under it asked.
   * Without a standing workspace the session notifies as itself, as before. */
  private planQuestion(chat: Row): { botId: string; item: NeedsItem } | undefined {
    let at: Row | undefined = chat;
    for (let depth = 0; at && depth <= MAX_CHILD_DEPTH; depth++) {
      if (at.fields.archived) return undefined;
      const botId = startedByOf(at.fields);
      if (botId) return this.workspaceOf(botId) ? { botId, item: { kind: "asking", id: at.id } } : undefined;
      at = this.parentChat(at);
    }
    return undefined;
  }
  /** Is `chat`'s question mirrored into a chat above it (one whose question's
   * `fromChatId` names it)? That chat reports it, so it pushes once. */
  private mirrored(chat: Row): boolean {
    let at = this.parentChat(chat);
    for (let depth = 0; at && depth < MAX_CHILD_DEPTH; depth++) {
      const above = [this.get<Row["fields"]>(`eventState:${at.id}`)?.asking, this.row("sessions", at.id)?.fields.asking];
      if (above.some(value => questionOf(value)?.fromChatId === chat.id)) return true;
      at = this.parentChat(at);
    }
    return false;
  }
  /** A workspace's `bots` row and its own chat, while both stand. */
  private workspaceOf(botId: string | undefined): { bot: Row; chat: Row } | undefined {
    const bot = botId ? this.row("bots", botId) : undefined;
    const chatId = bot?.fields.chatId;
    const chat = typeof chatId === "string" ? this.row("chats", chatId) : undefined;
    if (!bot || bot.deleted || !chat || chat.deleted || chat.fields.archived) return undefined;
    return { bot, chat };
  }
  /** Where a chat's own notices are filed, for muting and the payload: its
   * project, or -- for a workspace's own chat, which has none -- the
   * workspace (`projectId` = bot id, `workspace: true`). Any other chat
   * without a live project never notifies. */
  private placeOf(chat: Row): { projectId: string; workspace: boolean } | undefined {
    const botId = botIdOf(chat.fields);
    if (botId && chatAttention(chat.fields) === "all") {
      return this.workspaceOf(botId)?.chat.id === chat.id ? { projectId: botId, workspace: true } : undefined;
    }
    const spaceId = chat.fields.spaceId;
    const space = typeof spaceId === "string" ? this.row("spaces", spaceId) : undefined;
    return typeof spaceId === "string" && space && !space.deleted ? { projectId: spaceId, workspace: false } : undefined;
  }
  private projectAlive(notice: Pick<Notice, "projectId" | "workspace">): boolean {
    const row = this.row(notice.workspace ? "bots" : "spaces", notice.projectId);
    return !!row && !row.deleted;
  }
  /** Does a Needs you item still wait on the owner of workspace `botId`?
   * `from`: for a plan's question, the subagent's chat that asked it. */
  private itemPending(item: NeedsItem, botId: string, from?: string): boolean {
    if (item.kind === "asking") {
      const thread = this.row("chats", item.id);
      if (!thread || thread.deleted || thread.fields.archived) return false;
      if (askingThreadOf(thread.fields) === botId) return this.mayBeAsking(item.id);
      // A plan's session: its own question, or the one of the chat under it
      // that asked.
      if (startedByOf(thread.fields) !== botId) return false;
      if (this.mayBeAsking(item.id)) return true;
      const asker = from && from !== item.id ? this.row("chats", from) : undefined;
      return !!asker && !asker.deleted && !asker.fields.archived && this.mayBeAsking(asker.id);
    }
    const rowKind = Object.keys(NEEDS_RECORD_KINDS).find(kind => NEEDS_RECORD_KINDS[kind] === item.kind);
    const record = rowKind ? this.row(rowKind, item.id) : undefined;
    return !!rowKind && !!record && !record.deleted && record.fields.botId === botId && needsOwner(rowKind, record.fields);
  }
  /** The item was decided (or its thread moved on): withdraw its alert, and
   * the workspace chat's unread mark when that mark is the item's -- as a
   * workspace's session counts only while it asks. */
  private settleItem(item: NeedsItem): void {
    for (const { notice } of this.events()) {
      if (notice.item?.kind === item.kind && notice.item.id === item.id) this.remove(notice.id);
    }
    for (const unread of this.unread()) {
      if (unread.item?.kind === item.kind && unread.item.id === item.id) this.clearUnread(unread.chatId, unread.id);
    }
  }
  /** A plan's chat moved on: withdraw the alert and unread mark of its
   * question unless its session (or the chat under it that asked) still
   * asks -- one of several chats under the session settling is not the
   * session's question being answered. */
  private retireAnswered(item: NeedsItem): void {
    const same = (other: NeedsItem | undefined) => other?.kind === item.kind && other.id === item.id;
    for (const { notice } of this.events()) {
      if (same(notice.item) && !this.itemPending(item, notice.projectId, notice.from)) this.remove(notice.id);
    }
    for (const unread of this.unread()) {
      if (same(unread.item) && !this.itemPending(item, unread.projectId, unread.from)) {
        this.clearUnread(unread.chatId, unread.id);
      }
    }
  }
  /** A record row (plan, approval, review) changed. It is announced once,
   * when it starts waiting on the owner: on its creation by the workspace's
   * host, or on its host moving it into that state. Every later sync of the
   * unchanged item is not news; the claim key survives until it is decided,
   * so even a re-created row stays quiet. */
  private observeItem(before: Row | undefined, after: Row, sourceDevice: string): void {
    const kind = NEEDS_RECORD_KINDS[after.kind];
    if (!kind) return;
    const item: NeedsItem = { kind, id: after.id }, claim = `needs:${kind}:${after.id}`;
    const was = !!before && !before.deleted && needsOwner(after.kind, before.fields);
    if (after.deleted || !needsOwner(after.kind, after.fields)) {
      // Decided, expired or pruned. One that was not waiting has nothing left.
      if (was) {
        this.settleItem(item);
        this.ctx.storage.sql.exec("DELETE FROM notify_kv WHERE key=?", claim);
      }
      return;
    }
    if (was) return;
    const botId = after.fields.botId;
    const workspace = typeof botId === "string" ? this.workspaceOf(botId) : undefined;
    if (!workspace || workspace.bot.fields.hostDeviceId !== sourceDevice) return;
    const now = Date.now();
    if (!before || before.deleted) {
      // A row first seen long after it was made is a baseline, not news.
      const stamp = needsStamp(after.kind, after.fields);
      if (stamp === undefined || now - stamp > INPUT_EXPIRY_MS) return;
    }
    if (!this.recipients().length || this.get<number>(claim) !== undefined) return;
    this.set(claim, now);
    // Records carry no transition stamp: the Worker's clock is when it was
    // first seen waiting.
    this.queueItem(workspace.chat, botId as string, item, now);
  }
  /** One `input` notice for a Needs you item, filed under its workspace's own
   * chat (where the owner answers it) with the item named for the app. */
  private queueItem(
    chat: Row, botId: string, item: NeedsItem, eventAt: number, from?: string
  ): ReturnType<Notifications["enqueue"]> | undefined {
    const recipients = this.recipients().map(({ id, lease }) => ({ id, lease }));
    if (!recipients.length) return undefined;
    const now = Date.now();
    return this.enqueue({
      id: crypto.randomUUID(), chatId: chat.id, projectId: botId, kind: "input", child: false,
      run: `${item.kind}/${item.id}`, at: now, expires: now + INPUT_EXPIRY_MS, recipients, attempt: 0,
      eventAt, workspace: true, item, ...(from && from !== item.id ? { from } : {})
    });
  }
  /** Make `notice` its chat's one pending alert and unread mark, replacing
   * superseded state rather than accumulating alerts during flapping. A
   * workspace's turn usually ends as it proposes a plan or asks for an
   * approval: the item is the news, so a "completed" never displaces it. */
  private enqueue(notice: Notice): "queued" | "read" | "superseded" {
    const pending = this.events().filter(e => e.notice.chatId === notice.chatId);
    if (notice.kind === "completed" && pending.some(e => e.notice.item)) return "superseded";
    for (const e of pending) this.remove(e.notice.id);
    // An event arriving while the session is already visible is read too.
    // Remember the dedupe marker even if the viewer leaves before flush.
    // Already opened on another device after the host stamped the event
    // (the desktop marks a session it is showing seen immediately).
    if (iosViewingChat(notice.chatId, this.get<Activity[]>("activity") ?? [], notice.at) ||
        this.seenSince(notice.chatId, notice.eventAt)) {
      this.clearUnread(notice.chatId);
      return "read";
    }
    const queued = this.events();
    if (queued.length >= 256 && queued[0]) this.remove(queued[0].notice.id);
    this.put(notice, notice.at + NOTICE_DELAY_MS);
    this.markUnread(notice);
    return "queued";
  }
  private recipients(): Recipient[] { return this.get<Recipient[]>("recipients") ?? []; }
  private settings(): NotificationSettings { return this.get<NotificationSettings>("settings") ?? defaultNotificationSettings(); }
  private target(chatId: string): SessionTarget | undefined { return this.get<SessionTarget>(`target:${chatId}`); }
  /** The session went (back) to work at host time `at`: any question asked
   * before it has been answered. */
  private resumed(chatId: string, at: number): void {
    if (at > (this.get<number>(`resumedAt:${chatId}`) ?? 0)) this.set(`resumedAt:${chatId}`, at);
  }
  /** Claim the right to announce `run/kind`, once. Both the host event and
   * the registry mirror report the same transition, and either may replay.
   *
   * One run completes or fails once, but it can ask several questions: the
   * run id survives `awaitingInput → working`. A question is new only when a
   * resume newer than the last announced question was seen and it was asked
   * after that resume. Every stamp is the host's session clock, so a replayed
   * or late mirror of an answered question stays a duplicate.
   *
   * A host that names its questions (`question`, the row's
   * `asking.requestId`) makes each question its own claim: a new one is
   * news even while the status stays awaiting input, the same one never. */
  private claim(chatId: string, run: string, kind: NoticeKind, at: number, question?: string): boolean {
    const key = question ? `${run}/${kind}/${question}` : `${run}/${kind}`;
    if (this.get<string>(`enqueued:${chatId}`) === key) {
      if (question) return false;
      const asked = this.get<number>(`enqueuedAt:${chatId}`) ?? 0;
      const resumed = this.get<number>(`resumedAt:${chatId}`) ?? 0;
      if (kind !== "input" || resumed <= asked || at <= resumed) return false;
    }
    this.set(`enqueued:${chatId}`, key);
    this.set(`enqueuedAt:${chatId}`, at);
    return true;
  }

  /** Apply one activity report, whoever carried it.
   *
   * The periodic refresh rides the registry room's presence frame, which is
   * already sent every 15s on an open socket and costs 1/20 of a request; the
   * identical report over HTTP cost a whole one. Both paths land here so the
   * stored state cannot diverge by transport — only the reply differs, and a
   * refresh has no reply to give.
   *
   * `undefined` = the report was a duplicate or arrived out of order and
   * changed nothing; it must not replay a historical read, refresh a lease or
   * route a new event to an old page. */
  applyActivity(body: Record<string, unknown>): string[] | undefined {
    if (!notificationsAvailable(this.env)) return undefined;
    const now = Date.now(), id = identifier(body.clientId);
    const current = this.get<Activity[]>("activity") ?? [];
    const previous = current.find(a => a.clientId === id);
    const activity = parseActivity(body, previous, now);
    if (activity === previous) return undefined;
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
    return readEventIds;
  }
  private scope(): string { return this.ctx.id.toString(); }
  private badge(): BadgeSnapshot {
    return {
      badgeCount: Number(this.ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM notify_unread").one().count),
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
      kind: notice.kind, child: notice.child, eventAt: notice.eventAt,
      ...(notice.workspace ? { workspace: true } : {}), ...(notice.item ? { item: notice.item } : {}),
      ...(notice.from ? { from: notice.from } : {}) };
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
  /** Has the chat been opened on ANY device since the host stamped
   * `eventAt`? Every client writes the synced `lastSeenAt` marker when it
   * shows a session (desktop and iOS alike), so this is "read elsewhere". */
  private seenSince(chatId: string, eventAt: number | undefined): boolean {
    const seen = this.row("chats", chatId)?.fields.lastSeenAt;
    return typeof seen === "number" && typeof eventAt === "number" && seen >= eventAt - SEEN_SLACK_MS;
  }
  /** A chat's seen marker advanced: the session was opened on some device.
   * Everything it had pending up to that moment is read — the unread badge
   * contribution and any alert still waiting out its delay — so every
   * phone's badge follows a read on the desktop (and vice versa via the
   * marker itself). Events newer than the marker stay. */
  private observeSeen(before: Row | undefined, after: Row): void {
    const seen = after.fields.lastSeenAt, previous = before?.fields.lastSeenAt;
    if (after.deleted || typeof seen !== "number" || (typeof previous === "number" && seen <= previous)) return;
    const chatId = after.id;
    for (const { notice } of this.events()) {
      if (notice.chatId === chatId && seen >= (notice.eventAt ?? notice.at) - SEEN_SLACK_MS) this.remove(notice.id);
    }
    for (const unread of this.unread()) {
      if (unread.chatId === chatId || this.parentOf(unread) === chatId) this.clearIfRead(unread);
    }
  }
  private unread(): UnreadEvent[] {
    return [...this.ctx.storage.sql.exec("SELECT value FROM notify_unread")]
      .map(row => JSON.parse(row.value as string) as UnreadEvent);
  }
  private parentOf(unread: UnreadEvent): string | undefined {
    if (!unread.child) return;
    const child = this.row("chats", unread.chatId)?.fields.child;
    const parent = child && typeof child === "object" && !Array.isArray(child) ? child.parentChatId : undefined;
    return typeof parent === "string" ? parent : undefined;
  }
  /** Did any device's seen marker cover this unread event? The chat's own
   * marker, or — for a subagent chat, which Home never lists — its parent's:
   * the parent session is where that result is read. */
  private clearIfRead(unread: UnreadEvent): void {
    const parent = this.parentOf(unread);
    const markers = [this.row("chats", unread.chatId), parent ? this.row("chats", parent) : undefined];
    const lastMessageAt = markers[0]?.fields.lastMessageAt;
    const read = markers.some(chat => {
      const seen = chat?.fields.lastSeenAt;
      if (!chat || chat.deleted || typeof seen !== "number") return false;
      // Unread rows from before `eventAt` existed: fall back to the marker
      // covering the chat's latest message, the sidebar's own seen rule.
      return typeof unread.eventAt === "number"
        ? seen >= unread.eventAt - SEEN_SLACK_MS
        : typeof lastMessageAt !== "number" || seen >= lastMessageAt;
    });
    if (read) this.clearUnread(unread.chatId, unread.id);
  }
  private pruneUnread(): void {
    const prefs = this.settings();
    for (const notice of this.unread()) {
      const chat = this.row("chats", notice.chatId);
      if (!chat || chat.deleted || chat.fields.archived || !this.projectAlive(notice) ||
          !attends(this.attention(chat), notice.kind) ||
          (this.attention(chat) === "input" && !this.mayBeAsking(notice.chatId)) ||
          (notice.item && !this.itemPending(notice.item, notice.projectId, notice.from)) ||
          !prefs[notice.kind] || prefs.mutedProjects.includes(notice.projectId) ||
          (notice.child && notice.kind !== "input" && !prefs.subagents)) {
        this.clearUnread(notice.chatId);
      } else {
        // Rows written before seen markers reached the Worker (and child rows
        // before a parent's read counted) would otherwise stay unread forever:
        // the marker already moved, so no later write will retire them.
        this.clearIfRead(notice);
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
        if (!notificationsAvailable(this.env)) return json({ ok: true, available: false });
        const readEventIds = this.applyActivity(body);
        if (readEventIds === undefined) {
          return json({ ok: true, available: true, scope: this.scope(), readEventIds: [], ...this.badge() });
        }
        return json({ ok: true, available: true, scope: this.scope(), readEventIds, ...this.badge() });
      }
      if (path === "event" && request.method === "POST") {
        const chatId = identifier(body.chatId), deviceId = identifier(body.deviceId);
        const reported = String(body.status);
        // The open question (plan-asks.md §6), absent on old hosts: while one
        // is open the chat awaits input, whatever its status says.
        const question = questionOf(body.asking);
        const status = String(askingStatus(reported, question));
        if (!["idle", "working", "awaitingInput", "errored"].includes(reported) ||
            typeof body.updatedAt !== "number" || !Number.isSafeInteger(body.updatedAt) ||
            Math.abs(Date.now() - body.updatedAt) > 300_000) throw new Error();
        const chat = this.row("chats", chatId);
        const attention = chat ? this.attention(chat) : "none";
        // A hidden plan or routine thread is not the owner's to open, but its
        // question is theirs: it waits in its workspace's Needs you.
        const asking = chat && attention === "none" ? askingThreadOf(chat.fields) : undefined;
        const place = chat && attention !== "none" ? this.placeOf(chat) : undefined;
        // A plan's session (or a subagent's chat under one) asks through its
        // workspace's Needs you too.
        const plan = chat && attention !== "none" ? this.planQuestion(chat) : undefined;
        if (!chat || chat.deleted || chat.fields.archived || chat.fields.deviceId !== deviceId ||
            (attention !== "none" && !place) || (asking && !this.workspaceOf(asking))) {
          // Name the rule that fired, in the order above: it lets a host tell a
          // registry row that has not arrived yet from a chat that will never
          // notify, and makes an ignored chat diagnosable from the host's log.
          // `space`: neither a live project nor (for a workspace's own chat
          // or thread) a live workspace.
          const reason = !chat || chat.deleted ? "chat"
            : chat.fields.archived ? "archived"
            : chat.fields.deviceId !== deviceId ? "device"
            : "space";
          return json({ ok: true, ignored: true, reason });
        }
        // Other hidden threads and memory passes never reach the owner. Their
        // markers are written at birth and never change, so the host may
        // back off as for any other ignored chat.
        if (attention === "none" && !asking) return json({ ok: true, ignored: true, reason: "hidden" });
        const subagents = (Array.isArray(body.subagents) ? body.subagents : []).slice(0, 32).map(value => {
          const run = object(value);
          if (!["async", "sync", "message"].includes(String(run.mode)) ||
              !["running", "done", "error"].includes(String(run.status)) ||
              typeof run.updatedAt !== "number") throw new Error();
          return { mode: String(run.mode), status: String(run.status), updatedAt: run.updatedAt };
        });
        const eventFields = { status: reported, subagents, asking: question ?? null } as Row["fields"];
        const last = this.get<Row["fields"]>(`eventState:${chatId}`);
        if (last && typeof last.updatedAt === "number" && last.updatedAt >= body.updatedAt) {
          return json({ ok: true, stale: true });
        }
        this.set(`eventState:${chatId}`, { ...eventFields, updatedAt: body.updatedAt });
        const previous = this.get<string>(`eventStatus:${chatId}`);
        const children = asyncChildren(eventFields);
        const childrenSettled = status === "idle" && previous === "idle" &&
          asyncChildren(last).live && !children.live;
        // A new question is news even while the status stays awaiting input
        // (a subagent asks while the session's own prompt is open).
        const newQuestion = !!question && question.requestId !== questionOf(last?.asking)?.requestId;
        if (previous === status && !childrenSettled && !newQuestion) return json({ ok: true, duplicate: true });
        this.set(`eventStatus:${chatId}`, status);
        if (status === "working") {
          this.resumed(chatId, body.updatedAt);
          this.set(`run:${chatId}`, typeof body.startedAt === "number" ? String(body.startedAt) : crypto.randomUUID());
          if (attention === "input") { this.settle(chatId); this.schedule(); }
          if (asking) { this.settleItem({ kind: "asking", id: chatId }); this.schedule(); }
          if (plan) { this.retireAnswered(plan.item); this.schedule(); }
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
        if (asking) {
          const item: NeedsItem = { kind: "asking", id: chatId };
          if (kind !== "input") { this.settleItem(item); this.schedule(); return json({ ok: true, hidden: true }); }
          const run = this.get<string>(`run:${chatId}`) ?? crypto.randomUUID();
          if (!this.recipients().length) return json({ ok: true, noRecipients: true });
          // The thread's own dedupe state: each question of a run, once.
          if (!this.claim(chatId, run, kind, body.updatedAt, question?.requestId)) return json({ ok: true, duplicate: true });
          const outcome = this.queueItem(this.workspaceOf(asking)!.chat, asking, item, body.updatedAt);
          this.schedule();
          return json({ ok: true, [outcome ?? "noRecipients"]: true });
        }
        if (plan) {
          if (kind !== "input") { this.retireAnswered(plan.item); this.schedule(); return json({ ok: true, hidden: true }); }
          const run = this.get<string>(`run:${chatId}`) ?? crypto.randomUUID();
          if (!this.recipients().length) return json({ ok: true, noRecipients: true });
          // Mirrored into the chat above it: that chat's report is the one.
          if (this.mirrored(chat)) return json({ ok: true, mirrored: true });
          if (!this.claim(chatId, run, kind, body.updatedAt, question?.requestId)) {
            return json({ ok: true, duplicate: true });
          }
          const outcome = this.queueItem(this.workspaceOf(plan.botId)!.chat, plan.botId, plan.item, body.updatedAt, chatId);
          this.schedule();
          return json({ ok: true, [outcome ?? "noRecipients"]: true });
        }
        if (!attends(attention, kind)) { this.settle(chatId); this.schedule(); return json({ ok: true, hidden: true }); }
        const run = this.get<string>(`run:${chatId}`) ?? crypto.randomUUID();
        const startedAt = typeof body.startedAt === "number" ? body.startedAt : Number(run);
        if (kind === "completed" && Number.isFinite(startedAt) && Date.now() - startedAt < SHORT_RUN_MS) {
          return json({ ok: true, short: true });
        }
        const recipients = this.recipients().map(({ id, lease }) => ({ id, lease }));
        if (!recipients.length) return json({ ok: true, noRecipients: true });
        if (kind === "input" && this.mirrored(chat)) return json({ ok: true, mirrored: true });
        const notice: Notice = {
          id: crypto.randomUUID(), chatId, projectId: place!.projectId, kind,
          child: !!chat.fields.child, run, at: Date.now(),
          expires: Date.now() + (kind === "input" ? INPUT_EXPIRY_MS : 600_000),
          recipients, attempt: 0, sessionStatus: status, source: "event", eventAt: body.updatedAt,
          ...(place!.workspace ? { workspace: true } : {})
        };
        if (!this.claim(chatId, run, kind, body.updatedAt, kind === "input" ? question?.requestId : undefined)) {
          return json({ ok: true, duplicate: true });
        }
        // A subagent's question mirrored here: this is its one alert, so the
        // child's own (if it was reported first) is withdrawn.
        if (kind === "input" && question?.fromChatId) this.settle(question.fromChatId);
        const outcome = this.enqueue(notice);
        this.schedule();
        return json({ ok: true, [outcome]: true });
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
      // Any device may move the seen marker; it reads, never notifies.
      if (after.kind === "chats") { this.observeSeen(before, after); continue; }
      if (NEEDS_RECORD_KINDS[after.kind]) { this.observeItem(before, after, sourceDevice); continue; }
      if (after.kind !== "sessions" || after.deleted || after.fields.deviceId !== sourceDevice) continue;
      const chatId = after.fields.chatId;
      if (typeof chatId !== "string") continue;
      // An open question (`asking`) awaits input whatever the status says.
      const question = questionOf(after.fields.asking);
      const status = askingStatus(after.fields.status, question);
      const previousStatus = before ? askingStatus(before.fields.status, questionOf(before.fields.asking)) : undefined;
      const newQuestion = !!question && question.requestId !== questionOf(before?.fields.asking)?.requestId;
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
      if (!before || before.deleted || (previousStatus === status && !childrenSettled && !newQuestion) ||
          typeof eventAt !== "number" || Math.abs(now - eventAt) > 300_000) continue;
      if (status === "working") {
        this.resumed(chatId, eventAt);
        const chat = this.row("chats", chatId);
        if (chat && this.attention(chat) === "input") this.settle(chatId);
        if (chat && this.attention(chat) === "none" && askingThreadOf(chat.fields)) {
          this.settleItem({ kind: "asking", id: chatId });
        }
        const plan = chat && !chat.deleted && this.attention(chat) !== "none" ? this.planQuestion(chat) : undefined;
        if (plan) this.retireAnswered(plan.item);
        continue;
      }
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
      if (!chat || chat.deleted || chat.fields.archived || chat.fields.deviceId !== after.fields.deviceId) continue;
      const attention = this.attention(chat);
      const asking = attention === "none" ? askingThreadOf(chat.fields) : undefined;
      if (asking) {
        const item: NeedsItem = { kind: "asking", id: chatId };
        const workspace = this.workspaceOf(asking);
        if (kind !== "input") { this.settleItem(item); continue; }
        if (!workspace) continue;
        if (!run) { run = crypto.randomUUID(); this.set(key, run); }
        if (!this.recipients().length || !this.claim(chatId, run, kind, eventAt, question?.requestId)) continue;
        this.queueItem(workspace.chat, asking, item, eventAt);
        continue;
      }
      const plan = attention !== "none" ? this.planQuestion(chat) : undefined;
      if (plan) {
        if (kind !== "input") { this.retireAnswered(plan.item); continue; }
        if (!run) { run = crypto.randomUUID(); this.set(key, run); }
        // Mirrored into the chat above it: that chat's row is the one.
        if (!this.recipients().length || this.mirrored(chat) ||
            !this.claim(chatId, run, kind, eventAt, question?.requestId)) continue;
        this.queueItem(this.workspaceOf(plan.botId)!.chat, plan.botId, plan.item, eventAt, chatId);
        continue;
      }
      if (!attends(attention, kind)) { this.settle(chatId); continue; }
      const place = this.placeOf(chat);
      if (!place) continue;
      if (!run) { run = crypto.randomUUID(); this.set(key, run); }
      const recipients = this.recipients().map(({ id, lease }) => ({ id, lease }));
      if (!recipients.length) continue;
      if (kind === "input" && this.mirrored(chat)) continue;
      if (!this.claim(chatId, run, kind, eventAt, kind === "input" ? question?.requestId : undefined)) continue;
      if (kind === "input" && question?.fromChatId) this.settle(question.fromChatId);
      this.enqueue({
        id: crypto.randomUUID(), chatId, projectId: place.projectId, kind,
        child: !!chat.fields.child, run, at: now, expires: now + (kind === "input" ? INPUT_EXPIRY_MS : 600_000),
        recipients, attempt: 0, sessionStatus: String(status), eventAt,
        ...(place.workspace ? { workspace: true } : {})
      });
    }
    this.pruneUnread();
    this.schedule();
  }

  /** Is a session notice's chat still in the state it announced, in the
   * same run? */
  private sessionStillAt(notice: Notice): boolean {
    const session = notice.source === "event"
      ? (() => {
        const state = this.get<Row["fields"]>(`eventState:${notice.chatId}`);
        return state ? { fields: state, deleted: false } : undefined;
      })()
      : this.row("sessions", notice.chatId);
    const expected = notice.sessionStatus ?? { completed: "idle", failed: "errored", input: "awaitingInput" }[notice.kind];
    return !!session && !session.deleted &&
      askingStatus(session.fields.status, questionOf(session.fields.asking)) === expected &&
      this.get<string>(`run:${notice.chatId}`) === notice.run &&
      !(expected === "idle" && asyncChildren(session.fields).live);
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
      const chat = this.row("chats", notice.chatId);
      if (!chat || chat.deleted || chat.fields.archived || !this.projectAlive(notice) ||
          !attends(this.attention(chat), notice.kind) ||
          (notice.item ? !this.itemPending(notice.item, notice.projectId, notice.from) : !this.sessionStillAt(notice))) {
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
      // Only the iPhone reports a workspace's own chat as the one it is
      // showing (its workspace screen does; the desktop's does not), so the
      // chat has a target only once a phone has opened it. Without one its
      // notices go to every phone, as after a desktop target lapses.
      const recipients = target?.platform === "ios"
        ? this.recipients().filter(r => r.installationId === target.clientId)
        : (target?.platform === "desktop" && now - target.at > ACTIVITY_LEASE_MS) || (!target && notice.workspace)
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
            kind: notice.kind, expires: notice.expires, ...this.badge(),
            ...(notice.workspace ? { workspaceId: notice.projectId } : {}),
            ...(notice.item ? { itemKind: notice.item.kind, itemId: notice.item.id } : {})
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

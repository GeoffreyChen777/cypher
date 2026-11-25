import { describe, expect, it } from "vitest";
import {
  active, askingThreadOf, attends, botIdOf, chatAttention, defaultNotificationSettings, narrower, needsOwner, needsStamp,
  notificationDecision, parseActivity, parseSettings, questionOf, askingStatus, startedByOf, type Activity, type Notice
} from "./notifications-model";
import planAsks from "../../../../protocol/vectors/plan-asks-v1.json";
const now = 1_000_000;
const notice = (kind: Notice["kind"] = "completed"): Notice => ({
  id: crypto.randomUUID(), chatId: "chat", projectId: "project", child: false,
  kind, run: "run", at: now - 10_000, expires: now + 600_000, recipients: [], attempt: 0
});
const activity = (overrides: Partial<Activity> = {}): Activity => ({
  clientId: "desktop", platform: "desktop", foreground: true, interactionAt: now,
  receivedAt: now, openedAt: now - 60_000, chatId: null, sequence: 1, ...overrides
});
describe("single session notification target policy", () => {
  it("always allows eligible events regardless of desktop activity", () => {
    expect(notificationDecision(notice(), defaultNotificationSettings(), [activity()], now)).toBe("send");
    expect(notificationDecision(notice("input"), defaultNotificationSettings(), [activity()], now)).toBe("send");
    expect(active(activity({ interactionAt: now - 120_001 }), now)).toBe(false);
  });
  it.each(["completed", "failed", "input"] as const)("reads %s only while a fresh iOS viewer is on that chat", kind => {
    const prefs = defaultNotificationSettings();
    const viewer = activity({ clientId: "phone", platform: "ios", chatId: "chat" });
    expect(notificationDecision(notice(kind), prefs, [viewer], now)).toBe("drop");
    for (const other of [
      { ...viewer, chatId: null },
      { ...viewer, chatId: "other" },
      { ...viewer, foreground: false },
      { ...viewer, receivedAt: now - 45_001 },
      { ...viewer, platform: "desktop" as const },
    ]) {
      expect(notificationDecision(notice(kind), prefs, [other], now)).toBe("send");
    }
  });
  it("honors mode, event toggles and project muting", () => {
    const prefs = defaultNotificationSettings();
    expect(notificationDecision(notice(), prefs, [activity()], now)).toBe("send");
    expect(notificationDecision(notice("failed"), prefs, [], now)).toBe("send");
    expect(notificationDecision(notice("input"), { ...prefs, input: false }, [], now)).toBe("drop");
    expect(notificationDecision(notice(), { ...prefs, mutedProjects: ["project"] }, [], now)).toBe("drop");
  });
  it("does not independently announce normal child results by default", () => {
    expect(notificationDecision({ ...notice(), child: true }, defaultNotificationSettings(), [], now)).toBe("drop");
    expect(notificationDecision({ ...notice("failed"), child: true }, defaultNotificationSettings(), [], now)).toBe("drop");
    expect(notificationDecision({ ...notice("input"), child: true }, defaultNotificationSettings(), [], now)).toBe("send");
  });
  it("drops expired events and strictly validates settings", () => {
    expect(notificationDecision({ ...notice(), expires: now }, defaultNotificationSettings(), [], now)).toBe("drop");
    expect(() => parseSettings({ ...defaultNotificationSettings(), completed: "yes" })).toThrow();
    expect(() => parseSettings({ ...defaultNotificationSettings(), mutedProjects: ["../x"] })).toThrow();
    expect(parseSettings(defaultNotificationSettings())).toEqual(defaultNotificationSettings());
  });
  it("uses server time rather than trusting a client wall clock", () => {
    const parsed = parseActivity({ clientId: "ui", platform: "desktop", foreground: true,
      sequence: 1, interactionAgeMs: 5000, chatId: "chat", arbitraryClientTimestamp: Number.MAX_VALUE }, undefined, now);
    expect(parsed.interactionAt).toBe(now - 5000);
    expect(parsed.receivedAt).toBe(now);
    const again = parseActivity({ clientId: "ui", platform: "desktop", foreground: true,
      sequence: 2, interactionAgeMs: 15_000, chatId: "chat" }, parsed, now + 10_000);
    expect(again.openedAt).toBe(parsed.openedAt);
    expect(() => parseActivity({ clientId: "ui", platform: "desktop", foreground: true,
      sequence: 3, interactionAgeMs: -1, chatId: null }, undefined, now)).toThrow();
    expect(parseActivity({ clientId: "ui", platform: "desktop", foreground: false,
      sequence: 1, interactionAgeMs: 0, chatId: null }, again, now + 20_000)).toEqual(again);
  });

});

describe("which chats reach the owner (the desktop's Dock badge rule)", () => {
  it("follows the conversations the owner sees, workspace sessions only while they ask", () => {
    expect(chatAttention({ spaceId: "p", bot: null, memory: null, child: null })).toBe("all");
    // The workspace's own chat with its owner.
    expect(chatAttention({ bot: { botId: "b" } })).toBe("all");
    expect(chatAttention({ bot: { botId: "b", origin: "owner", thread: { kind: "main" } } })).toBe("all");
    // A session a workspace started: only its questions.
    expect(chatAttention({ spaceId: "p", startedBy: { botId: "b", workId: "w" } })).toBe("input");
    expect(attends("input", "input")).toBe(true);
    expect(attends("input", "completed")).toBe(false);
    expect(attends("input", "failed")).toBe(false);
  });
  it.each([
    ["work thread", { bot: { botId: "b", thread: { kind: "work", workId: "w" } } }],
    ["routine thread", { bot: { botId: "b", origin: "routine", thread: { kind: "routine", routineId: "r" } } }],
    ["routine session", { spaceId: "p", bot: { botId: "b", origin: "routine", thread: { kind: "routineSession", routineId: "r" } } }],
    ["peer thread", { bot: { botId: "b", origin: "peer", thread: { kind: "peer", connectionId: "c" } } }],
    ["routine-origin main", { bot: { botId: "b", origin: "routine" } }],
    ["thread from a newer engine", { bot: { botId: "b", thread: { kind: "later" } } }],
    ["malformed marker", { bot: "b" }],
    ["memory pass", { spaceId: "p", memory: { kind: "consolidate" } }]
  ])("never counts a %s", (_, fields) => {
    expect(chatAttention(fields)).toBe("none");
  });
  it("a subagent is no more visible than its session", () => {
    expect(narrower("all", "all")).toBe("all");
    expect(narrower("all", "input")).toBe("input");
    expect(narrower("input", "none")).toBe("none");
  });
});

describe("what a workspace waits on (its Needs you card)", () => {
  it("files a chat under the workspace its marker names", () => {
    expect(botIdOf({ bot: { botId: "ws" } })).toBe("ws");
    expect(botIdOf({ bot: { botId: "ws", thread: { kind: "routine", routineId: "r" } } })).toBe("ws");
    expect(botIdOf({ spaceId: "p" })).toBeUndefined();
    expect(botIdOf({ bot: "ws" })).toBeUndefined();
    expect(botIdOf({ bot: { botId: "../x" } })).toBeUndefined();
  });
  it.each([
    ["work thread", { bot: { botId: "ws", thread: { kind: "work", workId: "w" } } }, "ws"],
    ["routine thread", { bot: { botId: "ws", origin: "routine", thread: { kind: "routine", routineId: "r" } } }, "ws"],
    ["routine session", { spaceId: "p", bot: { botId: "ws", origin: "routine", thread: { kind: "routineSession", routineId: "r" } } }, "ws"],
    ["main chat", { bot: { botId: "ws" } }, undefined],
    ["peer thread", { bot: { botId: "ws", origin: "peer", thread: { kind: "peer", connectionId: "c" } } }, undefined],
    ["peer-origin routine thread", { bot: { botId: "ws", origin: "peer", thread: { kind: "routine", routineId: "r" } } }, undefined],
    ["thread from a newer engine", { bot: { botId: "ws", thread: { kind: "later" } } }, undefined],
    ["memory pass", { memory: { kind: "consolidate" }, bot: { botId: "ws", thread: { kind: "routine", routineId: "r" } } }, undefined],
    ["session a workspace started", { spaceId: "p", startedBy: { botId: "ws" }, bot: { botId: "ws", thread: { kind: "work", workId: "w" } } }, undefined]
  ])("a %s asks through Needs you: %s", (_, fields, botId) => {
    expect(askingThreadOf(fields as Record<string, unknown>)).toBe(botId);
  });
  it("reads the record rows as the Rust records do, missing fields as their defaults", () => {
    expect(needsOwner("botWork", { botId: "ws" })).toBe(true);
    expect(needsOwner("botWork", { state: "proposed", decision: null })).toBe(true);
    expect(needsOwner("botWork", { state: "proposed", decision: { approve: true, at: 1 } })).toBe(false);
    expect(needsOwner("botWork", { state: "running" })).toBe(false);
    expect(needsOwner("botApprovals", {})).toBe(true);
    expect(needsOwner("botApprovals", { state: "pending", decision: { approve: false, at: 1 } })).toBe(false);
    expect(needsOwner("botApprovals", { state: "expired" })).toBe(false);
    expect(needsOwner("botExchanges", { direction: "incoming", state: "awaitingReview" })).toBe(true);
    expect(needsOwner("botExchanges", { direction: "incoming", state: "awaitingReview", review: { send: true, at: 1 } })).toBe(false);
    expect(needsOwner("botExchanges", { direction: "outgoing", state: "awaitingReview" })).toBe(false);
    expect(needsOwner("botExchanges", { direction: "incoming", state: "working" })).toBe(false);
    expect(needsOwner("botNotes", {})).toBe(false);
    expect(needsStamp("botWork", { createdAt: 5 })).toBe(5);
    expect(needsStamp("botExchanges", { askedAt: 7, createdAt: 5 })).toBe(7);
    expect(needsStamp("botApprovals", {})).toBeUndefined();
  });
});
describe("a session row's open question (protocol/vectors/plan-asks-v1.json)", () => {
  type VectorRow = { chatId: string; status: string; asking?: { requestId: string; fromChatId?: string } };
  type Vector = { name: string; sessions: VectorRow[]; expect: { indicators: Record<string, string> } };
  const cases = (planAsks as unknown as { cases: Vector[] }).cases;
  it("reads every vector row as the shared rule does: an open question awaits input whatever its status", () => {
    expect(cases.length).toBeGreaterThanOrEqual(15);
    for (const vector of cases) {
      for (const row of vector.sessions) {
        const question = questionOf(row.asking);
        expect(question?.requestId, vector.name).toBe(row.asking?.requestId);
        expect(question?.fromChatId, vector.name).toBe(row.asking?.fromChatId);
        // An open question never ages out (the indicator says so too); a
        // row of an old host keeps its status.
        if (row.asking) {
          expect(askingStatus(row.status, question), vector.name).toBe("awaitingInput");
          expect(vector.expect.indicators[row.chatId] ?? "awaitingInput", vector.name).toBe("awaitingInput");
        } else {
          expect(askingStatus(row.status, question), vector.name).toBe(row.status);
        }
      }
    }
  });
  it("is lenient: a malformed question is no question, never a failure", () => {
    for (const value of [undefined, null, "r1", 3, [], {}, { requestId: "" }, { requestId: 7 }, { askedAt: 1 }]) {
      expect(questionOf(value)).toBeUndefined();
    }
    expect(questionOf({ requestId: "r1", fromChatId: "../x" })).toEqual({ requestId: "r1" });
    expect(startedByOf({ startedBy: { botId: "ws", workId: "w" } })).toBe("ws");
    expect(startedByOf({ startedBy: { botId: "a b" } })).toBeUndefined();
    expect(startedByOf({})).toBeUndefined();
  });
});

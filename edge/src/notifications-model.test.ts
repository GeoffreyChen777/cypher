import { describe, expect, it } from "vitest";
import {
  active, defaultNotificationSettings, notificationDecision, parseActivity, parseSettings,
  type Activity, type Notice
} from "./notifications-model";
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

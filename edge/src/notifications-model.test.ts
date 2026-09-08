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
describe("smart mobile notification policy", () => {
  it("silences the phone during actual desktop activity, not mere engine presence", () => {
    expect(notificationDecision(notice(), defaultNotificationSettings(), [activity()], now)).toBe("drop");
    expect(notificationDecision(notice(), defaultNotificationSettings(), [], now)).toBe("send");
    expect(active(activity({ interactionAt: now - 120_001 }), now)).toBe(false);
    expect(active(activity({ receivedAt: now - 45_001 }), now)).toBe(false);
    expect(active(activity({ foreground: false }), now)).toBe(false);
  });
  it("defers unresolved questions but never holds old completion alerts", () => {
    expect(notificationDecision(notice("input"), defaultNotificationSettings(), [activity()], now)).toBe("defer");
    expect(notificationDecision(notice("failed"), defaultNotificationSettings(), [activity()], now)).toBe("drop");
    expect(notificationDecision(notice("input"), defaultNotificationSettings(), [activity({ foreground: false })], now)).toBe("send");
  });
  it("opening the chat during the delay cancels the alert", () => {
    expect(notificationDecision(notice(), defaultNotificationSettings(),
      [activity({ chatId: "chat", openedAt: now - 5000 })], now)).toBe("drop");
  });
  it("phone in the same chat is silent; a different page may receive an in-app notice", () => {
    expect(notificationDecision(notice(), defaultNotificationSettings(),
      [activity({ platform: "ios", chatId: "chat" })], now)).toBe("drop");
    expect(notificationDecision(notice(), defaultNotificationSettings(),
      [activity({ platform: "ios", chatId: "other" })], now)).toBe("send");
  });
  it("honors mode, event toggles and project muting", () => {
    const prefs = defaultNotificationSettings();
    expect(notificationDecision(notice(), { ...prefs, mode: "always" }, [activity()], now)).toBe("send");
    expect(notificationDecision(notice(), { ...prefs, mode: "actionable" }, [], now)).toBe("drop");
    expect(notificationDecision(notice("failed"), { ...prefs, mode: "off" }, [], now)).toBe("drop");
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

// The demo's workspace chat beyond the board's fixtures: Ember, a workspace
// made an hour ago that hasn't been asked anything (its chat opens on the
// start screen), and Atlas's earlier requests, so its chat shows a plan card
// in every phase — done `work-notes`, failed `work-loro` and stopped
// `work-docs` above the awaiting and running ones.

import Foundation

extension DemoDataset {
    static let emberId = "bot-ember"

    /// Ember and its empty chat.
    func installEmber(now: Int64) {
        let hour: Int64 = 3_600_000
        bots.append(
            Bot(
                id: Self.emberId, name: "Ember", icon: "bot-happy", color: "amber",
                hostDeviceId: "dev-mac", chatId: "chat-ember",
                scope: BotScope(projects: .selected(["space-webapp"])),
                capabilities: BotCapabilities(), createdAt: now - hour))
        chats.append(
            Chat(
                id: "chat-ember", deviceId: "dev-mac", title: "Ember", archived: false,
                cwd: "/Users/dev/.cypher/bots/\(Self.emberId)", branch: nil, checkoutId: nil,
                config: ChatConfig(harness: "pi", model: "demo/pi", reasoning: "high", sandbox: "workspace-write"),
                lastMessagePreview: nil, lastMessageAt: nil, createdAt: now - hour, spaceId: nil,
                lastSeenAt: nil, bot: BotChatMarker(botId: Self.emberId)))
    }

    /// A workspace chat's or thread's transcript (nil for any other chat),
    /// with Atlas's earlier plans in time order.
    static func workspaceChatTranscript(for chatId: String, now: Int64) -> [MessageEntry]? {
        if chatId == "chat-memory-pass" { return memoryPass(now: now) }
        guard let base = workspaceTranscript(for: chatId, now: now) else { return nil }
        guard chatId == "chat-atlas" else { return base }
        return (earlierAtlasPlans(now: now) + base).enumerated()
            .sorted { ($0.element.createdAt, $0.offset) < ($1.element.createdAt, $1.offset) }
            .map(\.element)
    }

    /// cypher's nightly consolidation pass (read-only, like a thread).
    private static func memoryPass(now: Int64) -> [MessageEntry] {
        let start = now - 7 * 3_600_000 - 4 * 60_000
        func tool(_ id: String, _ command: String) -> MessagePart {
            .tool(
                id: id, call: RenderToolCall(tag: "unknown", fields: ["name": "memory", "command": command]),
                isError: false, resolved: true)
        }
        return [
            MessageEntry(
                id: "pass-u", role: .user,
                parts: [.text(id: "pass-u-t", text: "Consolidate cypher's memory: merge duplicates, drop what's stale.")],
                createdAt: start, deviceId: "dev-mac", status: .complete),
            MessageEntry(
                id: "pass-a", role: .assistant,
                parts: [
                    tool("pass-view", "view"), tool("pass-search", "search"), tool("pass-edit", "str_replace"),
                    tool("pass-delete", "delete"),
                    .text(
                        id: "pass-a-t",
                        text: "Merged 3 notes into `build-and-test.md` and removed `old-ci.md`, which described the CI before the move to GitHub Actions."),
                ], createdAt: start + 4 * 60_000, deviceId: "dev-mac", status: .complete),
        ]
    }

    /// The requests behind Atlas's ended plans, each answered with its plan.
    private static func earlierAtlasPlans(now: Int64) -> [MessageEntry] {
        let minute: Int64 = 60_000
        let hour: Int64 = 3_600_000
        let day: Int64 = 86_400_000
        func ask(_ id: String, _ text: String, plan callId: String, reply: String, at: Int64) -> [MessageEntry] {
            [
                MessageEntry(
                    id: "\(id)-u", role: .user, parts: [.text(id: "\(id)-u-t", text: text)], createdAt: at,
                    deviceId: "ios-demo", status: .complete),
                MessageEntry(
                    id: "\(id)-a", role: .assistant,
                    parts: [
                        .tool(
                            id: callId, call: RenderToolCall(tag: "unknown", fields: ["name": "propose_plan"]),
                            isError: false, resolved: true),
                        .text(id: "\(id)-a-t", text: reply),
                    ], createdAt: at + 2 * minute, deviceId: "dev-mac", status: .complete),
            ]
        }
        return ask(
            "atlas-docs", "The onboarding guide still describes the old pairing flow. Can you rewrite it?",
            plan: "call-plan-docs", reply: "Two steps: outline the new flow, then rewrite the guide around it.",
            at: now - 2 * day - hour - 3 * minute)
            + ask(
                "atlas-loro", "Upgrade Loro to 1.6 everywhere, and make sure old snapshots still load.",
                plan: "call-plan-loro", reply: "One bump and one round-trip check. Nothing runs until you approve.",
                at: now - 28 * hour - 3 * minute)
            + ask(
                "atlas-notes", "Draft the 0.9 release notes from everything merged since 0.8.",
                plan: "call-plan-notes", reply: "I'll collect the merged PRs, then write the notes by area.",
                at: now - 4 * hour - 8 * minute)
    }
}

// Sent comments showcase (`-demo -route chat:chat-comments`): a prompt that
// carries two comments on the reply before it (one quote too long for its
// line, one with a line break), and a steer with a comment of its own, so the
// comments over a bubble can be looked at beside a plain and a steer bubble.

import Foundation

extension DemoDataset {
    static let messageCommentsChatId = "chat-comments"

    static func messageCommentsChat(spaceId: String, cwd: String, config: ChatConfig, now: Int64) -> Chat {
        Chat(
            id: messageCommentsChatId, deviceId: "dev-mac", title: "Release notes review",
            archived: false, cwd: cwd, branch: "main", checkoutId: nil,
            config: config, lastMessagePreview: "Both points are tighter now.",
            lastMessageAt: now - 3_300_000, createdAt: now - 3_500_000,
            spaceId: spaceId, lastSeenAt: now - 3_300_000)
    }

    static func messageCommentsTranscript(now: Int64) -> [MessageEntry] {
        [
            MessageEntry(
                id: "u0", role: .user,
                parts: [.text(id: "t0", text: "Draft the release notes for 0.4.8.")],
                createdAt: now - 3_500_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
            MessageEntry(
                id: "a0", role: .assistant, parts: [.text(id: "t0", text: messageCommentsDraft)],
                createdAt: now - 3_480_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            MessageEntry(
                id: "u1", role: .user,
                parts: [.text(id: "t0", text: "Tighten these two before we publish.")],
                createdAt: now - 3_400_000, deviceId: "ios-demo", status: .complete, continuationOf: nil,
                comments: [
                    MessageComment(
                        quote:
                            "Relay links now recover on their own when a host re-joins the room, instead of staying sealed until the app restarts.",
                        comment: "Say what the user saw before: \u{201c}Couldn't load models\u{201d} on the phone."),
                    MessageComment(
                        quote: "Tool calls fold behind\none row past five.",
                        comment: "Mention that the limit is a setting on desktop."),
                ]),
            MessageEntry(
                id: "a1", role: .assistant, parts: [.text(id: "t0", text: messageCommentsRevision)],
                createdAt: now - 3_350_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            MessageEntry(
                id: "u2", role: .user,
                parts: [.text(id: "t0", text: "Keep it to one line.")],
                createdAt: now - 3_340_000, deviceId: "ios-demo", status: .complete, continuationOf: nil,
                isSteer: true,
                comments: [
                    MessageComment(quote: "a setting under Appearance", comment: "改成 Settings › Chat。")
                ]),
            MessageEntry(
                id: "a2", role: .assistant, parts: [.text(id: "t0", text: "Both points are tighter now.")],
                createdAt: now - 3_300_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
        ]
    }

    private static let messageCommentsDraft = """
        **Cypher 0.4.8**

        - Relay links now recover on their own when a host re-joins the room, instead of staying sealed until the app restarts.
        - Tool calls fold behind one row past five.
        - Permission control asks less often for work inside the chat's own checkout.
        """

    private static let messageCommentsRevision = """
        - Phones no longer show \u{201c}Couldn't load models\u{201d} after the Mac re-joins: relay links recover on their own.
        - Long tool runs fold behind one row past five calls (a setting under Appearance on desktop).
        """
}

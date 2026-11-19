// A result notice in full (sessions.rs `render_post_panel`): how the job
// ended and when, what it wrote, and the ways on to its routine and its
// session. Opened from the compact card in the workspace chat; a plan's
// notice opens the plan's own page instead while its record is kept.

import SwiftUI

struct BotPostPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.botOpenChat) private var openChat
    let botId: String
    let entryId: String

    var body: some View {
        if let (from, text) = post, let notice = BotPostNotice(from) {
            page(from, text: text, notice: notice)
        } else {
            BotEmptyState(
                symbol: "tray", title: "Not on this device yet",
                hint: "The result syncs with the workspace's conversation.")
        }
    }

    /// The post in the workspace's conversation.
    private var post: (BotPost, String)? {
        guard let bot = model.bot(id: botId), let chat = model.chat(id: bot.chatId),
            let store = model.sessionStore(for: chat)
        else { return nil }
        return Self.find(entryId, in: store.entries)
    }

    /// sessions.rs `find_post`: a result notice as the chat holds it.
    static func find(_ entryId: String, in entries: [MessageEntry]) -> (BotPost, String)? {
        guard let entry = entries.first(where: { $0.id == entryId }) else { return nil }
        for part in entry.parts {
            if case .post(_, let text, let from) = part { return (from, text) }
        }
        return nil
    }

    /// When it ran or ended, and how long it took.
    private func times(_ from: BotPost) -> [(label: String, value: String)] {
        switch from {
        case .routine(_, _, _, let runAt, _): [("Ran", BotTime.past(runAt))]
        case .work(_, _, _, let startedAt, let endedAt):
            [("Ended", BotTime.past(endedAt))] + (startedAt.map { [("Took", BotTime.span(endedAt - $0))] } ?? [])
        case .unknown: []
        }
    }

    private func page(_ from: BotPost, text: String, notice: BotPostNotice) -> some View {
        BotPageList(title: "Result") {
            // How it ended (when, on a tap), what it was, and what it wrote.
            Section {
                BotItemHeader(
                    status: notice.status, title: singleLine(notice.title), details: times(from),
                    statusIdentifier: "post-status"
                ) {
                    BoardGlyph(symbol: notice.status.tone.symbol ?? "stop.circle", color: notice.status.tone.color)
                }
            }
            .groupedRowStyle()
            Section {
                BotMarkdown(text: text, cacheKey: "post-\(entryId)")
                    .padding(.vertical, 4)
            }
            .groupedRowStyle()
            if case .routine(let routineId, _, _, _, let threadChatId) = from {
                Section {
                    if model.data?.routine(id: routineId) != nil {
                        NavigationLink(value: BotPage.routine(routineId)) {
                            BoardRow(title: "Open the routine") { BoardGlyph(symbol: "clock") }
                        }
                        .accessibilityIdentifier("post-routine")
                    }
                    // Its session, while this device still lists it (a Run
                    // now's is replaced by the next one).
                    if let threadChatId, let chat = model.chat(id: threadChatId) {
                        if chat.isReadOnlyThread {
                            NavigationLink(value: BotThreadLink(chatId: threadChatId)) {
                                BoardRow(title: "Open its session", sub: "Read-only") {
                                    BoardGlyph(symbol: "bubble.left.and.text.bubble.right")
                                }
                            }
                            .accessibilityIdentifier("post-session")
                        } else {
                            Button {
                                openChat(threadChatId)
                            } label: {
                                BoardRow(title: "Open its session") {
                                    BoardGlyph(symbol: "bubble.left.and.text.bubble.right")
                                }
                            }
                            .tint(Theme.text)
                            .accessibilityIdentifier("post-session")
                        }
                    }
                }
                .groupedRowStyle()
            }
        }
    }
}

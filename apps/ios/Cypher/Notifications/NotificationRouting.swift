// Where a tapped notification lands: a session, or a workspace's chat with
// the board page that settles the Needs you item the notice names. Pure, so
// the rules are tested without a screen (HomeView applies the result).

import Foundation

enum NotificationTarget: Equatable {
    /// A project's session.
    case session(chatId: String)
    /// A workspace (bot id), its chat on screen; `page`, when set, is the
    /// board page to open over it.
    case workspace(botId: String, chatId: String, page: BotPage?)

    /// The route the stack opens.
    var route: Route {
        switch self {
        case .session(let chatId): .chat(chatId)
        case .workspace(let botId, _, _): .bot(botId)
        }
    }

    /// The chat the screen shows: reported as viewed once it opens.
    var chatId: String {
        switch self {
        case .session(let chatId), .workspace(_, let chatId, _): chatId
        }
    }
}

/// A board page a workspace notice asked for, opened by that workspace's
/// screen once it is frontmost (it owns the board's sheet and inspector).
struct NotificationBoardRequest: Equatable, Identifiable {
    let id = UUID()
    let botId: String
    let page: BotPage
    let at = Date()

    /// Too old to act on: the workspace never came up for it, and opening
    /// the board on a later, unrelated visit would be a surprise.
    func expired(now: Date = Date()) -> Bool {
        now.timeIntervalSince(at) > 10
    }
}

extension WorkspaceData {
    /// Where the notice opens, or nil while its chat (or workspace) isn't
    /// known here — not yet synced, or another account's. Only an actual
    /// chat of the notice's project, or the workspace's own chat, opens.
    func notificationTarget(_ payload: PushPayload) -> NotificationTarget? {
        guard let botId = payload.workspaceId else {
            guard let chat = chat(id: payload.chatId), chat.spaceId == payload.projectId else { return nil }
            return .session(chatId: chat.id)
        }
        guard let bot = bot(id: botId), bot.chatId == payload.chatId else { return nil }
        return .workspace(botId: bot.id, chatId: bot.chatId, page: payload.item.flatMap { page(for: $0, of: bot.id) })
    }

    /// The board page of a Needs you item of `botId`'s; nil once the item
    /// is gone (deleted, or not this workspace's), which leaves the tap on
    /// the workspace's chat. A decided item still opens: its page says how
    /// it ended.
    private func page(for item: PushItem, of botId: String) -> BotPage? {
        switch item {
        case .plan(let id):
            return botRecords.work.contains { $0.id == id && $0.botId == botId } ? .workItem(id) : nil
        case .approval(let id):
            return botRecords.approvals.contains { $0.id == id && $0.botId == botId } ? .approval(id) : nil
        case .review(let id):
            return botRecords.exchanges.contains { $0.id == id && $0.botId == botId } ? .exchange(id) : nil
        case .asking(let chatId):
            // A session one of its plans started (docs/design/plan-asks.md):
            // its question waits in the workspace's Needs you too.
            if let session = chat(id: chatId), let startedBy = session.startedBy, startedBy.botId == botId {
                return .asking(chatId: chatId, workId: startedBy.workId)
            }
            guard let thread = chat(id: chatId), let marker = thread.bot, marker.botId == botId, !marker.isMain
            else { return nil }
            if case .work(let workId) = marker.thread { return .asking(chatId: chatId, workId: workId) }
            return .asking(chatId: chatId, workId: nil)
        }
    }
}

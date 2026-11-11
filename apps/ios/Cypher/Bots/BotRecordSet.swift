// Every workspace record the registry holds, as the board reads them
// (bot_records.rs `BotRecords`, minus the legacy notebook), and the one
// cross-record rule the board is built around: what needs the owner.

import Foundation

/// Every workspace record in the registry. Projected from the registry rows
/// in `WorkspaceStore.project()` (stable orders as `read_bot_records`
/// sorts them), or held in memory by the demo.
struct BotRecords: Hashable, Sendable {
    /// Oldest first.
    var work: [BotWork] = []
    /// Oldest first.
    var routines: [BotRoutine] = []
    /// Newest first.
    var activity: [BotActivity] = []
    /// Oldest first.
    var approvals: [BotApproval] = []
    /// Oldest first.
    var connections: [BotConnection] = []
    /// Newest first.
    var invites: [BotInvite] = []
    /// Newest first.
    var exchanges: [BotExchange] = []
    var shortcuts: [BotShortcut] = []
    /// Every memory store's row, not only workspaces': the app lists all of
    /// them from here. By id.
    var memoryStores: [MemoryStore] = []
}

extension BotRecords: Codable {
    private enum CodingKeys: String, CodingKey {
        case work, routines, activity, approvals, connections, invites, exchanges, shortcuts, memoryStores
    }

    /// The `WatchBotRecords` frame shape; every list defaults to empty.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        work = try c.decode(.work, or: [])
        routines = try c.decode(.routines, or: [])
        activity = try c.decode(.activity, or: [])
        approvals = try c.decode(.approvals, or: [])
        connections = try c.decode(.connections, or: [])
        invites = try c.decode(.invites, or: [])
        exchanges = try c.decode(.exchanges, or: [])
        shortcuts = try c.decode(.shortcuts, or: [])
        memoryStores = try c.decode(.memoryStores, or: [])
    }
}

/// One item on a workspace's **Needs you** list.
enum NeedsYouItem: Hashable, Identifiable, Sendable {
    /// The workspace chat itself is waiting on an answer (its chat id).
    case chat(String)
    /// An action awaiting approval (approval id).
    case approval(String)
    /// A session the workspace started is asking.
    case asking(chatId: String, workId: String)
    /// A plan awaiting approval (work id).
    case plan(String)
    /// A peer's answer drafted in Review mode (exchange id).
    case review(String)

    var id: String {
        switch self {
        case .chat(let id): "chat:\(id)"
        case .approval(let id): "approval:\(id)"
        case .asking(let chatId, let workId): "asking:\(workId):\(chatId)"
        case .plan(let id): "plan:\(id)"
        case .review(let id): "review:\(id)"
        }
    }
}

extension BotRecords {
    /// What waits on the owner's decision for `botId`, most pressing kind
    /// first and oldest first within a kind (bot_records.rs `needs_you`),
    /// led by the workspace chat when its own session is asking (as the
    /// desktop's Needs you card does). `asking(chatId)` says whether a
    /// session is waiting on an answer. Decisions only: a failed routine run
    /// is state, shown on the routine.
    func needsYou(botId: String, chatId: String?, asking: (String) -> Bool) -> [NeedsYouItem] {
        var items: [NeedsYouItem] = []
        if let chatId, asking(chatId) { items.append(.chat(chatId)) }
        items += approvals.filter { $0.botId == botId && $0.awaiting }
            .sorted { $0.createdAt < $1.createdAt }
            .map { .approval($0.id) }
        let mine = work.filter { $0.botId == botId }.sorted { $0.createdAt < $1.createdAt }
        // A session the workspace started that waits on an answer needs the
        // owner whatever its work did since: the work may have ended while
        // the question stays open.
        for item in mine {
            for session in item.sessionIds where asking(session) {
                items.append(.asking(chatId: session, workId: item.id))
            }
        }
        items += mine.filter(\.awaitingApproval).map { .plan($0.id) }
        items += exchanges.filter { $0.botId == botId && $0.awaitingReview }
            .sorted { $0.askedAt < $1.askedAt }
            .map { .review($0.id) }
        return items
    }

    /// The row of the store for `scope`, when it has one.
    func memoryStore(scope: MemoryScope) -> MemoryStore? {
        memoryStores.first { $0.scope == scope }
    }

    /// `botId`'s shortcuts, in their order.
    func shortcuts(of botId: String) -> [BotShortcut] {
        shortcuts.filter { $0.botId == botId }.sorted {
            ($0.order, $0.createdAt, $0.id) < ($1.order, $1.createdAt, $1.id)
        }
    }

    /// The work item and step that started `chatId`, when a workspace did.
    func startedBy(chatId: String) -> (work: BotWork, step: WorkStep)? {
        for item in work {
            if let step = item.steps.first(where: { $0.sessions.contains(chatId) }) {
                return (item, step)
            }
        }
        return nil
    }
}

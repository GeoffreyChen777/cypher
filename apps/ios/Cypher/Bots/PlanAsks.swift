// Which chats wait on the owner's answer (crates/proto/src/view/plan_asks.rs,
// docs/design/plan-asks.md). One rule for Needs you, the workspace counts
// and the step marks: a chat asks while its session row carries an open
// question (`SessionRow.asking`), whatever the row's status or age; a plan's
// question is one open in a chat the plan started or steered, or in any
// child chat under one. A row from a host that doesn't write `asking` falls
// back to the old rule (`awaitingInput` and fresh). Shared vectors:
// protocol/vectors/plan-asks-v1.json.

import Foundation

/// Whether one session row waits on an answer (plan_asks.rs `Ask`).
enum SessionAskState: Hashable {
    case none
    case asking
    /// Asking, but the chat's host device is offline: the question is open
    /// and is answered once the device is back.
    case askingOffline

    var isAsking: Bool { self != .none }
}

/// Whether `row` waits on an answer (plan_asks.rs `session_ask`).
/// `hostOnline`: the row's `deviceId` is reachable now. Pure.
func sessionAsk(_ row: SessionRow?, now: Int64, hostOnline: Bool) -> SessionAskState {
    guard let row else { return .none }
    if row.asking != nil { return hostOnline ? .asking : .askingOffline }
    // A host that doesn't write `asking`: the status, while the row is fresh.
    return effectiveStatus(row, now: now) == .awaitingInput ? .asking : .none
}

/// A question found under a chat (`ChatTree.question`).
struct PlanQuestion: Hashable {
    /// The chat that asks: the chat itself (its own question, or a
    /// subagent's mirrored into it), else the nearest child chat that does.
    var chatId: String
    /// Its host device is offline ("Waiting for its device").
    var offline: Bool
}

/// Chats by parent, to find a question anywhere under a chat
/// (plan_asks.rs `ChatTree`).
struct ChatTree {
    /// How deep under a chat its child chats are searched: the engine
    /// forwards a subagent's question at most this many levels up.
    static let maxChildDepth = 8

    private var children: [String: [String]] = [:]
    private var ended: Set<String> = []

    /// One chat's place in the tree: its id, `child.parentChatId` and
    /// whether it is archived (an archived chat has ended: it asks nothing,
    /// and neither do its child chats on its behalf).
    init(links: [(id: String, parentChatId: String?, archived: Bool)]) {
        for link in links {
            if link.archived { ended.insert(link.id) }
            if let parent = link.parentChatId { children[parent, default: []].append(link.id) }
        }
        for key in children.keys { children[key]?.sort() }
    }

    init(_ chats: [Chat]) {
        self.init(links: chats.map { ($0.id, $0.child?.parentChatId, $0.archived) })
    }

    /// The child chats directly under `chatId`, by id.
    func children(of chatId: String) -> [String] {
        children[chatId] ?? []
    }

    /// The question open in `chatId` or in a child chat under it: the chat's
    /// own first, then its children level by level, `maxChildDepth` deep.
    /// An archived chat asks nothing and hides its subtree.
    func question(_ chatId: String, ask: (String) -> SessionAskState) -> PlanQuestion? {
        var seen = Set<String>()
        var level = [chatId]
        for _ in 0...Self.maxChildDepth {
            var next: [String] = []
            for id in level {
                if ended.contains(id) || !seen.insert(id).inserted { continue }
                switch ask(id) {
                case .none: next += children(of: id)
                case let found: return PlanQuestion(chatId: id, offline: found == .askingOffline)
                }
            }
            if next.isEmpty { break }
            level = next
        }
        return nil
    }
}

/// `ChatTree.question` over rows by chat id and presence by device id
/// (plan_asks.rs `plan_question`).
func findPlanQuestion(
    tree: ChatTree, chatId: String, sessions: (String) -> SessionRow?, deviceOnline: (String) -> Bool,
    now: Int64
) -> PlanQuestion? {
    tree.question(chatId) { id in
        let row = sessions(id)
        return sessionAsk(row, now: now, hostOnline: row.map { deviceOnline($0.deviceId) } ?? true)
    }
}

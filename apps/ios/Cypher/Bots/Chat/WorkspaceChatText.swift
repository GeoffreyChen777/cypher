// The workspace chat's words, apart from its views so they can be tested:
// what a workspace can see and do in a few words (shell/bots/chat.rs
// `access_summary`), what it does (`bot_role`), the questions a new one
// offers, what a hidden thread belongs to, and how shortcuts move.

import Foundation

enum WorkspaceChatText {
    /// What a workspace can see and do: "Billing · reads conversations",
    /// "invoice-demo and cypher · works directly · starts sessions".
    /// `spaceName` names a project by id; unknown ids are left out.
    static func accessSummary(_ bot: Bot, spaceName: (String) -> String?) -> String {
        let scope = bot.scope
        let projects: String
        switch scope.projects {
        case .all: projects = "All projects"
        case .noProjects: projects = ""
        case .selected(let ids):
            let names = ids.compactMap(spaceName)
            switch names.count {
            case 0: projects = ""
            case 1: projects = names[0]
            case 2: projects = "\(names[0]) and \(names[1])"
            default: projects = "\(names.count) projects"
            }
        }
        let shared = scope.includeSessions.count
        let reach: String
        switch (projects.isEmpty, shared) {
        case (true, 0): return "No access yet"
        case (true, let n): reach = "\(n) shared session\(n == 1 ? "" : "s")"
        case (false, 0): reach = projects
        case (false, let n): reach = "\(projects) + \(n) session\(n == 1 ? "" : "s")"
        }
        let caps = bot.capabilities
        var acts: [String] = []
        if caps.workDirectly { acts.append("works directly") }
        switch (caps.coordinate, caps.steer) {
        case (true, true): acts.append("starts and steers sessions")
        case (true, false): acts.append("starts sessions")
        case (false, true): acts.append("steers sessions")
        case (false, false): break
        }
        // What it may do says more than how deep it reads, which only stays
        // when it is the narrow kind.
        let depth: String? =
            !scope.transcripts ? "titles and status only" : acts.isEmpty ? "reads conversations" : nil
        return ([reach] + [depth].compactMap { $0 } + acts).joined(separator: " · ")
    }

    /// What the workspace does, in a few words.
    static func role(_ capabilities: BotCapabilities) -> String {
        if capabilities.coordinate { return "Plans and does work across your projects" }
        if capabilities.workDirectly { return "Answers questions and works in your projects" }
        return "Answers questions about your work"
    }

    /// Whether the workspace can see anything yet.
    static func hasAccess(_ bot: Bot) -> Bool {
        if case .noProjects = bot.scope.projects { return !bot.scope.includeSessions.isEmpty }
        return true
    }

    /// The questions a new workspace offers, each with its glyph; the
    /// third names the first project it sees.
    static func suggestions(firstProject: String?) -> [(symbol: String, text: String)] {
        var out: [(symbol: String, text: String)] = [
            ("waveform.path.ecg", "What did I work on today?"),
            ("questionmark.circle", "Which sessions are waiting on me?"),
        ]
        if let firstProject { out.append(("folder", "Where do things stand in \(firstProject)?")) }
        out.append(("checklist", "What's still unfinished across my projects?"))
        return out
    }

    /// Who a chat run in a workspace's own folder belongs to, for its
    /// subtitle ("Atlas · MacBook Pro"): the workspace for its chat and
    /// threads, "Memory" for a memory pass outside any workspace. nil for
    /// a session in a project (a project routine's session, the sessions
    /// work starts), which keeps its project.
    @MainActor static func threadOwner(_ chat: Chat, in data: (any WorkspaceData)?) -> String? {
        if let pass = chat.memory {
            if case .workspace(let botId) = pass.scope, let bot = data?.bot(id: botId) { return bot.name }
            return "Memory"
        }
        guard let marker = chat.bot, marker.isBotThread else { return nil }
        return data?.bot(id: marker.botId)?.name ?? "Workspace"
    }

    /// "2 need you".
    static func needsYou(_ count: Int) -> String {
        count == 1 ? "1 needs you" : "\(count) need you"
    }

    /// The ids after moving `id` one place by `delta` (-1 earlier, +1
    /// later); nil when it is already at that end (shortcuts.rs `moved`).
    static func moved(_ ids: [String], _ id: String, by delta: Int) -> [String]? {
        guard let from = ids.firstIndex(of: id) else { return nil }
        let to = from + delta
        guard ids.indices.contains(to) else { return nil }
        var ids = ids
        ids.swapAt(from, to)
        return ids
    }
}

/// What a hidden chat belongs to, for the banner over it: "Plan · Fix
/// invoice rounding", "Routine · Nightly tests", "Question from Ledger".
struct ThreadOrigin: Equatable {
    var symbol: String
    var text: String
    /// The workspace whose board shows it.
    var botId: String?
    /// The board page it opens; nil opens none (or Memory settings, for a
    /// memory pass outside a workspace).
    var page: BotPage?
    /// A memory pass's store, for a pass outside any workspace.
    var memory: MemoryScope?

    /// The origin of a hidden chat; nil for the workspace chat itself and
    /// every ordinary session.
    @MainActor static func of(_ chat: Chat, in data: any WorkspaceData) -> ThreadOrigin? {
        if let pass = chat.memory {
            let botId: String? = if case .workspace(let id) = pass.scope { id } else { nil }
            return ThreadOrigin(
                symbol: "book.closed", text: "Memory · \(memoryWords(pass.scope, data))",
                botId: botId, page: botId != nil ? .memory(pass.scope) : nil, memory: pass.scope)
        }
        guard let marker = chat.bot else { return nil }
        let bot = data.bot(id: marker.botId)
        switch marker.thread {
        case .main:
            return nil
        case .work(let workId):
            let title = data.work(id: workId)?.title ?? "A removed plan"
            return ThreadOrigin(symbol: "checklist", text: "Plan · \(oneLine(title))", botId: marker.botId, page: .workItem(workId))
        case .routine(let routineId), .routineSession(let routineId):
            let name = data.routine(id: routineId)?.name ?? "A removed routine"
            return ThreadOrigin(
                symbol: "clock.arrow.circlepath", text: "Routine · \(oneLine(name))", botId: marker.botId,
                page: .routine(routineId))
        case .peer(let connectionId):
            let peer = data.connection(id: connectionId)?.peer.name ?? "a connection"
            return ThreadOrigin(
                symbol: "person.2", text: "Question from \(peer)", botId: marker.botId, page: .connection(connectionId))
        case .unknown:
            return ThreadOrigin(symbol: "square.grid.2x2", text: bot.map { "Workspace · \($0.name)" } ?? "Workspace")
        }
    }

    @MainActor private static func memoryWords(_ scope: MemoryScope, _ data: any WorkspaceData) -> String {
        switch scope {
        case .you: return "Your memory"
        case .project(let spaceId): return data.spaces.first { $0.id == spaceId }?.displayName ?? "A project"
        case .workspace(let botId): return data.bot(id: botId)?.name ?? "A workspace"
        case .role(let agent, _): return agent
        }
    }

    private static func oneLine(_ text: String) -> String {
        text.split(whereSeparator: \.isNewline).joined(separator: " ")
    }
}

extension Chat {
    /// A hidden chat only its host writes to: a workspace's work, routine
    /// and peer threads, and memory passes. It opens read-only. A project
    /// routine's session is an ordinary session and stays writable.
    var isReadOnlyThread: Bool {
        if memory != nil { return true }
        guard let bot else { return false }
        return !bot.isMain && bot.isBotThread
    }

    /// The workspace's own conversation (not one of its threads).
    var isWorkspaceChat: Bool { bot?.isMain == true }
}

extension AppModel {
    /// Puts `text` in a session's composer, unless a draft is in progress
    /// (the composer takes it once).
    func offerDraft(_ text: String, chatId: String) {
        pendingDrafts[chatId] = text
        draftOffers &+= 1
    }
}

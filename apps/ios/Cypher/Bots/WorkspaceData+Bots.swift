// What the workspace screens read, derived once for the live store and the
// demo alike (the desktop's crates/ui/src/shell/bots.rs list helpers): a
// workspace's needs, work, routines, connections and status, in the orders
// the board shows them.

import Foundation

/// What a workspace is doing, for its row in a list: the most pressing
/// first, as the desktop switcher picks it.
enum BotListStatus: Hashable {
    /// Items wait on the owner.
    case needsYou(Int)
    /// Its chat is working.
    case working
    case paused
    /// Its host device is offline.
    case hostOffline
    case idle
}

extension WorkspaceData {
    func bot(id: String) -> Bot? {
        bots.first { $0.id == id }
    }

    /// The workspace's own conversation.
    func botChat(_ bot: Bot) -> Chat? {
        chat(id: bot.chatId)
    }

    /// The workspace a chat belongs to: its own chat or one of its threads.
    func bot(of chat: Chat) -> Bot? {
        chat.bot.flatMap { bot(id: $0.botId) }
    }

    /// Whether a session waits on an answer, as its row's dot says
    /// (bots.rs `chat_asking`).
    func sessionAsking(_ chatId: String) -> Bool {
        guard let chat = chat(id: chatId) else { return false }
        return indicator(for: chat) == .awaitingInput
    }

    /// What waits on the owner for a workspace, most pressing first: its
    /// own chat asking, then approvals, sessions it started that ask, plans
    /// and review drafts (bots.rs `bot_needs`).
    func needsYou(botId: String) -> [NeedsYouItem] {
        botRecords.needsYou(botId: botId, chatId: bot(id: botId)?.chatId, asking: sessionAsking)
    }

    func needsYouCount(botId: String) -> Int {
        needsYou(botId: botId).count
    }

    /// Everything that needs the owner across every workspace.
    var needsYouTotal: Int {
        bots.reduce(0) { $0 + needsYouCount(botId: $1.id) }
    }

    /// A workspace's work as the Work list sorts it (work.rs `work_lists`):
    /// running (newest start first), then awaiting approval (newest first),
    /// then ended work, newest end first.
    func work(of botId: String) -> [BotWork] {
        let mine = botRecords.work.filter { $0.botId == botId }
        let running = mine.filter { $0.state == .running }
            .sorted { ($0.startedAt ?? $0.createdAt) > ($1.startedAt ?? $1.createdAt) }
        let proposed = mine.filter { $0.state == .proposed }.sorted { $0.createdAt > $1.createdAt }
        let ended = mine.filter { !$0.isLive }
            .sorted { ($0.endedAt ?? $0.createdAt) > ($1.endedAt ?? $1.createdAt) }
        return running + proposed + ended
    }

    func work(id: String) -> BotWork? {
        botRecords.work.first { $0.id == id }
    }

    /// A workspace's routines in order of next run; those with none (manual,
    /// disabled, done) last, by name.
    func routines(of botId: String) -> [BotRoutine] {
        botRecords.routines.filter { $0.botId == botId }.sorted { a, b in
            switch (a.nextRunAt, b.nextRunAt) {
            case (let x?, let y?) where x != y: return x < y
            case (.some, .none): return true
            case (.none, .some): return false
            default: return (a.name.lowercased(), a.createdAt, a.id) < (b.name.lowercased(), b.createdAt, b.id)
            }
        }
    }

    func routine(id: String) -> BotRoutine? {
        botRecords.routines.first { $0.id == id }
    }

    /// A workspace's shortcuts, in their order.
    func shortcuts(of botId: String) -> [BotShortcut] {
        botRecords.shortcuts(of: botId)
    }

    /// Live connections, most recently active first (network.rs
    /// `live_connections`).
    func connections(of botId: String) -> [BotConnection] {
        botRecords.connections.filter { $0.botId == botId && $0.isLive }.sorted {
            ($0.lastExchangeAt ?? $0.createdAt, $0.id) > ($1.lastExchangeAt ?? $1.createdAt, $1.id)
        }
    }

    func connection(id: String) -> BotConnection? {
        botRecords.connections.first { $0.id == id }
    }

    /// Invites still open, newest first.
    func openInvites(of botId: String, now: Int64 = nowMs()) -> [BotInvite] {
        botRecords.invites.filter { $0.botId == botId && $0.isOpen(nowMs: now) }
            .sorted { $0.createdAt > $1.createdAt }
    }

    /// Questions to and from a workspace (one connection's, when named),
    /// newest first.
    func exchanges(of botId: String, connectionId: String? = nil) -> [BotExchange] {
        botRecords.exchanges.filter { $0.botId == botId && (connectionId == nil || $0.connectionId == connectionId) }
            .sorted { $0.askedAt > $1.askedAt }
    }

    func exchange(id: String) -> BotExchange? {
        botRecords.exchanges.first { $0.id == id }
    }

    func approval(id: String) -> BotApproval? {
        botRecords.approvals.first { $0.id == id }
    }

    /// The audit log, newest first, one group's when named.
    func activity(of botId: String, group: ActivityGroup? = nil) -> [BotActivity] {
        botRecords.activity.filter { $0.botId == botId && (group == nil || $0.kind.group == group) }
            .sorted { ($0.at, $0.id) > ($1.at, $1.id) }
    }

    func memoryStore(scope: MemoryScope) -> MemoryStore? {
        botRecords.memoryStore(scope: scope)
    }

    /// The work item and step that started a session, when a workspace did
    /// ("by Atlas" on its row).
    func startedBy(chatId: String) -> (work: BotWork, step: WorkStep)? {
        botRecords.startedBy(chatId: chatId)
    }

    /// The projects a workspace's scope sees, in the projects' order.
    func grantedSpaces(of bot: Bot) -> [Space] {
        spaces.filter { bot.scope.seesSpace($0.id) }
    }

    func botHostOnline(_ bot: Bot) -> Bool {
        deviceOnline(bot.hostDeviceId)
    }

    /// The device's name, or its id for a device this replica doesn't list.
    func botDeviceName(_ deviceId: String) -> String {
        devices.first { $0.id == deviceId }?.name ?? deviceId
    }

    /// What a workspace is doing, most pressing first (picker.rs): what needs
    /// the owner, its chat working, a pause, an offline host.
    func botStatus(_ bot: Bot) -> BotListStatus {
        let needs = needsYouCount(botId: bot.id)
        if needs > 0 { return .needsYou(needs) }
        if let chat = botChat(bot), indicator(for: chat) == .working { return .working }
        if bot.paused { return .paused }
        if !botHostOnline(bot) { return .hostOffline }
        return .idle
    }
}

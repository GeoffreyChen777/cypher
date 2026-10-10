// AppModel data accessors: one path for views over the demo dataset or the
// live workspace, plus the chat/space actions behind them.

import Foundation
import Network
import SwiftUI

extension AppModel {
    // MARK: Unified data accessors (demo or live — one path for views)

    var spaces: [Space] { demo?.spaces ?? workspace?.spaces ?? [] }
    var allChats: [Chat] { demo?.chats ?? workspace?.chats ?? [] }
    var sessionRows: [String: SessionRow] { demo?.sessions ?? workspace?.sessions ?? [:] }

    func subagents(for parent: Chat, store: SessionStore, now: Int64) -> [SubagentPanelEntry] {
        let session = sessionRows[parent.id]
        let snapshot = session?.deviceId == parent.deviceId ? session?.subagents ?? [] : []
        return SubagentProjection.aggregate(parent: parent, transcript: store.entries,
            snapshot: snapshot, chats: allChats, sessions: sessionRows, now: now)
    }

    var connected: Bool { demo != nil || workspace?.connected == true }

    var overviewChats: [Chat] {
        if let demo {
            let liveIds = Set(demo.spaces.map(\.id))
            let live = demo.chats.filter { !$0.isChild && !$0.archived && $0.spaceId.map(liveIds.contains) == true }
            return sortActive(live)
        }
        return workspace?.overviewChats ?? []
    }

    /// Active sessions outside any live project (see WorkspaceStore).
    var projectlessChats: [Chat] {
        if let demo {
            let liveIds = Set(demo.spaces.map(\.id))
            return sortActive(demo.chats.filter {
                !$0.isChild && !$0.archived && !$0.isScratch && !($0.spaceId.map(liveIds.contains) ?? false)
            })
        }
        return workspace?.projectlessChats ?? []
    }

    /// Active quick chats across devices.
    var quickChats: [Chat] {
        if let demo {
            return sortActive(demo.chats.filter { !$0.isChild && !$0.archived && $0.isScratch })
        }
        return workspace?.quickChats ?? []
    }

    /// Registered devices, online first then by name (the quick-chat
    /// palette's order, minus "this device": the phone isn't one).
    var devices: [DeviceRow] {
        let all = demo?.devices ?? workspace?.devices ?? []
        return all.sorted { a, b in
            let (oa, ob) = (deviceOnline(a.id), deviceOnline(b.id))
            if oa != ob { return oa }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    /// Quick chat's first two steps: the host makes the scratch folder, then
    /// the chat row is minted there without a project. Returns the chat id.
    func createQuickChat(deviceId: String, config: ChatConfig) async throws -> String {
        let chatId = UUID().uuidString.lowercased()
        if let demo {
            try? await Task.sleep(nanoseconds: 200_000_000)
            demo.chats.append(Chat(id: chatId, deviceId: deviceId, title: nil, archived: false,
                                   cwd: "/tmp/cypher-scratch/\(chatId)", branch: nil, checkoutId: nil,
                                   config: config, lastMessagePreview: nil, lastMessageAt: nil,
                                   createdAt: nowMs(), spaceId: nil, lastSeenAt: nowMs()))
            return chatId
        }
        guard let workspace else { throw RelayError.notConnected }
        let cwd = try await workspace.createScratchDir(deviceId: deviceId, chatId: chatId)
        workspace.createQuickChat(chatId: chatId, deviceId: deviceId, cwd: cwd, config: config)
        return chatId
    }

    enum ForkOutcome: Equatable {
        case created(chatId: String)
        case failed(String)
    }

    /// Session Fork v1: a new chat from this transcript up to `anchor` —
    /// before a user message (its text comes back as the draft), or after
    /// an assistant reply.
    func forkSession(_ chat: Chat, anchor: MessageEntry) async -> ForkOutcome {
        if let demo { return demoFork(chat, anchor: anchor, demo: demo) }
        guard let workspace else { return .failed("Not connected") }
        let key = "\(chat.id)#\(anchor.id)"
        let requestId = forkRequestIds[key] ?? UUID().uuidString.lowercased()
        forkRequestIds[key] = requestId
        do {
            let response = try await workspace.forkSession(deviceId: chat.deviceId, requestId: requestId,
                                                           sourceChatId: chat.id, anchorMessageId: anchor.id)
            forkRequestIds[key] = nil
            switch response {
            case .created(let chatId, _, let composerText):
                if let composerText, !composerText.isEmpty { pendingDrafts[chatId] = composerText }
                return .created(chatId: chatId)
            case .unavailable(let message):
                return .failed(message)
            }
        } catch RelayError.rpc(let message) where message.lowercased().contains("unknown method") {
            forkRequestIds[key] = nil
            return .failed("Forking needs a newer Cypher on \(deviceName(chat.deviceId)).")
        } catch {
            return .failed("Couldn't fork — \(error.localizedDescription)")
        }
    }

    /// The composer's seeded draft for a session, once.
    func takePendingDraft(chatId: String) -> String? {
        pendingDrafts.removeValue(forKey: chatId)
    }

    private func demoFork(_ chat: Chat, anchor: MessageEntry, demo: DemoDataset) -> ForkOutcome {
        let entries = demo.sessionStore(for: chat.id).entries
        guard let ix = entries.firstIndex(where: { $0.id == anchor.id }) else { return .failed("Message not found") }
        let isUser = anchor.role == .user
        let copied = Array(entries.prefix(isUser ? ix : ix + 1))
        let id = "chat-\(UUID().uuidString.lowercased().prefix(8))"
        var fork = chat
        fork.id = id
        fork.title = String("\(chat.displayTitle) — Fork".prefix(120))
        fork.createdAt = nowMs()
        fork.lastMessageAt = nowMs()
        fork.lastSeenAt = nowMs()
        demo.chats.append(fork)
        demo.sessionStore(for: id).setEntries(copied)
        if isUser {
            let text = anchor.parts.compactMap { part -> String? in
                if case .text(_, let t, _) = part { return t }
                return nil
            }.joined(separator: "\n")
            pendingDrafts[id] = parseUserMessageImages(text).text
        }
        return .created(chatId: id)
    }

    /// A side chat about `quote`, on the parent's host.
    func sideChat(parent: Chat, quote: String, anchorEntryId: String?) -> SideChatStore? {
        if let demo {
            return SideChatStore(parent: parent, quote: quote, anchorEntryId: anchorEntryId,
                                 relay: nil, config: DemoDataset.dummyConfig, demo: demo)
        }
        guard let workspace, let config else { return nil }
        return SideChatStore(parent: parent, quote: quote, anchorEntryId: anchorEntryId,
                             relay: workspace.relayClient(for: parent.deviceId), config: config)
    }

    /// Rename (desktop Rename…): trimmed; an empty title is ignored.
    func renameChat(chatId: String, title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        if let demo {
            if let ix = demo.chats.firstIndex(where: { $0.id == chatId }) { demo.chats[ix].title = title }
            return
        }
        workspace?.rename(chatId: chatId, title: title)
    }

    /// Permanently delete a session (and its subagent children). Returns a
    /// notice when part of the cleanup couldn't happen.
    func deleteChat(_ chat: Chat) async -> String? {
        if let demo {
            demo.chats.removeAll { $0.id == chat.id || $0.child?.parentChatId == chat.id }
            return nil
        }
        guard let workspace else { return "Not connected" }
        let notice = await workspace.deleteChat(chat, hostOnline: deviceOnline(chat.deviceId))
        sessionStores.removeValue(forKey: chat.id)?.stop()
        recentSessionIds.removeAll { $0 == chat.id }
        return notice
    }

    func chats(in spaceId: String) -> [Chat] {
        if let demo {
            return sortActive(demo.chats.filter { !$0.isChild && !$0.archived && $0.spaceId == spaceId })
        }
        return workspace?.chats(in: spaceId) ?? []
    }

    func chat(id: String) -> Chat? {
        (demo?.chats ?? workspace?.chats)?.first { $0.id == id }
    }

    /// state.rs `space_for_chat` — nil for a dangling/missing space_id.
    func space(for chat: Chat) -> Space? {
        guard let spaceId = chat.spaceId else { return nil }
        return spaces.first { $0.id == spaceId }
    }

    func indicator(for chat: Chat) -> ChatIndicator {
        if let demo {
            return chatIndicator(chat: chat, live: effectiveStatus(demo.sessions[chat.id], now: nowMs()))
        }
        return workspace?.indicator(for: chat) ?? .idle
    }

    func spaceIndicator(_ spaceId: String) -> ChatIndicator? {
        ChatIndicator.projectSummary(chats(in: spaceId).map { indicator(for: $0) })
    }

    func deviceName(_ deviceId: String) -> String {
        (demo?.devices ?? workspace?.devices)?.first { $0.id == deviceId }?.name ?? deviceId
    }

    func deviceOnline(_ deviceId: String) -> Bool {
        if let demo {
            guard let seen = demo.devices.first(where: { $0.id == deviceId })?.lastSeenAt else { return false }
            return nowMs() - seen < presenceFreshMs
        }
        return workspace?.deviceOnline(deviceId) ?? false
    }

    /// The session row (live status, context usage) behind a chat.
    func sessionRow(chatId: String) -> SessionRow? {
        demo?.sessions[chatId] ?? workspace?.sessions[chatId]
    }

    /// The Pi slash commands on the chat's host device.
    func listCommands(deviceId: String) async throws -> [SlashCommand] {
        if demo != nil {
            try? await Task.sleep(nanoseconds: 150_000_000)
            return DemoDataset.slashCommands
        }
        guard let workspace, deviceOnline(deviceId) else { throw RelayError.hostOffline }
        return try await workspace.listCommands(deviceId: deviceId, harness: "pi")
    }

    /// What the chat's Pi switches are set to, for the `/` menu's badges.
    func piSessionModes(deviceId: String, chatId: String) async throws -> PiSessionModes {
        if demo != nil { return DemoDataset.piSessionModes }
        guard let workspace, deviceOnline(deviceId) else { throw RelayError.hostOffline }
        return try await workspace.piSessionModes(deviceId: deviceId, chatId: chatId)
    }

    /// SearchFiles on the device that owns the checkout, for `@` mentions.
    func searchFiles(_ scope: MentionScope.Files, query: String) async throws -> [FileSearchMatch] {
        if let demo {
            try? await Task.sleep(nanoseconds: 120_000_000)
            return demo.searchFiles(query)
        }
        guard let workspace, deviceOnline(scope.deviceId) else { throw RelayError.hostOffline }
        var params = scope.params
        params["query"] = query
        return try await workspace.searchFiles(deviceId: scope.deviceId, params: params)
    }

    /// The `@session` references' snapshots, in mention order (composer.rs
    /// send path). Each comes from this phone's synced copy of the session,
    /// so a sleeping laptop's sessions stay referenceable; a copy that has
    /// never synced fails the send rather than dropping the reference.
    func sessionReferences(_ ids: [String]) async throws -> [SessionReference] {
        var references: [SessionReference] = []
        for id in ids {
            guard let chat = chat(id: id) else {
                throw SessionReferenceError("A referenced session no longer exists — remove the @session reference and try again.")
            }
            references.append(SessionReference(title: Mentions.sessionTitle(chat),
                                               context: try await referenceContext(chat)))
        }
        return references
    }

    /// Quiet window after a cold copy first shows content: backfill lands as
    /// a checkpoint then its rows (composer.rs SESSION_REPLICA_SETTLE).
    private static let referenceSettle: TimeInterval = 0.4
    private static let referenceTimeout: TimeInterval = 8

    private func referenceContext(_ chat: Chat) async throws -> String {
        if let demo {
            let entries = SessionReferences.entries(demo.sessionStore(for: chat.id).entries)
            return SessionReferences.boundedContext(entries) ?? ""
        }
        guard let store = warmSessionStore(for: chat) else {
            throw SessionReferenceError("Couldn't load the referenced session — check your connection and try again.")
        }
        // A copy that already has content is current enough (it syncs live);
        // a cold one waits for its backfill to land and go quiet.
        if store.entries.isEmpty {
            let deadline = Date().addingTimeInterval(Self.referenceTimeout)
            var revision = store.revision
            var quietSince: Date?
            while Date() < deadline {
                try await Task.sleep(nanoseconds: 100_000_000)
                if store.revision != revision {
                    revision = store.revision
                    quietSince = store.entries.isEmpty ? nil : Date()
                } else if let quietSince, Date().timeIntervalSince(quietSince) >= Self.referenceSettle {
                    break
                }
            }
            if store.entries.isEmpty, !store.connected {
                throw SessionReferenceError("This phone has no synced copy of “\(Mentions.sessionTitle(chat))” yet — try again once it loads.")
            }
        }
        return SessionReferences.boundedContext(await store.referenceEntries()) ?? ""
    }

    /// The phone never resolves a local Runtime or substitutes a model list.
    func listPiModels(deviceId: String) async throws -> [ModelInfo] {
        if demo != nil {
            return HarnessCatalog.demoModels
        }
        #if CYPHER_DEVELOPMENT
        // `-mock-providers` (dev-ios.sh argument): the engine's catalog plus
        // mock providers, or the mocks alone when the engine is unreachable.
        if ProcessInfo.processInfo.arguments.contains("-mock-providers") {
            let real = (try? await workspace?.listPiModels(deviceId: deviceId)) ?? []
            let mocked = HarnessCatalog.mockProviderModels.filter { mock in !real.contains { $0.id == mock.id } }
            return real + mocked
        }
        #endif
        guard let workspace, deviceOnline(deviceId) else {
            throw PiCatalogError.unavailable
        }
        return try await workspace.listPiModels(deviceId: deviceId)
    }

    /// Refs of the space's repo (git spaces only).
    func listRefs(space: Space) async -> [RepoRef]? {
        if let demo {
            try? await Task.sleep(nanoseconds: 120_000_000)
            return demo.listRefs(spacePath: space.path)
        }
        return await workspace?.listRefs(deviceId: space.deviceId, repoPath: space.path)
    }

    /// Draft-mode checkout switch: `git checkout` in the SPACE's folder.
    /// Returns an error message, or nil on success.
    func switchSpaceRef(space: Space, refName: String) async -> String? {
        if let demo {
            try? await Task.sleep(nanoseconds: 200_000_000)
            demo.switchRef(path: space.path, refName: refName)
            return nil
        }
        guard let workspace else { return "Not connected" }
        return await workspace.switchRef(deviceId: space.deviceId,
                                         repoPath: space.path, refName: refName)
    }

    /// CreateWorktree off the base ref; returns the new worktree's path.
    func createWorktree(space: Space, base: String) async -> String? {
        if let demo {
            try? await Task.sleep(nanoseconds: 250_000_000)
            return demo.createWorktree(spacePath: space.path, base: base)
        }
        return await workspace?.createWorktree(deviceId: space.deviceId,
                                               repoPath: space.path, branch: base)
    }

    @discardableResult
    func createChat(space: Space, config chatConfig: ChatConfig,
                    branch: String? = nil, cwd: String? = nil) -> String? {
        if let demo {
            let id = "chat-\(UUID().uuidString.lowercased().prefix(8))"
            demo.chats.append(Chat(id: id, deviceId: space.deviceId, title: nil, archived: false,
                                   cwd: cwd ?? space.path, branch: branch, checkoutId: nil,
                                   config: chatConfig, lastMessagePreview: nil, lastMessageAt: nil,
                                   createdAt: nowMs(), spaceId: space.id, lastSeenAt: nowMs()))
            return id
        }
        return workspace?.createChat(space: space, config: chatConfig, branch: branch, cwd: cwd)
    }

    /// Browse folders on a remote device (the desktop add-space palette's data
    /// path). Demo mode serves a canned tree; live mode asks the device over
    /// the relay.
    func listFolders(deviceId: String, path: String?) async -> FolderListing? {
        if let demo {
            try? await Task.sleep(nanoseconds: 120_000_000)  // feel like a network hop
            let target = path ?? demo.homePath(deviceId: deviceId)
            return demo.listFolders(deviceId: deviceId, path: target)
        }
        return await workspace?.listFolders(deviceId: deviceId, path: path)
    }

    @discardableResult
    func createSpace(deviceId: String, path: String, gitDetected: Bool = false) async -> String? {
        if let demo {
            if let existing = demo.spaces.first(where: { $0.deviceId == deviceId && $0.path == path }) {
                return existing.id
            }
            let id = "space-\(UUID().uuidString.lowercased().prefix(8))"
            demo.spaces.append(Space(id: id, deviceId: deviceId, path: path, name: nil,
                                     gitDetected: gitDetected, gitCheckedAt: nil, checkoutId: nil,
                                     createdAt: nowMs()))
            return id
        }
        return await workspace?.createSpace(deviceId: deviceId, path: path, gitDetected: gitDetected)
    }

    /// Archived chats under the same scope as the list above the shelf.
    func archivedChats(in spaceId: String? = nil) -> [Chat] {
        if let demo {
            return sortActive(demo.chats.filter {
                !$0.isChild && $0.archived && (spaceId == nil || $0.spaceId == spaceId)
            })
        }
        return workspace?.archivedChats(in: spaceId) ?? []
    }

    func archive(chatId: String) { setArchived(chatId: chatId, archived: true) }
    func unarchive(chatId: String) { setArchived(chatId: chatId, archived: false) }

    private func setArchived(chatId: String, archived: Bool) {
        if let demo {
            if let ix = demo.chats.firstIndex(where: { $0.id == chatId }) {
                demo.chats[ix].archived = archived
            }
            return
        }
        workspace?.setArchived(chatId: chatId, archived: archived)
    }

    func setChatConfig(chatId: String, config: ChatConfig) {
        if let demo {
            if let ix = demo.chats.firstIndex(where: { $0.id == chatId }) {
                demo.chats[ix].config = config
            }
            return
        }
        workspace?.setChatConfig(chatId: chatId, config: config)
    }

    func markSeen(chatId: String) {
        if let demo {
            if let ix = demo.chats.firstIndex(where: { $0.id == chatId }) {
                demo.chats[ix].lastSeenAt = nowMs()
            }
            return
        }
        workspace?.markSeen(chatId: chatId)
    }

    /// Persist every open doc now (app backgrounding).
    func flushDocs() {
        workspace?.flushToDisk()
        sessionStores.values.forEach { $0.flushToDisk() }
    }

    /// Foreground hook: kick every room NOW (see ChatRoomClient.kick) — after
    /// a suspension the workspace room in particular stays dead while chat
    /// views reconnect on open, freezing sidebar rows and Working indicators
    /// against perfectly live transcripts.
    func foregrounded() {
        trimSessionStores()
        kickAllRooms()
    }

    private func kickAllRooms() {
        workspace?.kickRoom()
        sessionStores.values.forEach { $0.kickRoom() }
    }

    /// Kick rooms the moment the network path recovers or hops interfaces
    /// (wifi drop-and-return while foregrounded, wifi→cellular handover).
    /// Without this the clients sleep out their full reconnect backoff — up
    /// to 30s of dead sidebar on exactly the flaky networks (airplane wifi)
    /// where the OS knows recovery happened the instant it did. Kicks are
    /// idempotent: fresh backoff + immediate redial or a deadline-checked
    /// probe on a session that looks alive.
    func startPathMonitor() {
        guard pathMonitor == nil else { return }
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            // Interface set is part of the key: a satisfied→satisfied hop
            // (wifi→cellular) silently kills established sockets too.
            let key = path.status == .satisfied
                ? "up:" + path.availableInterfaces.map(\.name).sorted().joined(separator: ",")
                : "down"
            Task { @MainActor [weak self] in
                guard let self else { return }
                let previous = self.lastPathKey
                self.lastPathKey = key
                // First callback reports the initial state — nothing to revive.
                guard let previous, previous != key, path.status == .satisfied else { return }
                roomLog.info("network path recovered (\(key, privacy: .public)); kicking rooms")
                self.kickAllRooms()
            }
        }
        monitor.start(queue: DispatchQueue(label: "cypher.path-monitor"))
        pathMonitor = monitor
    }
}

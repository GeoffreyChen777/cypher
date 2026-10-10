// AppModel data accessors: one path for views over the demo dataset or the
// live workspace, plus the chat/space actions behind them.

import Foundation
import Network
import SwiftUI

extension AppModel {
    // MARK: Data accessors (one path for views over demo or live data)

    /// The active workspace: the offline demo, or the live registry mirror.
    var data: (any WorkspaceData)? {
        if let demo { return demo }
        return workspace
    }

    var spaces: [Space] { data?.spaces ?? [] }
    var allChats: [Chat] { data?.chats ?? [] }
    var sessionRows: [String: SessionRow] { data?.sessions ?? [:] }

    func subagents(for parent: Chat, store: SessionStore, now: Int64) -> [SubagentPanelEntry] {
        let session = sessionRows[parent.id]
        let snapshot = session?.deviceId == parent.deviceId ? session?.subagents ?? [] : []
        return SubagentProjection.aggregate(
            parent: parent, transcript: store.entries,
            snapshot: snapshot, chats: allChats, sessions: sessionRows, now: now)
    }

    var connected: Bool { data?.connected == true }

    var overviewChats: [Chat] { data?.overviewChats ?? [] }

    /// Active sessions outside any live project (see WorkspaceData).
    var projectlessChats: [Chat] { data?.projectlessChats ?? [] }

    /// Active quick chats across devices.
    var quickChats: [Chat] { data?.quickChats ?? [] }

    /// Registered devices, online first then by name (the quick-chat
    /// palette's order, minus "this device": the phone isn't one).
    var devices: [DeviceRow] {
        (data?.devices ?? []).sorted { a, b in
            let (oa, ob) = (deviceOnline(a.id), deviceOnline(b.id))
            if oa != ob { return oa }
            return a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
    }

    /// Quick chat's first two steps: the host makes the scratch folder, then
    /// the chat row is minted there without a project. Returns the chat id.
    func createQuickChat(deviceId: String, config: ChatConfig) async throws -> String {
        guard let data else { throw RelayError.notConnected }
        return try await data.createQuickChat(deviceId: deviceId, config: config)
    }

    enum ForkOutcome: Equatable {
        case created(chatId: String)
        case failed(String)
    }

    /// Session Fork v1: a new chat from this transcript up to `anchor` —
    /// before a user message (its text comes back as the draft), or after
    /// an assistant reply.
    func forkSession(_ chat: Chat, anchor: MessageEntry) async -> ForkOutcome {
        guard let data else { return .failed("Not connected") }
        let key = "\(chat.id)#\(anchor.id)"
        let requestId = forkRequestIds[key] ?? UUID().uuidString.lowercased()
        forkRequestIds[key] = requestId
        do {
            let response = try await data.fork(chat, anchor: anchor, requestId: requestId)
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

    /// A side chat about `quote`, on the parent's host.
    func sideChat(parent: Chat, quote: String, anchorEntryId: String?) -> SideChatStore? {
        data?.sideChat(parent: parent, quote: quote, anchorEntryId: anchorEntryId, config: config)
    }

    /// Rename (desktop Rename…): trimmed; an empty title is ignored.
    func renameChat(chatId: String, title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { return }
        data?.rename(chatId: chatId, title: title)
    }

    /// Permanently delete a session (and its subagent children). Returns a
    /// notice when part of the cleanup couldn't happen.
    func deleteChat(_ chat: Chat) async -> String? {
        guard let data else { return "Not connected" }
        let notice = await data.deleteChat(chat, hostOnline: deviceOnline(chat.deviceId))
        sessionStores.removeValue(forKey: chat.id)?.stop()
        recentSessionIds.removeAll { $0 == chat.id }
        return notice
    }

    func chats(in spaceId: String) -> [Chat] {
        data?.chats(in: spaceId) ?? []
    }

    func chat(id: String) -> Chat? {
        data?.chat(id: id)
    }

    /// state.rs `space_for_chat` — nil for a dangling/missing space_id.
    func space(for chat: Chat) -> Space? {
        guard let spaceId = chat.spaceId else { return nil }
        return spaces.first { $0.id == spaceId }
    }

    func indicator(for chat: Chat) -> ChatIndicator {
        data?.indicator(for: chat) ?? .idle
    }

    func spaceIndicator(_ spaceId: String) -> ChatIndicator? {
        ChatIndicator.projectSummary(chats(in: spaceId).map { indicator(for: $0) })
    }

    func deviceName(_ deviceId: String) -> String {
        data?.devices.first { $0.id == deviceId }?.name ?? deviceId
    }

    func deviceOnline(_ deviceId: String) -> Bool {
        data?.deviceOnline(deviceId) ?? false
    }

    /// The session row (live status, context usage) behind a chat.
    func sessionRow(chatId: String) -> SessionRow? {
        data?.sessions[chatId]
    }

    /// The Pi slash commands on the chat's host device.
    func listCommands(deviceId: String) async throws -> [SlashCommand] {
        guard let data else { throw RelayError.hostOffline }
        return try await data.slashCommands(deviceId: deviceId)
    }

    /// What the chat's Pi switches are set to, for the `/` menu's badges.
    func piSessionModes(deviceId: String, chatId: String) async throws -> PiSessionModes {
        guard let data else { throw RelayError.hostOffline }
        return try await data.piSessionModes(deviceId: deviceId, chatId: chatId)
    }

    /// SearchFiles on the device that owns the checkout, for `@` mentions.
    func searchFiles(_ scope: MentionScope.Files, query: String) async throws -> [FileSearchMatch] {
        guard let data else { throw RelayError.hostOffline }
        return try await data.searchFiles(scope, query: query)
    }

    /// The `@session` references' snapshots, in mention order (composer.rs
    /// send path). Each comes from this phone's synced copy of the session,
    /// so a sleeping laptop's sessions stay referenceable; a copy that has
    /// never synced fails the send rather than dropping the reference.
    func sessionReferences(_ ids: [String]) async throws -> [SessionReference] {
        var references: [SessionReference] = []
        for id in ids {
            guard let chat = chat(id: id) else {
                throw SessionReferenceError(
                    "A referenced session no longer exists — remove the @session reference and try again.")
            }
            references.append(
                SessionReference(
                    title: Mentions.sessionTitle(chat),
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
                throw SessionReferenceError(
                    "This phone has no synced copy of “\(Mentions.sessionTitle(chat))” yet — try again once it loads.")
            }
        }
        return SessionReferences.boundedContext(await store.referenceEntries()) ?? ""
    }

    /// The models the chat's host offers; never a local substitute.
    func listPiModels(deviceId: String) async throws -> [ModelInfo] {
        guard let data else { throw PiCatalogError.unavailable }
        return try await data.piModels(deviceId: deviceId)
    }

    /// Refs of the space's repo (git spaces only).
    func listRefs(space: Space) async -> [RepoRef]? {
        await data?.listRefs(deviceId: space.deviceId, repoPath: space.path)
    }

    /// Draft-mode checkout switch: `git checkout` in the SPACE's folder.
    /// Returns an error message, or nil on success.
    func switchSpaceRef(space: Space, refName: String) async -> String? {
        guard let data else { return "Not connected" }
        return await data.switchRef(deviceId: space.deviceId, repoPath: space.path, refName: refName)
    }

    /// CreateWorktree off the base ref; returns the new worktree's path.
    func createWorktree(space: Space, base: String) async -> String? {
        await data?.createWorktree(deviceId: space.deviceId, repoPath: space.path, branch: base)
    }

    @discardableResult
    func createChat(
        space: Space, config chatConfig: ChatConfig,
        branch: String? = nil, cwd: String? = nil
    ) -> String? {
        data?.createChat(space: space, config: chatConfig, branch: branch, cwd: cwd)
    }

    /// Browse folders on a remote device (the desktop add-space palette's data
    /// path); nil path = the device's home directory.
    func listFolders(deviceId: String, path: String?) async -> FolderListing? {
        await data?.listFolders(deviceId: deviceId, path: path)
    }

    @discardableResult
    func createSpace(deviceId: String, path: String, gitDetected: Bool = false) async -> String? {
        await data?.createSpace(deviceId: deviceId, path: path, gitDetected: gitDetected)
    }

    /// Archived chats under the same scope as the list above the shelf.
    func archivedChats(in spaceId: String? = nil) -> [Chat] {
        data?.archivedChats(in: spaceId) ?? []
    }

    func archive(chatId: String) { data?.setArchived(chatId: chatId, archived: true) }
    func unarchive(chatId: String) { data?.setArchived(chatId: chatId, archived: false) }

    func setChatConfig(chatId: String, config: ChatConfig) {
        data?.setChatConfig(chatId: chatId, config: config)
    }

    func markSeen(chatId: String) {
        data?.markSeen(chatId: chatId)
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
            let key =
                path.status == .satisfied
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

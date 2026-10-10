// The demo dataset as the app's workspace: in-memory writes, and host calls
// answered from the fake filesystem after a short delay that feels like a
// network hop.

import Foundation

extension DemoDataset: WorkspaceData {
    var connected: Bool { true }

    func deviceOnline(_ deviceId: String) -> Bool {
        guard let seen = devices.first(where: { $0.id == deviceId })?.lastSeenAt else { return false }
        return nowMs() - seen < presenceFreshMs
    }

    // MARK: Writes

    @discardableResult
    func createChat(space: Space, config: ChatConfig, branch: String? = nil, cwd: String? = nil) -> String {
        let id = "chat-\(UUID().uuidString.lowercased().prefix(8))"
        chats.append(
            Chat(
                id: id, deviceId: space.deviceId, title: nil, archived: false,
                cwd: cwd ?? space.path, branch: branch, checkoutId: nil,
                config: config, lastMessagePreview: nil, lastMessageAt: nil,
                createdAt: nowMs(), spaceId: space.id, lastSeenAt: nowMs()))
        return id
    }

    func createSpace(deviceId: String, path: String, gitDetected: Bool = false) async -> String {
        if let existing = spaces.first(where: { $0.deviceId == deviceId && $0.path == path }) {
            return existing.id
        }
        let id = "space-\(UUID().uuidString.lowercased().prefix(8))"
        spaces.append(
            Space(
                id: id, deviceId: deviceId, path: path, name: nil,
                gitDetected: gitDetected, gitCheckedAt: nil, checkoutId: nil,
                createdAt: nowMs()))
        return id
    }

    func createQuickChat(deviceId: String, config: ChatConfig) async throws -> String {
        let chatId = UUID().uuidString.lowercased()
        try? await Task.sleep(nanoseconds: 200_000_000)
        chats.append(
            Chat(
                id: chatId, deviceId: deviceId, title: nil, archived: false,
                cwd: "/tmp/cypher-scratch/\(chatId)", branch: nil, checkoutId: nil,
                config: config, lastMessagePreview: nil, lastMessageAt: nil,
                createdAt: nowMs(), spaceId: nil, lastSeenAt: nowMs()))
        return chatId
    }

    /// Copies the transcript up to the anchor into a new chat; forking before
    /// a user message hands its text back as the draft.
    func fork(_ chat: Chat, anchor: MessageEntry, requestId: String) async throws -> ForkResponse {
        let entries = sessionStore(for: chat.id).entries
        guard let ix = entries.firstIndex(where: { $0.id == anchor.id }) else {
            return .unavailable(message: "Message not found")
        }
        let isUser = anchor.role == .user
        let copied = Array(entries.prefix(isUser ? ix : ix + 1))
        let id = "chat-\(UUID().uuidString.lowercased().prefix(8))"
        var fork = chat
        fork.id = id
        fork.title = String("\(chat.displayTitle) — Fork".prefix(120))
        fork.createdAt = nowMs()
        fork.lastMessageAt = nowMs()
        fork.lastSeenAt = nowMs()
        chats.append(fork)
        sessionStore(for: id).setEntries(copied)
        var draft: String?
        if isUser {
            let text = anchor.parts.compactMap { part -> String? in
                if case .text(_, let t, _) = part { return t }
                return nil
            }.joined(separator: "\n")
            draft = parseUserMessageImages(text).text
        }
        return .created(chatId: id, title: fork.title, composerText: draft)
    }

    func deleteChat(_ chat: Chat, hostOnline: Bool) async -> String? {
        chats.removeAll { $0.id == chat.id || $0.child?.parentChatId == chat.id }
        return nil
    }

    func rename(chatId: String, title: String) {
        if let ix = chats.firstIndex(where: { $0.id == chatId }) { chats[ix].title = title }
    }

    func setArchived(chatId: String, archived: Bool) {
        if let ix = chats.firstIndex(where: { $0.id == chatId }) { chats[ix].archived = archived }
    }

    func setChatConfig(chatId: String, config: ChatConfig) {
        if let ix = chats.firstIndex(where: { $0.id == chatId }) { chats[ix].config = config }
    }

    func markSeen(chatId: String) {
        if let ix = chats.firstIndex(where: { $0.id == chatId }) { chats[ix].lastSeenAt = nowMs() }
    }

    // MARK: Host calls

    func sideChat(parent: Chat, quote: String, anchorEntryId: String?, config: AppConfig?) -> SideChatStore? {
        SideChatStore(
            parent: parent, quote: quote, anchorEntryId: anchorEntryId,
            relay: nil, config: Self.dummyConfig, demo: self)
    }

    func listFolders(deviceId: String, path: String?) async -> FolderListing? {
        try? await Task.sleep(nanoseconds: 120_000_000)
        return folderListing(at: path ?? homePath(deviceId: deviceId))
    }

    func listRefs(deviceId: String, repoPath: String) async -> [RepoRef]? {
        try? await Task.sleep(nanoseconds: 120_000_000)
        return refs(spacePath: repoPath)
    }

    func switchRef(deviceId: String, repoPath: String, refName: String) async -> String? {
        try? await Task.sleep(nanoseconds: 200_000_000)
        checkOut(refName, in: repoPath)
        return nil
    }

    func createWorktree(deviceId: String, repoPath: String, branch: String) async -> String? {
        try? await Task.sleep(nanoseconds: 250_000_000)
        return addWorktree(spacePath: repoPath, base: branch)
    }

    func slashCommands(deviceId: String) async throws -> [SlashCommand] {
        try? await Task.sleep(nanoseconds: 150_000_000)
        return Self.slashCommands
    }

    func piSessionModes(deviceId: String, chatId: String) async throws -> PiSessionModes {
        Self.piSessionModes
    }

    func searchFiles(_ scope: MentionScope.Files, query: String) async throws -> [FileSearchMatch] {
        try? await Task.sleep(nanoseconds: 120_000_000)
        return fileMatches(query)
    }

    func piModels(deviceId: String) async throws -> [ModelInfo] {
        HarnessCatalog.demoModels
    }
}

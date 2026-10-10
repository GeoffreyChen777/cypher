// The workspace views read and write: the live registry mirror
// (WorkspaceStore) or the offline demo (DemoDataset). AppModel forwards to
// whichever is active, so no accessor special-cases demo mode; the derived
// session lists are computed once, here, for both.

import Foundation

@MainActor
protocol WorkspaceData: AnyObject {
    var devices: [DeviceRow] { get }
    var spaces: [Space] { get }
    var chats: [Chat] { get }
    var sessions: [String: SessionRow] { get }
    var connected: Bool { get }
    func deviceOnline(_ deviceId: String) -> Bool

    // MARK: Writes

    @discardableResult
    func createChat(space: Space, config: ChatConfig, branch: String?, cwd: String?) -> String
    /// Dedups on (device, path) and returns the existing space's id.
    func createSpace(deviceId: String, path: String, gitDetected: Bool) async -> String
    /// Quick chat: a scratch folder on the device, then a chat without a
    /// project there. Returns the chat id.
    func createQuickChat(deviceId: String, config: ChatConfig) async throws -> String
    func fork(_ chat: Chat, anchor: MessageEntry, requestId: String) async throws -> ForkResponse
    /// Returns a notice when part of the cleanup couldn't happen.
    func deleteChat(_ chat: Chat, hostOnline: Bool) async -> String?
    func rename(chatId: String, title: String)
    func setArchived(chatId: String, archived: Bool)
    func setChatConfig(chatId: String, config: ChatConfig)
    func markSeen(chatId: String)

    // MARK: Host calls

    func sideChat(parent: Chat, quote: String, anchorEntryId: String?, config: AppConfig?) -> SideChatStore?
    func listFolders(deviceId: String, path: String?) async -> FolderListing?
    func listRefs(deviceId: String, repoPath: String) async -> [RepoRef]?
    /// `git checkout` in a folder; returns git's error, nil on success.
    func switchRef(deviceId: String, repoPath: String, refName: String) async -> String?
    func createWorktree(deviceId: String, repoPath: String, branch: String) async -> String?
    /// The Pi slash commands on a device.
    func slashCommands(deviceId: String) async throws -> [SlashCommand]
    func piSessionModes(deviceId: String, chatId: String) async throws -> PiSessionModes
    /// `@` mention candidates in a checkout.
    func searchFiles(_ scope: MentionScope.Files, query: String) async throws -> [FileSearchMatch]
    /// The models a device's installed, enabled Pi offers.
    func piModels(deviceId: String) async throws -> [ModelInfo]
}

extension WorkspaceData {
    func chat(id: String) -> Chat? {
        chats.first { $0.id == id }
    }

    /// state.rs `overview_chats`: every non-archived chat of a live space,
    /// attention-sorted.
    var overviewChats: [Chat] {
        let liveSpaceIds = Set(spaces.map(\.id))
        let live = chats.filter { !$0.isChild && !$0.archived && $0.spaceId.map(liveSpaceIds.contains) == true }
        return sortActive(live)
    }

    /// A space's sessions, in the sidebar's Sessions order (recency).
    ///
    /// NOT desktop's `chats_in_space`, which is creation order because there
    /// the rows are TABS and activity must never reorder tabs. The phone has
    /// no tabs — a space opens into the same list, with the same rows, as the
    /// Sessions section — so it follows that list's ordering instead.
    func chats(in spaceId: String) -> [Chat] {
        sortActive(chats.filter { !$0.isChild && !$0.archived && $0.spaceId == spaceId })
    }

    /// Active sessions outside any live project (desktop "No project"
    /// groups: project-less chats, or ones whose project was removed).
    /// Quick chats are listed on their own.
    var projectlessChats: [Chat] {
        let liveSpaceIds = Set(spaces.map(\.id))
        return sortActive(
            chats.filter {
                !$0.isChild && !$0.archived && !$0.isScratch
                    && !($0.spaceId.map(liveSpaceIds.contains) ?? false)
            })
    }

    /// Active quick chats, every device merged (state.rs merge_scratch_groups).
    var quickChats: [Chat] {
        sortActive(chats.filter { !$0.isChild && !$0.archived && $0.isScratch })
    }

    /// Archived chats under an optional space scope, recency order — feeds the
    /// Archived shelf (shell/spaces.rs `render_archived_section`). Unlike
    /// `overviewChats`, a live space is not required: an archived session of a
    /// deleted space should still be reachable for unarchive.
    func archivedChats(in spaceId: String? = nil) -> [Chat] {
        sortActive(chats.filter { !$0.isChild && $0.archived && (spaceId == nil || $0.spaceId == spaceId) })
    }

    func indicator(for chat: Chat) -> ChatIndicator {
        chatIndicator(chat: chat, live: effectiveStatus(sessions[chat.id], now: nowMs()))
    }

    /// Aggregate active sessions for the project's trailing status indicator.
    func spaceIndicator(_ spaceId: String) -> ChatIndicator? {
        ChatIndicator.projectSummary(chats(in: spaceId).map { indicator(for: $0) })
    }
}

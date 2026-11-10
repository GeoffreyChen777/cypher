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
    /// Restart the conversation at `anchor` inside the same chat: the host
    /// deletes everything after it (and a user anchor itself).
    func rewind(_ chat: Chat, anchor: MessageEntry) async throws -> RewindResponse
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
    /// A device's permission settings (Settings → Permissions).
    func permissionSettings(deviceId: String) async throws -> PermissionSnapshot
    /// Save them; the engine answers with what it saved.
    func savePermissionSettings(deviceId: String, settings: PermissionSettings) async throws -> PermissionSnapshot

    // MARK: Workspaces (live: WorkspaceStore+Bots.swift; demo: DemoDataset+Bots.swift)

    /// Every workspace, oldest first.
    var bots: [Bot] { get }
    /// Every workspace record (derived lists: WorkspaceData+Bots.swift).
    var botRecords: BotRecords { get }

    // Owner fields: local registry writes, instant and offline-safe (the
    // host watches the rows and acts on them). Each throws a
    // `BotActionError` with a sentence to show.

    /// Approve or cancel a plan that still awaits approval.
    func resolvePlan(workId: String, approve: Bool) async throws
    /// Stop running work and every session it started.
    func stopWork(workId: String) async throws
    func resolveApproval(approvalId: String, approve: Bool) async throws
    /// Run a routine once now.
    func runRoutine(routineId: String) async throws
    func setRoutineEnabled(routineId: String, enabled: Bool) async throws
    /// A connection's reply mode and narrowed scope (nil = the workspace's).
    func updateConnection(connectionId: String, replyMode: ReplyMode, scope: BotScope?) async throws
    /// Revoke on both sides (the host tells the other side).
    func revokeConnection(connectionId: String) async throws
    func revokeInvite(inviteId: String) async throws
    /// Send (with the owner's edited `text`, if any) or decline a drafted
    /// answer.
    func reviewExchange(exchangeId: String, send: Bool, text: String?) async throws
    /// The kill switch: no routine runs and no new work starts.
    func setBotPaused(botId: String, paused: Bool) async throws
    func deleteShortcut(shortcutId: String) async throws
    /// Put a workspace's shortcuts in this order (ids, first to last).
    func orderShortcuts(botId: String, ids: [String]) async throws

    // Engine calls over the relay: the workspace's host when it is online,
    // else (for registry writes the engine validates) any online device.

    /// Create a workspace hosted on `deviceId`; returns it.
    func createBot(_ request: CreateBotRequest, on deviceId: String) async throws -> Bot
    /// The settings form's save: every editable field at once (nil clears
    /// an optional one; nil capabilities leave them alone).
    func updateBot(
        botId: String, name: String, icon: String?, color: String?, instructions: String?,
        scope: BotScope, capabilities: BotCapabilities?
    ) async throws
    /// Delete a workspace with its chats and records.
    func deleteBot(botId: String) async throws
    /// Create or edit a routine's definition; throws the engine's
    /// validation message.
    func saveRoutine(_ routine: BotRoutine) async throws
    /// Delete a routine with its sessions.
    func deleteRoutine(routineId: String) async throws
    /// Delete ended work and its background session.
    func deleteWork(workId: String) async throws
    /// A one-time invite code (the host publishes the workspace first).
    func createInvite(botId: String) async throws -> InviteCreated
    /// Connect to another user's workspace with their code.
    func redeemInvite(botId: String, code: String) async throws -> BotConnection
    /// One memory store, live: a view per change until cancelled.
    func watchMemory(scope: MemoryScope) -> AsyncThrowingStream<MemoryView, Error>
    /// One edit; returns the engine's one-line result.
    func editMemory(scope: MemoryScope, edit: MemoryEdit) async throws -> String
}

extension Chat {
    /// A root session the lists show: not a subagent child, a workspace's
    /// chat or thread, or a memory pass.
    var isListedSession: Bool { !isChild && !isHiddenFromSessions }
}

extension WorkspaceData {
    func chat(id: String) -> Chat? {
        chats.first { $0.id == id }
    }

    /// state.rs `overview_chats`: every non-archived chat of a live space,
    /// attention-sorted.
    var overviewChats: [Chat] {
        let liveSpaceIds = Set(spaces.map(\.id))
        let live = chats.filter {
            $0.isListedSession && !$0.archived && $0.spaceId.map(liveSpaceIds.contains) == true
        }
        return sortActive(live)
    }

    /// Every active session — projects, project-less and quick chats — in
    /// recency order: Home's list.
    var activeChats: [Chat] {
        sortActive(chats.filter { $0.isListedSession && !$0.archived })
    }

    /// A space's sessions, in the sidebar's Sessions order (recency).
    ///
    /// NOT desktop's `chats_in_space`, which is creation order because there
    /// the rows are TABS and activity must never reorder tabs. The phone has
    /// no tabs — a space opens into the same list, with the same rows, as the
    /// Sessions section — so it follows that list's ordering instead.
    func chats(in spaceId: String) -> [Chat] {
        sortActive(chats.filter { $0.isListedSession && !$0.archived && $0.spaceId == spaceId })
    }

    /// Active sessions outside any live project (desktop "No project"
    /// groups: project-less chats, or ones whose project was removed).
    /// Quick chats are listed on their own.
    var projectlessChats: [Chat] {
        let liveSpaceIds = Set(spaces.map(\.id))
        return sortActive(
            chats.filter {
                $0.isListedSession && !$0.archived && !$0.isScratch
                    && !($0.spaceId.map(liveSpaceIds.contains) ?? false)
            })
    }

    /// Active quick chats, every device merged (state.rs merge_scratch_groups).
    var quickChats: [Chat] {
        sortActive(chats.filter { $0.isListedSession && !$0.archived && $0.isScratch })
    }

    /// Archived chats under an optional space scope, recency order — feeds the
    /// Archived shelf (shell/spaces.rs `render_archived_section`). Unlike
    /// `overviewChats`, a live space is not required: an archived session of a
    /// deleted space should still be reachable for unarchive.
    func archivedChats(in spaceId: String? = nil) -> [Chat] {
        sortActive(chats.filter { $0.isListedSession && $0.archived && (spaceId == nil || $0.spaceId == spaceId) })
    }

    func indicator(for chat: Chat) -> ChatIndicator {
        chatIndicator(chat: chat, live: effectiveStatus(sessions[chat.id], now: nowMs()))
    }

    /// Aggregate active sessions for the project's trailing status indicator.
    func spaceIndicator(_ spaceId: String) -> ChatIndicator? {
        ChatIndicator.projectSummary(chats(in: spaceId).map { indicator(for: $0) })
    }
}

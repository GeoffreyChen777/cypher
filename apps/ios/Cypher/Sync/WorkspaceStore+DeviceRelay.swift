// WorkspaceStore's direct host RPCs over the device relay: folder browsing,
// refs, worktrees, model and command catalogs, file search and forks.

import Foundation

extension WorkspaceStore {
    /// ForkSession on the source chat's host (session_forks.rs). The host
    /// builds the new Pi session and mints its row; idempotent per
    /// `requestId`, which is also the new chat's id. The helper can take
    /// ~30s, plus quiescing the source.
    func fork(_ chat: Chat, anchor: MessageEntry, requestId: String) async throws -> ForkResponse {
        try await relayClient(for: chat.deviceId).call(method: "ForkSession", params: [
            "requestId": requestId,
            "sourceChatId": chat.id,
            "anchorMessageId": anchor.id,
        ], timeoutSeconds: 90)
    }

    /// A side chat talks to the parent's host directly.
    func sideChat(parent: Chat, quote: String, anchorEntryId: String?, config: AppConfig?) -> SideChatStore? {
        guard let config else { return nil }
        return SideChatStore(parent: parent, quote: quote, anchorEntryId: anchorEntryId,
                             relay: relayClient(for: parent.deviceId), config: config)
    }

    /// ListFolders on the target device (engine caps at 500 entries, hides
    /// dotfiles, stamps isRepo). nil path = the device's home directory.
    func listFolders(deviceId: String, path: String?) async -> FolderListing? {
        try? await listFoldersDetailed(deviceId: deviceId, path: path)
    }

    func listFoldersDetailed(deviceId: String, path: String?) async throws -> FolderListing {
        var params: [String: Any] = [:]
        if let path { params["path"] = path }
        return try await relayClient(for: deviceId).call(method: "ListFolders", params: params)
    }

    /// Only the read-only browser's fixed RPC set, on the chat's host device.
    func workspaceBrowserCall<T: Decodable & Sendable>(deviceId: String, method: String,
                                            params: [String: Any]) async throws -> T {
        guard ["ListWorkspaceFiles", "ReadWorkspaceFile", "GetCheckoutDiff"].contains(method) else {
            throw RelayError.notConnected
        }
        return try await relayClient(for: deviceId).call(method: method, params: params, timeoutSeconds: 30)
    }

    /// ListRefs on the target device — branches with current/worktree markers
    /// (default branch first, per the engine's ordering).
    func listRefs(deviceId: String, repoPath: String) async -> [RepoRef]? {
        try? await relayClient(for: deviceId).call(method: "ListRefs", params: ["repoPath": repoPath])
    }

    /// The phone never resolves a local Runtime or substitutes a model list.
    func piModels(deviceId: String) async throws -> [ModelInfo] {
        #if CYPHER_DEVELOPMENT
        // `-mock-providers` (dev-ios.sh argument): the engine's catalog plus
        // mock providers, or the mocks alone when the engine is unreachable.
        if ProcessInfo.processInfo.arguments.contains("-mock-providers") {
            let real = (try? await listPiModels(deviceId: deviceId)) ?? []
            let mocked = HarnessCatalog.mockProviderModels.filter { mock in !real.contains { $0.id == mock.id } }
            return real + mocked
        }
        #endif
        guard deviceOnline(deviceId) else { throw PiCatalogError.unavailable }
        return try await listPiModels(deviceId: deviceId)
    }

    /// Only the target engine's installed/enabled Pi models may be offered.
    /// Empty catalogs and transport errors are not replaced with static data.
    private func listPiModels(deviceId: String) async throws -> [ModelInfo] {
        let wire: [PiHarnessDescriptor] = try await relayClient(for: deviceId)
            .call(method: "ListHarnesses", params: [:])
        guard wire.contains(where: \.available) else {
            throw PiCatalogError.runtimeUnavailable
        }
        return try await listModels(deviceId: deviceId, harness: "pi")
    }

    /// Raw RPC also used by the isolated mock E2E rig. Production UI calls
    /// listPiModels, which applies the installed/enabled Pi gate first.
    func listModels(deviceId: String, harness: String) async throws -> [ModelInfo] {
        struct WireModel: Decodable {
            var id: String
            var label: String
            var description: String?
            var reasoningLevels: [String]?
        }
        let wire: [WireModel] = try await relayClient(for: deviceId)
            .call(method: "ListModels", params: ["harness": harness])
        var seen = Set<String>()
        return wire.filter { !$0.id.isEmpty && seen.insert($0.id).inserted }.map {
            ModelInfo(id: $0.id, label: $0.label, description: $0.description,
                      reasoningLevels: $0.reasoningLevels ?? [])
        }
    }

    /// Pi's slash commands on the target device (Pi discovers them by
    /// spawning itself — a cold call can take most of 10s, hence the longer
    /// deadline). Forwardable, so the host answers directly.
    func slashCommands(deviceId: String) async throws -> [SlashCommand] {
        guard deviceOnline(deviceId) else { throw RelayError.hostOffline }
        return try await relayClient(for: deviceId)
            .call(method: "ListCommands", params: ["harness": "pi"], timeoutSeconds: 20)
    }

    /// SearchFiles — `@` mention candidates in a chat's or space's checkout
    /// (paths only, never contents). Forwardable, so the host answers.
    func searchFiles(_ scope: MentionScope.Files, query: String) async throws -> [FileSearchMatch] {
        guard deviceOnline(scope.deviceId) else { throw RelayError.hostOffline }
        var params = scope.params
        params["query"] = query
        return try await relayClient(for: scope.deviceId).call(method: "SearchFiles", params: params, timeoutSeconds: 15)
    }

    /// PiSessionModes — the Pi plugins' switches for one chat (Fast mode,
    /// Scripts, orchestration, the goal), read from its Pi session file on
    /// the host. Forwardable, so the host answers directly.
    func piSessionModes(deviceId: String, chatId: String) async throws -> PiSessionModes {
        guard deviceOnline(deviceId) else { throw RelayError.hostOffline }
        return try await relayClient(for: deviceId).call(method: "PiSessionModes", params: ["chatId": chatId])
    }

    /// SwitchRef — `git checkout` in the given folder on the target device.
    /// Returns git's error message on failure (dirty tree, held ref, …).
    func switchRef(deviceId: String, repoPath: String, refName: String) async -> String? {
        struct Reply: Decodable { var branch: String? }
        do {
            let _: Reply = try await relayClient(for: deviceId)
                .call(method: "SwitchRef", params: ["repoPath": repoPath, "refName": refName])
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// CreateWorktree — a fresh isolated worktree off the base ref; returns
    /// its path.
    func createWorktree(deviceId: String, repoPath: String, branch: String) async -> String? {
        struct Reply: Decodable { var path: String }
        let reply: Reply? = try? await relayClient(for: deviceId)
            .call(method: "CreateWorktree", params: ["repoPath": repoPath, "branch": branch])
        return reply?.path
    }
}

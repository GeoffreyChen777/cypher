// Workspace registry mirror — the iOS analogue of the desktop's RegistryDoc
// host (crates/doc/src/registry.rs + crates/engine WorkspaceHost). Joins the
// per-user `/registry/{orgId}/ws` room, projects the row table into typed
// rows, and performs the writes the writer discipline allows a viewer device:
// chat creates, archives, renames, seen marks, and device/space deletes. iOS is a viewport, not an
// engine device, so it owns no device row; it does publish a presence beat
// (registry presence replaced the old ws room's ephemeral store).
//
// Reads are OVERLAY reads: the server's authoritative rows plus the pending
// op-batch queue replayed on top (optimistic local writes, retired on ack).
// Field names are identical to the old Loro rows (camelCase, epoch-ms
// timestamps) so the projection — and every view above it — is unchanged.

import Foundation
import Observation

@MainActor
@Observable
final class WorkspaceStore {
    private(set) var devices: [DeviceRow] = []
    private(set) var spaces: [Space] = []
    private(set) var chats: [Chat] = []
    private(set) var sessions: [String: SessionRow] = [:]
    private(set) var presence: [String: Int64] = [:]  // deviceId → last beat ms
    private(set) var connected = false

    /// Presence entries older than this are expired (mirrors the Rust
    /// client's 30s TTL, measured from RECEIPT — beats carry the sender's
    /// wall clock, which we never trust for freshness).
    static let presenceTtlMs: Int64 = 30_000

    /// Chats being deleted: hidden at once while the host is told first
    /// (see `deleteChat`), so the list never waits on the network.
    @ObservationIgnored private var deleting: Set<String> = []
    @ObservationIgnored private var doc: RegistryDoc
    @ObservationIgnored private var client: RegistryClient?
    @ObservationIgnored private var saver: RegistrySaver?
    @ObservationIgnored private var presenceReceivedAt: [String: Int64] = [:]
    private let config: AppConfig

    /// `pendingActivity` lets the registry room's presence beat carry the
    /// viewport's periodic activity refresh. That beat already flows every 15s
    /// and bills 20:1, so the refresh rides free; sent over HTTP it cost a full
    /// billable request every time.
    init(config: AppConfig, initialDocument: RegistryDoc? = nil,
         pendingActivity: @escaping @MainActor @Sendable () -> ActivityReport? = { nil }) {
        self.config = config
        self.doc = initialDocument ?? RegistryDoc(deviceId: config.deviceId)
        self.pendingActivity = pendingActivity
        project()
    }

    private let pendingActivity: @MainActor @Sendable () -> ActivityReport?

    func start() {
        guard client == nil else { return }
        // Local-first: hydrate from the on-device blob before joining — the
        // sidebar renders immediately and the hello backfills from our
        // cursor. First run after the update: no blob → cursor null → the
        // server's full state (the engines already seeded everything).
        let blobURL = DocDisk.registryURL(orgId: config.orgId, userId: config.userId)
        if let data = try? Data(contentsOf: blobURL),
           let loaded = try? RegistryDoc.from(data: data, deviceId: config.deviceId) {
            doc = loaded
        }
        project()
        saver = RegistrySaver(url: blobURL) { [weak self] in
            try? self?.doc.toData()
        }

        let delegate = RegistryClient.Delegate(
            helloCursor: { [weak self] in self?.doc.helloCursor ?? nil },
            takePushable: { [weak self] in self?.doc.takePushable() ?? [] },
            resetPushable: { [weak self] in
                self?.doc.markDisconnected()
            },
            acknowledge: { [weak self] batch, seq in
                guard let self else { return }
                self.doc.ackBatch(batch, seq: seq)
                self.project()
                self.saver?.poke()
            },
            event: { [weak self] event in self?.handle(event) },
            pendingActivity: { [pendingActivity] in pendingActivity() }
        )
        let client = RegistryClient(device: config.deviceId,
                                    urlProvider: { [config] in await config.registrySocketURL() },
                                    rowsRequest: { [config] since in
                                        await config.registryRowsRequest(since: since)
                                    },
                                    pushRequest: { [config] in
                                        await config.registryPushRequest()
                                    },
                                    delegate: delegate)
        self.client = client
        Task { await client.start() }
    }

    /// Backgrounding hook: persist immediately.
    func flushToDisk() {
        saver?.flush()
    }

    /// Foreground hook: revive the room after a suspension (see
    /// RegistryClient.kick).
    func kickRoom() {
        guard let client else { return }
        Task { await client.kick() }
    }

    func stop() {
        saver?.flush()
        if let client {
            Task { await client.stop() }
        }
        client = nil
        connected = false
    }

    // MARK: Server events (delivered in frame order — rows before ack)

    private func handle(_ event: RegistryEvent) {
        switch event {
        case .state(let seq, let full, let gcFloor, let rows, let beats):
            // On a state frame with full=true and seq < our cursor (server
            // wiped), applyState keeps local rows and re-seeds them as
            // upserts carrying their ORIGINAL per-field clocks; the client
            // pushes those pending batches right after this returns.
            let outcome = doc.applyState(seq: seq, full: full, gcFloor: gcFloor, rows: rows)
            if outcome == .reseeded {
                roomLog.info("registry: server behind local state; re-seeding")
            }
            let now = nowMs()
            for (device, at) in beats {
                presence[device] = at
                presenceReceivedAt[device] = now
            }
            project()
            saver?.poke()
        case .connected:
            connected = true
        case .rows(let seq, let rows):
            let contiguous = doc.applyRows(seq: seq, rows: rows)
            project()
            saver?.poke()
            if !contiguous {
                roomLog.warning("registry: broadcast seq gap (seq=\(seq)); redialing")
                if let client {
                    Task { await client.redial() }
                }
            }
        case .ack(let batch, let seq, _):
            // Rows for this batch already arrived (server orders rows before
            // ack), so retiring the optimistic overlay can't flicker — and if
            // our op lost LWW, the merged row is now the truth on display.
            doc.ackBatch(batch, seq: seq)
            project()
            saver?.poke()
        case .presence(let device, let at):
            presence[device] = at
            presenceReceivedAt[device] = nowMs()
        case .disconnected:
            connected = false
            doc.markDisconnected()
        }
    }

    /// Every local write: re-project the overlay, schedule the snapshot, and
    /// wake the client to push the fresh batch.
    private func afterLocalWrite() {
        project()
        saver?.poke()
        if let client {
            Task { await client.nudge() }
        }
    }

    // MARK: Presence

    func deviceOnline(_ deviceId: String) -> Bool {
        guard let received = presenceReceivedAt[deviceId] else { return false }
        return nowMs() - received < Self.presenceTtlMs
    }

    // MARK: Projection (rows → typed entities)

    private func project() {
        devices = doc.overlayRows(kind: "devices").map { row in
            let f = row.fields
            let id = f["id"]?.stringValue ?? row.id
            return DeviceRow(id: id,
                             name: f["name"]?.stringValue ?? id,
                             platform: f["platform"]?.stringValue ?? "",
                             lastSeenAt: f["lastSeenAt"]?.int64Value,
                             createdAt: f["createdAt"]?.int64Value)
        }.sorted { $0.name < $1.name }

        spaces = doc.overlayRows(kind: "spaces").compactMap { row in
            let f = row.fields
            guard let deviceId = f["deviceId"]?.stringValue,
                  let path = f["path"]?.stringValue else { return nil }
            return Space(id: f["id"]?.stringValue ?? row.id, deviceId: deviceId, path: path,
                         name: f["name"]?.stringValue,
                         gitDetected: f["gitDetected"]?.boolValue ?? false,
                         gitCheckedAt: f["gitCheckedAt"]?.int64Value,
                         checkoutId: f["checkoutId"]?.stringValue,
                         createdAt: f["createdAt"]?.int64Value ?? 0)
        }.sorted { ($0.createdAt, $0.id) < ($1.createdAt, $1.id) }  // creation order, id tiebreak

        chats = doc.overlayRows(kind: "chats").compactMap { row in
            let f = row.fields
            guard let deviceId = f["deviceId"]?.stringValue,
                  !deleting.contains(f["id"]?.stringValue ?? row.id) else { return nil }
            let child = SubagentProjection.decode(f["child"], as: ChildChat.self)
            // Don't promote a malformed child relation into the root list.
            if let rawChild = f["child"], rawChild != .null, child == nil { return nil }
            var chatConfig: ChatConfig?
            if let c = f["config"]?.objectValue {
                chatConfig = ChatConfig(harness: c["harness"]?.stringValue ?? "claude-code",
                                        model: c["model"]?.stringValue,
                                        reasoning: c["reasoning"]?.stringValue,
                                        modelOptions: c["modelOptions"]?.objectValue ?? [:],
                                        sandbox: c["sandbox"]?.stringValue)
            }
            return Chat(id: f["id"]?.stringValue ?? row.id, deviceId: deviceId,
                        title: f["title"]?.stringValue,
                        archived: f["archived"]?.boolValue ?? false,
                        cwd: f["cwd"]?.stringValue,
                        branch: f["branch"]?.stringValue,
                        checkoutId: f["checkoutId"]?.stringValue,
                        config: chatConfig,
                        lastMessagePreview: f["lastMessagePreview"]?.stringValue,
                        lastMessageAt: f["lastMessageAt"]?.int64Value,
                        createdAt: f["createdAt"]?.int64Value ?? 0,
                        spaceId: f["spaceId"]?.stringValue,
                        lastSeenAt: f["lastSeenAt"]?.int64Value,
                        roomGen: f["roomGen"]?.int64Value.map(Int.init),
                        child: child)
        }

        var rows: [String: SessionRow] = [:]
        for row in doc.overlayRows(kind: "sessions") {
            let f = row.fields
            guard let chatId = f["chatId"]?.stringValue,
                  let deviceId = f["deviceId"]?.stringValue,
                  let statusStr = f["status"]?.stringValue,
                  let status = SessionStatus(rawValue: statusStr) else { continue }
            rows[chatId] = SessionRow(chatId: chatId, deviceId: deviceId, status: status,
                                      startedAt: f["startedAt"]?.int64Value,
                                      updatedAt: f["updatedAt"]?.int64Value ?? 0,
                                      subagents: SubagentProjection.snapshot(f["subagents"]),
                                      contextUsage: ContextUsage(f["contextUsage"]))
        }
        sessions = rows
    }

    // MARK: Derived views

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
        return sortActive(chats.filter {
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

    // MARK: Device relay (folder browsing / direct host RPCs)

    @ObservationIgnored private var relayClients: [String: DeviceRelayClient] = [:]

    /// The host relay, for features that manage their own calls (side chats).
    func relayClient(for deviceId: String) -> DeviceRelayClient {
        relay(for: deviceId)
    }

    /// ForkSession on the source chat's host (session_forks.rs). The host
    /// builds the new Pi session and mints its row; idempotent per
    /// `requestId`, which is also the new chat's id. The helper can take
    /// ~30s, plus quiescing the source.
    func forkSession(deviceId: String, requestId: String, sourceChatId: String,
                     anchorMessageId: String) async throws -> ForkResponse {
        try await relay(for: deviceId).call(method: "ForkSession", params: [
            "requestId": requestId,
            "sourceChatId": sourceChatId,
            "anchorMessageId": anchorMessageId,
        ], timeoutSeconds: 90)
    }

    private func relay(for deviceId: String) -> DeviceRelayClient {
        if let existing = relayClients[deviceId] { return existing }
        let client = DeviceRelayClient(deviceId: deviceId, config: config)
        relayClients[deviceId] = client
        return client
    }

    /// The last relay failure, for surfacing in UI/diagnostics.
    private(set) var lastRelayError: String?

    /// ListFolders on the target device (engine caps at 500 entries, hides
    /// dotfiles, stamps isRepo). nil path = the device's home directory.
    func listFolders(deviceId: String, path: String?) async -> FolderListing? {
        do {
            return try await listFoldersDetailed(deviceId: deviceId, path: path)
        } catch {
            lastRelayError = error.localizedDescription
            return nil
        }
    }

    func listFoldersDetailed(deviceId: String, path: String?) async throws -> FolderListing {
        var params: [String: Any] = [:]
        if let path { params["path"] = path }
        return try await relay(for: deviceId).call(method: "ListFolders", params: params)
    }

    /// Only the read-only browser's fixed RPC set, on the chat's host device.
    func workspaceBrowserCall<T: Decodable>(deviceId: String, method: String,
                                            params: [String: Any]) async throws -> T {
        guard ["ListWorkspaceFiles", "ReadWorkspaceFile", "GetCheckoutDiff"].contains(method) else {
            throw RelayError.notConnected
        }
        return try await relay(for: deviceId).call(method: method, params: params, timeoutSeconds: 30)
    }

    /// ListRefs on the target device — branches with current/worktree markers
    /// (default branch first, per the engine's ordering).
    func listRefs(deviceId: String, repoPath: String) async -> [RepoRef]? {
        try? await relay(for: deviceId).call(method: "ListRefs", params: ["repoPath": repoPath])
    }

    /// Only the target engine's installed/enabled Pi models may be offered.
    /// Empty catalogs and transport errors are not replaced with static data.
    func listPiModels(deviceId: String) async throws -> [ModelInfo] {
        let wire: [PiHarnessDescriptor] = try await relay(for: deviceId)
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
        let wire: [WireModel] = try await relay(for: deviceId)
            .call(method: "ListModels", params: ["harness": harness])
        var seen = Set<String>()
        return wire.filter { !$0.id.isEmpty && seen.insert($0.id).inserted }.map {
            ModelInfo(id: $0.id, label: $0.label, description: $0.description,
                      reasoningLevels: $0.reasoningLevels ?? [])
        }
    }

    /// The harness's slash commands on the target device (Pi discovers
    /// them by spawning itself — a cold call can take most of 10s, hence the
    /// longer deadline). Forwardable, so the host answers directly.
    func listCommands(deviceId: String, harness: String) async throws -> [SlashCommand] {
        try await relay(for: deviceId)
            .call(method: "ListCommands", params: ["harness": harness], timeoutSeconds: 20)
    }

    /// SwitchRef — `git checkout` in the given folder on the target device.
    /// Returns git's error message on failure (dirty tree, held ref, …).
    func switchRef(deviceId: String, repoPath: String, refName: String) async -> String? {
        struct Reply: Decodable { var branch: String? }
        do {
            let _: Reply = try await relay(for: deviceId)
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
        let reply: Reply? = try? await relay(for: deviceId)
            .call(method: "CreateWorktree", params: ["repoPath": repoPath, "branch": branch])
        return reply?.path
    }

    /// Retarget a session onto another checkout (the desktop's
    /// setChatCwd/setChatBranch mutates — LWW field writes here).
    func setChatCheckout(chatId: String, cwd: String, branch: String) {
        updateChat(chatId, set: ["cwd": .string(cwd), "branch": .string(branch)])
    }

    // MARK: Writes (viewer-device discipline → op batches)

    /// Mint a new chat onto a space (workspace_host.rs create_chat shape):
    /// a full-row upsert. The host = the space's owning device picks it up
    /// via the registry.
    @discardableResult
    func createChat(space: Space, config chatConfig: ChatConfig,
                    branch: String? = nil, cwd: String? = nil) -> String {
        let chatId = UUID().uuidString.lowercased()
        var set: [String: JSONValue] = [
            "id": .string(chatId),
            "deviceId": .string(space.deviceId),
            "archived": .bool(false),
            "cwd": .string(cwd ?? space.path),
            "spaceId": .string(space.id),
            "createdAt": .int(nowMs()),
            // Born on chat2 (workspace_host.rs create_chat): a brand-new
            // chat has an empty doc — nothing to seed, no migration race.
            "roomGen": .int(2),
        ]
        if let branch {
            set["branch"] = .string(branch)
        }
        if let cfg = JSONValue(encodable: chatConfig) {
            set["config"] = cfg
        }
        doc.write(kind: "chats", id: chatId, op: .upsert, set: set)
        afterLocalWrite()
        return chatId
    }

    /// Create a space. Preferred path: `Mutate {op:createSpace}` straight to
    /// the owning host over its relay (it applies the row to its own registry
    /// doc, functionally identical to the desktop's local mutate + sync).
    /// Fallback when the host is unreachable: a full-row upsert from here —
    /// creates are legal from any device; the owner stamps git on arrival.
    @discardableResult
    func createSpace(deviceId: String, path: String, gitDetected: Bool = false) async -> String {
        // Dedup on (device, path) like the desktop palette.
        if let existing = spaces.first(where: { $0.deviceId == deviceId && $0.path == path }) {
            return existing.id
        }
        let spaceId = UUID().uuidString.lowercased()
        struct OkReply: Decodable { var ok: Bool? }
        let params: [String: Any] = [
            "op": "createSpace",
            "spaceId": spaceId,
            "deviceId": deviceId,
            "path": path,
            "gitDetected": gitDetected,
        ]
        let viaHost: OkReply? = try? await relay(for: deviceId).call(method: "Mutate", params: params)
        if viaHost == nil {
            doc.write(kind: "spaces", id: spaceId, op: .upsert, set: [
                "id": .string(spaceId),
                "deviceId": .string(deviceId),
                "path": .string(path),
                "gitDetected": .bool(gitDetected),
                "createdAt": .int(nowMs()),
            ])
        }
        afterLocalWrite()
        return spaceId
    }

    func setArchived(chatId: String, archived: Bool) {
        updateChat(chatId, set: ["archived": .bool(archived)])
    }

    /// Synced seen marker (LWW) with a monotonic guard: no write when the
    /// stored stamp is already current.
    func markSeen(chatId: String) {
        guard let row = doc.overlayRow(kind: "chats", id: chatId) else { return }
        let at = nowMs()
        if let current = row.fields["lastSeenAt"]?.int64Value, current >= at { return }
        doc.write(kind: "chats", id: chatId, op: .update, set: ["lastSeenAt": .int(at)])
        afterLocalWrite()
    }

    func rename(chatId: String, title: String) {
        updateChat(chatId, set: ["title": .string(title)])
    }

    /// Chat config is an LWW field on the chat row; the host reads it when
    /// dispatching the next run.
    func setChatConfig(chatId: String, config chatConfig: ChatConfig) {
        guard let value = JSONValue(encodable: chatConfig) else { return }
        updateChat(chatId, set: ["config": value])
    }

    /// Delete a session, desktop `delete_chat` order. The host is told
    /// FIRST (`Mutate deleteChat` over its relay): only the engine that
    /// receives the op purges its doc snapshot and interrupts + deletes the
    /// chat's subagent children — it finds them by looking up rows our
    /// tombstones would already have removed. Then the tombstones land here
    /// regardless (an offline host still loses the rows), children included,
    /// and a quick chat's scratch folder is removed. The chat's own run is
    /// not interrupted, as on the desktop.
    ///
    /// Returns a notice for a partial failure, nil when all went through.
    func deleteChat(_ chat: Chat, hostOnline: Bool) async -> String? {
        deleting.insert(chat.id)
        project()
        defer { deleting.remove(chat.id) }
        let host = relay(for: chat.deviceId)
        var hostReached = false
        if hostOnline {
            let reply: IgnoredReply? = try? await host.call(
                method: "Mutate", params: ["op": "deleteChat", "chatId": chat.id], timeoutSeconds: 8)
            hostReached = reply != nil
        }
        var keys: [(kind: String, id: String)] = [("chats", chat.id), ("sessions", chat.id)]
        for child in chats where child.child?.parentChatId == chat.id {
            keys.append(("chats", child.id))
            keys.append(("sessions", child.id))
        }
        doc.deleteRows(keys)
        afterLocalWrite()
        guard chat.isScratch, let cwd = chat.cwd else { return nil }
        guard hostReached else {
            return "The session was deleted, but its quick-chat folder stays on the device until it's back online."
        }
        do {
            let _: IgnoredReply = try await host.call(
                method: "DeleteScratchDir", params: ["chatId": chat.id, "path": cwd], timeoutSeconds: 15)
            return nil
        } catch {
            return "The session was deleted, but its quick-chat folder couldn't be removed: \(error.localizedDescription)"
        }
    }

    /// Quick chat step 1 (composer.rs first send): the host makes this
    /// chat's `…/cypher-scratch/<chatId>` folder and returns its path.
    func createScratchDir(deviceId: String, chatId: String) async throws -> String {
        struct Reply: Decodable { var path: String }
        let reply: Reply = try await relay(for: deviceId)
            .call(method: "CreateScratchDir", params: ["chatId": chatId], timeoutSeconds: 20)
        return reply.path
    }

    /// Quick chat step 2: `createChat`'s full-row upsert without a project,
    /// under the id the scratch folder was made for.
    func createQuickChat(chatId: String, deviceId: String, cwd: String, config chatConfig: ChatConfig) {
        var set: [String: JSONValue] = [
            "id": .string(chatId),
            "deviceId": .string(deviceId),
            "archived": .bool(false),
            "cwd": .string(cwd),
            "createdAt": .int(nowMs()),
            "roomGen": .int(2),
        ]
        if let cfg = JSONValue(encodable: chatConfig) {
            set["config"] = cfg
        }
        doc.write(kind: "chats", id: chatId, op: .upsert, set: set)
        afterLocalWrite()
    }

    /// Hard-delete a space and cascade to its chats: ONE batch tombstones the
    /// space row and every chat/session row whose spaceId matches — the
    /// server applies the batch atomically.
    func deleteSpace(spaceId: String) {
        var keys: [(kind: String, id: String)] = []
        for chat in chats where chat.spaceId == spaceId {
            keys.append(("chats", chat.id))
            keys.append(("sessions", chat.id))
        }
        keys.append(("spaces", spaceId))
        doc.deleteRows(keys)
        afterLocalWrite()
    }

    /// Unpair a device: tombstone the device row only. Spaces and chats stay
    /// so that machine can keep its work after it drops to local-only. The
    /// local (this) device is the caller's responsibility to refuse.
    func deleteDevice(deviceId: String) {
        doc.deleteRows([("devices", deviceId)])
        afterLocalWrite()
    }

    /// Field sets are `update` ops — they NEVER create or revive rows (the
    /// old "never invent rows" discipline), so check the overlay row first.
    private func updateChat(_ chatId: String, set: [String: JSONValue]) {
        guard doc.rowExists(kind: "chats", id: chatId) else { return }
        doc.write(kind: "chats", id: chatId, op: .update, set: set)
        afterLocalWrite()
    }
}

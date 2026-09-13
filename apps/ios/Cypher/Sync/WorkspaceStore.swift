// Normal v3 sidebar. SQLite owns canonical rows, clocks and offline edits;
// The disposable sidebar cache contains only typed current rows.
// Account-scoped WorkspaceHub supplies metadata and replaceable peer leases.
// The phone is a viewer, not an execution host.

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

    /// Bound freshness by the server lease and at most 45 seconds of receipt.
    static let presenceTtlMs: Int64 = 45_000

    @ObservationIgnored private var cached: [String: [String: Workspace3Row]] = [:]
    @ObservationIgnored private var context: Workspace3Context?
    @ObservationIgnored private var journal: Workspace3Journal?
    @ObservationIgnored private var observer: UUID?
    @ObservationIgnored private var stopped = false
    private struct Peer {
        let connection: String
        let expires: Int64
        let sessions: [SessionRow]
    }
    @ObservationIgnored private var peers: [String: Peer] = [:]
    private(set) var error: String?
    private(set) var connectionError: String?
    private let config: AppConfig

    init(config: AppConfig, initialJournal: Workspace3Journal? = nil) {
        self.config = config
        self.journal = initialJournal
        do { try reload() } catch { self.error = error.localizedDescription }
    }

    func start() {
        guard context == nil, !stopped else { return }
        do {
            let context = try config.workspaceContext()
            self.context = context; journal = context.journal
            try reload()
            observer = context.observe { [weak self] event in self?.consume(event) }
            consume(nil)
        } catch { self.error = error.localizedDescription }
    }

    /// Backgrounding hook: persist immediately.
    func flushToDisk() {
        // Every successful mutation is already durably committed.
    }

    /// Foreground hook: verify business liveness without HTTP polling.
    func kickRoom() {
        context?.client.probe()
    }

    func stop() {
        stopped = true
        if let observer { context?.removeObserver(observer) }
        context?.retire(); context = nil
        peers.removeAll(); sessions.removeAll(); presence.removeAll()
        connected = false
    }

    // MARK: Server events (delivered in frame order — rows before ack)

    func consume(_ event: Workspace3Event?) {
        guard !stopped, config.isActive else { return }
        do {
            if case .metadata(let keys) = event { try refresh(keys) }
            if case .frame(_, let frame) = event {
                if frame["type"] == .string("presence"), frame["role"] == .string("host"),
                   let actor = frame["actor"]?.stringValue, let connection = frame["connection"]?.stringValue,
                   let expires = frame["expiresAt"]?.int64Value, expires > nowMs() {
                    var rows: [SessionRow] = []
                    if case .array(let entries) = frame["state"]?.objectValue?["sessions"] {
                        for entry in entries {
                            guard let f = entry.objectValue, f["deviceId"] == .string(actor),
                                  let chat = f["chatId"]?.stringValue, let raw = f["status"]?.stringValue,
                                  let status = SessionStatus(rawValue: raw) else { continue }
                            rows.append(SessionRow(chatId: chat, deviceId: actor, status: status,
                                startedAt: Self.timestamp(f["startedAt"]), updatedAt: nowMs(),
                                subagents: SubagentProjection.snapshot(f["subagents"])))
                        }
                    }
                    peers[actor] = Peer(connection: connection, expires: min(expires, nowMs() + Self.presenceTtlMs), sessions: rows)
                } else if frame["type"] == .string("peerClosed"), let actor = frame["actor"]?.stringValue,
                          peers[actor]?.connection == frame["connection"]?.stringValue { peers.removeValue(forKey: actor) }
            }
            connected = context?.client.status.connected ?? false
            connectionError = context?.client.status.error
            projectPresence()
        } catch { self.error = error.localizedDescription }
    }
    private static func timestamp(_ value: JSONValue?) -> Int64? {
        if let ms = value?.int64Value { return ms }
        guard let text = value?.stringValue else { return nil }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: text)
        formatter.formatOptions = [.withInternetDateTime]
        return (date ?? formatter.date(from: text)).map { Int64($0.timeIntervalSince1970 * 1000) }
    }
    private func reload() throws {
        cached.removeAll()
        if let journal {
            for kind in ["devices", "spaces", "chats"] {
                var after = ""
                while true {
                    let page = try journal.window(kind: kind, after: after)
                    guard let next = page.next else { break }
                    for row in page.rows { cached[row.kind, default: [:]][row.id] = row }
                    after = next
                }
            }
        }
        project(); projectPresence()
    }
    private func refresh(_ keys: [(String, String)]) throws {
        guard let journal else { throw RelayError.notConnected }
        for (kind, id) in keys {
            if let row = try journal.row(kind: kind, id: id) {
                cached[kind, default: [:]][id] = row
            }
        }
        project(); projectPresence()
    }
    @discardableResult private func write(_ operations: [Workspace3Op]) -> Bool {
        do {
            guard !stopped, config.isActive, let journal else { throw RelayError.notConnected }
            try journal.mutate(operations, now: nowMs())
            try refresh(operations.map { ($0.kind, $0.id) })
            error = nil; context?.client.nudge(); return true
        } catch { self.error = error.localizedDescription; return false }
    }
    private func operation(_ kind: String, _ id: String, _ op: Workspace3OpType, _ set: [String: JSONValue]? = nil) -> Workspace3Op {
        Workspace3Op(kind: kind, id: id, op: op, set: set, hlc: "")
    }
    private func rows(_ kind: String) -> [Workspace3Row] {
        Array(cached[kind, default: [:]].values).filter { !$0.deleted }
    }
    private func projectPresence() {
        let now = nowMs()
        peers = peers.filter { $0.value.expires > now }
        presence = peers.mapValues { _ in now }
        sessions = [:]
        for (actor, peer) in peers {
            for var row in peer.sessions where chats.contains(where: { $0.id == row.chatId && $0.deviceId == actor }) {
                row.updatedAt = now
                sessions[row.chatId] = row
            }
        }
    }

    // MARK: Presence

    func deviceOnline(_ deviceId: String) -> Bool {
        peers[deviceId].map { $0.expires > nowMs() } ?? false
    }

    // MARK: Projection (rows → typed entities)

    private func project() {
        devices = rows("devices").map { row in
            let f = row.fields
            let id = f["id"]?.stringValue ?? row.id
            return DeviceRow(id: id,
                             name: f["name"]?.stringValue ?? id,
                             platform: f["platform"]?.stringValue ?? "",
                             lastSeenAt: f["lastSeenAt"]?.int64Value,
                             createdAt: f["createdAt"]?.int64Value)
        }.sorted { $0.name < $1.name }

        spaces = rows("spaces").compactMap { row in
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

        chats = rows("chats").compactMap { row in
            let f = row.fields
            guard let deviceId = f["deviceId"]?.stringValue else { return nil }
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

    @ObservationIgnored private var relayClients: [String: WorkspaceRemote] = [:]

    private func relay(for deviceId: String) -> WorkspaceRemote {
        if let existing = relayClients[deviceId] { return existing }
        let client = WorkspaceRemote(deviceId: deviceId, config: config)
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
                    branch: String? = nil, cwd: String? = nil) -> String? {
        let chatId = UUID().uuidString.lowercased()
        var set: [String: JSONValue] = [
            "id": .string(chatId),
            "deviceId": .string(space.deviceId),
            "archived": .bool(false),
            "cwd": .string(cwd ?? space.path),
            "spaceId": .string(space.id),
            "createdAt": .int(nowMs()),
        ]
        if let branch {
            set["branch"] = .string(branch)
        }
        if let cfg = JSONValue(encodable: chatConfig) {
            set["config"] = cfg
        }
        return write([operation("chats", chatId, .upsert, set)]) ? chatId : nil
    }

    /// One durable local metadata operation. No uncertain RPC and fallback.
    @discardableResult
    func createSpace(deviceId: String, path: String, gitDetected: Bool = false) async -> String? {
        // Dedup on (device, path) like the desktop palette.
        if let existing = spaces.first(where: { $0.deviceId == deviceId && $0.path == path }) {
            return existing.id
        }
        let spaceId = UUID().uuidString.lowercased()
        return write([operation("spaces", spaceId, .upsert, [
            "id": .string(spaceId), "deviceId": .string(deviceId), "path": .string(path),
            "gitDetected": .bool(gitDetected), "createdAt": .int(nowMs())
        ])]) ? spaceId : nil
    }

    func setArchived(chatId: String, archived: Bool) {
        updateChat(chatId, set: ["archived": .bool(archived)])
    }

    /// Synced seen marker (LWW) with a monotonic guard: no write when the
    /// stored stamp is already current.
    func markSeen(chatId: String) {
        guard let row = cached["chats"]?[chatId], !row.deleted else { return }
        let at = nowMs()
        if let current = row.fields["lastSeenAt"]?.int64Value, current >= at { return }
        write([operation("chats", chatId, .update, ["lastSeenAt": .int(at)])])
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

    /// Tombstone a chat and its descendant metadata atomically on this device.
    func deleteChat(chatId: String) {
        let ids = descendants([chatId])
        write(ids.sorted().map { operation("chats", $0, .delete) })
    }

    /// Cascade in one local SQLite transaction, then bounded LWW sync batches.
    func deleteSpace(spaceId: String) {
        let ids = descendants(Set(chats.filter { $0.spaceId == spaceId }.map(\.id)))
        write(ids.sorted().map { operation("chats", $0, .delete) } + [operation("spaces", spaceId, .delete)])
    }

    /// Unpair a device: tombstone the device row only. Spaces and chats stay
    /// so that machine can keep its work after it drops to local-only. The
    /// local (this) device is the caller's responsibility to refuse.
    func deleteDevice(deviceId: String) {
        guard deviceId != config.deviceId else { error = "cannot_unpair_self"; return }
        write([operation("devices", deviceId, .delete)])
    }

    /// Field sets are `update` ops — they NEVER create or revive rows (the
    /// old "never invent rows" discipline), so check the overlay row first.
    private func updateChat(_ chatId: String, set: [String: JSONValue]) {
        guard cached["chats"]?[chatId]?.deleted == false else { return }
        write([operation("chats", chatId, .update, set)])
    }
    private func descendants(_ roots: Set<String>) -> Set<String> {
        var result = roots
        while true {
            let next = chats.filter { $0.child.map { result.contains($0.parentChatId) } == true }.map(\.id)
            let count = result.count; result.formUnion(next)
            if result.count == count { return result }
        }
    }
}

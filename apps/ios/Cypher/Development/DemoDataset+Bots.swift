// The demo's workspace actions: the same guards as the live store, applied
// to the in-memory records so every screen reacts at once, and the host's
// side of each control field played back after a short delay (an approved
// plan starts, a stopped one stops, a revoked connection goes) so a demo
// walk-through reads like a live one.

import Foundation

typealias MemoryViewContinuation = AsyncThrowingStream<MemoryView, Error>.Continuation

extension DemoDataset {
    // MARK: Owner fields

    func resolvePlan(workId: String, approve: Bool) async throws {
        let ix = try index(of: workId, in: \.work, what: "plan")
        guard botRecords.work[ix].awaitingApproval else { throw BotActionError.alreadyDecided }
        let now = nowMs()
        botRecords.work[ix].decision = PlanDecision(approve: approve, at: now)
        let work = botRecords.work[ix]
        logActivity(
            work.botId, approve ? .planApproved : .planCancelled,
            approve ? "Approved “\(work.title)”" : "Cancelled “\(work.title)”", workId: workId)
        hostLater { demo in
            guard let ix = demo.botRecords.work.firstIndex(where: { $0.id == workId }) else { return }
            if approve {
                demo.botRecords.work[ix].state = .running
                demo.botRecords.work[ix].startedAt = nowMs()
                if !demo.botRecords.work[ix].steps.isEmpty {
                    demo.botRecords.work[ix].steps[0].state = .running
                    demo.botRecords.work[ix].steps[0].startedAt = nowMs()
                }
            } else {
                demo.botRecords.work[ix].state = .cancelled
                demo.botRecords.work[ix].endedAt = nowMs()
            }
        }
    }

    func stopWork(workId: String) async throws {
        let ix = try index(of: workId, in: \.work, what: "plan")
        botRecords.work[ix].stopRequestedAt = nowMs()
        hostLater { demo in
            guard let ix = demo.botRecords.work.firstIndex(where: { $0.id == workId }),
                demo.botRecords.work[ix].state == .running
            else { return }
            demo.botRecords.work[ix].state = .stopped
            demo.botRecords.work[ix].endedAt = nowMs()
            for step in demo.botRecords.work[ix].steps.indices
            where [StepState.running, .asking].contains(demo.botRecords.work[ix].steps[step].state) {
                demo.botRecords.work[ix].steps[step].state = .failed
                demo.botRecords.work[ix].steps[step].endedAt = nowMs()
            }
            let work = demo.botRecords.work[ix]
            demo.logActivity(work.botId, .workStopped, "Stopped “\(work.title)”", workId: workId)
        }
    }

    func resolveApproval(approvalId: String, approve: Bool) async throws {
        let ix = try index(of: approvalId, in: \.approvals, what: "approval")
        botRecords.approvals[ix].decision = PlanDecision(approve: approve, at: nowMs())
        hostLater { demo in
            guard let ix = demo.botRecords.approvals.firstIndex(where: { $0.id == approvalId }) else { return }
            demo.botRecords.approvals[ix].state = approve ? .approved : .declined
        }
    }

    func runRoutine(routineId: String) async throws {
        let ix = try index(of: routineId, in: \.routines, what: "routine")
        let now = nowMs()
        botRecords.routines[ix].runRequestedAt = now
        hostLater { demo in
            guard let ix = demo.botRecords.routines.firstIndex(where: { $0.id == routineId }) else { return }
            demo.botRecords.routines[ix].lastRun = RunRecord(
                at: nowMs(), outcome: .running, chatId: demo.botRecords.routines[ix].threadChatId, manual: true)
        }
    }

    func setRoutineEnabled(routineId: String, enabled: Bool) async throws {
        let ix = try index(of: routineId, in: \.routines, what: "routine")
        botRecords.routines[ix].enabled = enabled
        botRecords.routines[ix].updatedAt = nowMs()
    }

    func updateConnection(connectionId: String, replyMode: ReplyMode, scope: BotScope?) async throws {
        if let error = scope?.validationError { throw BotActionError(error) }
        let ix = try index(of: connectionId, in: \.connections, what: "connection")
        botRecords.connections[ix].replyMode = replyMode
        botRecords.connections[ix].scope = scope
    }

    func revokeConnection(connectionId: String) async throws {
        let ix = try index(of: connectionId, in: \.connections, what: "connection")
        botRecords.connections[ix].revokeRequestedAt = nowMs()
        hostLater { demo in
            guard let ix = demo.botRecords.connections.firstIndex(where: { $0.id == connectionId }) else { return }
            demo.botRecords.connections[ix].revokedAt = nowMs()
            let connection = demo.botRecords.connections[ix]
            demo.logActivity(connection.botId, .connectionRevoked, "Disconnected from \(connection.peer.name)")
        }
    }

    func revokeInvite(inviteId: String) async throws {
        let ix = try index(of: inviteId, in: \.invites, what: "invite")
        botRecords.invites[ix].revokeRequestedAt = nowMs()
        hostLater { demo in
            guard let ix = demo.botRecords.invites.firstIndex(where: { $0.id == inviteId }) else { return }
            demo.botRecords.invites[ix].revokedAt = nowMs()
        }
    }

    func reviewExchange(exchangeId: String, send: Bool, text: String?) async throws {
        let ix = try index(of: exchangeId, in: \.exchanges, what: "question")
        let text = text.map { clip($0, max: BotCaps.answer) }.flatMap { $0.isEmpty ? nil : $0 }
        botRecords.exchanges[ix].review = ReviewDecision(send: send, text: text, at: nowMs())
        hostLater { demo in
            guard let ix = demo.botRecords.exchanges.firstIndex(where: { $0.id == exchangeId }) else { return }
            demo.botRecords.exchanges[ix].state = send ? .answered : .declined
            demo.botRecords.exchanges[ix].answeredAt = nowMs()
            if send, let text { demo.botRecords.exchanges[ix].answer = text }
        }
    }

    func setBotPaused(botId: String, paused: Bool) async throws {
        guard let ix = bots.firstIndex(where: { $0.id == botId }) else { throw BotActionError.missing("workspace") }
        bots[ix].paused = paused
    }

    func deleteShortcut(shortcutId: String) async throws {
        botRecords.shortcuts.removeAll { $0.id == shortcutId }
    }

    func orderShortcuts(botId: String, ids: [String]) async throws {
        for (order, id) in ids.enumerated() {
            guard let ix = botRecords.shortcuts.firstIndex(where: { $0.id == id && $0.botId == botId }) else {
                continue
            }
            botRecords.shortcuts[ix].order = Int64(order)
        }
    }

    // MARK: Engine calls

    func createBot(_ request: CreateBotRequest, on deviceId: String) async throws -> Bot {
        if let error = Bot.nameError(request.name) { throw BotActionError(error) }
        guard deviceOnline(deviceId) else { throw BotActionError("\(botDeviceName(deviceId)) is offline.") }
        try? await Task.sleep(nanoseconds: 400_000_000)
        let id = "bot-\(UUID().uuidString.lowercased().prefix(8))"
        let now = nowMs()
        let bot = Bot(
            id: id, name: request.name.trimmingCharacters(in: .whitespacesAndNewlines),
            icon: request.icon, color: request.color, hostDeviceId: deviceId, chatId: "chat-\(id)",
            instructions: request.instructions, scope: request.scope,
            capabilities: request.capabilities ?? .newBot, paused: false, createdAt: now)
        bots.append(bot)
        chats.append(
            Chat(
                id: bot.chatId, deviceId: deviceId, title: bot.name, archived: false,
                cwd: "/Users/dev/.cypher/bots/\(id)", branch: nil, checkoutId: nil,
                config: ChatConfig(harness: "pi", model: request.model ?? "demo/pi", reasoning: request.reasoning),
                lastMessagePreview: nil, lastMessageAt: nil, createdAt: now, spaceId: nil, lastSeenAt: now,
                bot: BotChatMarker(botId: id)))
        return bot
    }

    func updateBot(
        botId: String, name: String, icon: String?, color: String?, instructions: String?,
        scope: BotScope, capabilities: BotCapabilities?
    ) async throws {
        guard let ix = bots.firstIndex(where: { $0.id == botId }) else { throw BotActionError.missing("workspace") }
        if let error = Bot.nameError(name) ?? scope.validationError { throw BotActionError(error) }
        try? await Task.sleep(nanoseconds: 200_000_000)
        bots[ix].name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        bots[ix].icon = icon.flatMap { $0.isEmpty ? nil : $0 }
        bots[ix].color = color.flatMap { $0.isEmpty ? nil : $0 }
        bots[ix].instructions = instructions.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .flatMap { $0.isEmpty ? nil : $0 }
        bots[ix].scope = scope
        if let capabilities { bots[ix].capabilities = capabilities }
    }

    func deleteBot(botId: String) async throws {
        guard bots.contains(where: { $0.id == botId }) else { return }
        try? await Task.sleep(nanoseconds: 200_000_000)
        bots.removeAll { $0.id == botId }
        chats.removeAll { $0.bot?.botId == botId }
        botRecords.work.removeAll { $0.botId == botId }
        botRecords.routines.removeAll { $0.botId == botId }
        botRecords.activity.removeAll { $0.botId == botId }
        botRecords.approvals.removeAll { $0.botId == botId }
        botRecords.exchanges.removeAll { $0.botId == botId }
        botRecords.shortcuts.removeAll { $0.botId == botId }
        botRecords.memoryStores.removeAll { $0.scope == .workspace(botId: botId) }
    }

    func saveRoutine(_ routine: BotRoutine) async throws {
        guard let bot = bot(id: routine.botId) else { throw BotActionError.missing("workspace") }
        if let error = routine.validationError { throw BotActionError(error) }
        // The engine's checks for a project routine (rpc/bots.rs).
        if let spaceId = routine.target.spaceId {
            if !bot.capabilities.coordinate {
                throw BotActionError("Running a routine in a project needs the workspace's Coordinate permission.")
            }
            if !bot.scope.seesSpace(spaceId) { throw BotActionError("The workspace has no access to that project.") }
        }
        try? await Task.sleep(nanoseconds: 200_000_000)
        var saved = routine
        saved.name = clip(saved.name, max: BotCaps.title)
        saved.prompt = clip(saved.prompt, max: BotCaps.prompt)
        saved.updatedAt = nowMs()
        if let ix = botRecords.routines.firstIndex(where: { $0.id == routine.id }) {
            // The host's fields stay the host's.
            let current = botRecords.routines[ix]
            saved.threadChatId = current.threadChatId
            saved.manualChatId = current.manualChatId
            saved.worktreePath = current.worktreePath
            saved.lastRun = current.lastRun
            saved.nextRunAt = current.nextRunAt
            saved.runRequestedAt = current.runRequestedAt
            botRecords.routines[ix] = saved
        } else {
            saved.threadChatId = nil
            saved.manualChatId = nil
            saved.lastRun = nil
            saved.nextRunAt = nil
            botRecords.routines.append(saved)
        }
    }

    func deleteRoutine(routineId: String) async throws {
        guard let routine = botRecords.routines.first(where: { $0.id == routineId }) else { return }
        botRecords.routines.removeAll { $0.id == routineId }
        let sessions = [routine.threadChatId, routine.manualChatId].compactMap { $0 }
        chats.removeAll { sessions.contains($0.id) }
    }

    func deleteWork(workId: String) async throws {
        guard let work = botRecords.work.first(where: { $0.id == workId }) else { return }
        guard !work.isLive else { throw BotActionError.stillLive }
        botRecords.work.removeAll { $0.id == workId }
        if let thread = work.threadChatId { chats.removeAll { $0.id == thread } }
    }

    func createInvite(botId: String) async throws -> InviteCreated {
        guard let bot = bot(id: botId) else { throw BotActionError.missing("workspace") }
        guard botHostOnline(bot) else { throw BotActionError.hostOffline(bot) }
        try? await Task.sleep(nanoseconds: 500_000_000)
        let now = nowMs()
        let id = "invite-\(UUID().uuidString.lowercased().prefix(8))"
        let invite = BotInvite(
            id: id, botId: botId, createdAt: now, expiresAt: now + 7 * 86_400_000, uses: 0, maxUses: 1)
        botRecords.invites.insert(invite, at: 0)
        return InviteCreated(
            inviteId: id,
            code: "room-\(botId).\(UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: ""))",
            createdAt: now, expiresAt: invite.expiresAt, maxUses: 1)
    }

    func redeemInvite(botId: String, code: String) async throws -> BotConnection {
        guard let bot = bot(id: botId) else { throw BotActionError.missing("workspace") }
        guard botHostOnline(bot) else { throw BotActionError.hostOffline(bot) }
        let code = code.trimmingCharacters(in: .whitespacesAndNewlines)
        let parts = code.split(separator: ".")
        guard parts.count == 2, parts.allSatisfy({ !$0.isEmpty }) else {
            throw BotActionError("That isn't an invite code.")
        }
        try? await Task.sleep(nanoseconds: 600_000_000)
        let now = nowMs()
        let connection = BotConnection(
            id: "conn-\(UUID().uuidString.lowercased().prefix(8))", botId: botId,
            peer: PeerBot(roomId: String(parts[0]), name: "Meridian", ownerName: "Alex Kim"),
            createdAt: now, replyMode: .review)
        botRecords.connections.append(connection)
        logActivity(botId, .connectionAdded, "Connected to Meridian (Alex Kim)")
        return connection
    }

    func watchMemory(scope: MemoryScope) -> AsyncThrowingStream<MemoryView, Error> {
        let (stream, continuation) = AsyncThrowingStream<MemoryView, Error>.makeStream()
        // A store nothing has written yet reads empty, as the engine's does.
        let view = memoryViews[scope] ?? emptyMemoryView(scope)
        let id = UUID()
        memoryWatchers[id] = (scope, continuation)
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor in self?.memoryWatchers[id] = nil }
        }
        continuation.yield(view)
        return stream
    }

    func editMemory(scope: MemoryScope, edit: MemoryEdit) async throws -> String {
        var view = memoryViews[scope] ?? emptyMemoryView(scope)
        try? await Task.sleep(nanoseconds: 150_000_000)
        let now = nowMs()
        let result: String
        switch edit {
        case .write(let path, let text):
            guard validMemoryPath(path) else { throw BotActionError("Use a path like /notes.md.") }
            if let ix = view.files.firstIndex(where: { $0.path == path }) {
                let before = view.files[ix].text
                view.files[ix].text = text
                view.files[ix].author = .owner
                view.files[ix].updatedAt = now
                view.record(path: path, action: .edit, summary: "Edited the file", before: before, at: now)
                result = "Saved \(path)."
            } else {
                view.files.append(MemoryFile(path: path, text: text, author: .owner, createdAt: now, updatedAt: now))
                view.record(path: path, action: .create, summary: "Created the file", before: nil, at: now)
                result = "Created \(path)."
            }
        case .delete(let path):
            guard let ix = view.files.firstIndex(where: { $0.path == path }) else {
                throw BotActionError("No file at \(path).")
            }
            let before = view.files.remove(at: ix).text
            view.record(path: path, action: .delete, summary: "Removed the file", before: before, at: now)
            result = "Deleted \(path)."
        case .rename(let from, let to):
            guard validMemoryPath(to) else { throw BotActionError("Use a path like /notes.md.") }
            guard let ix = view.files.firstIndex(where: { $0.path == from }) else {
                throw BotActionError("No file at \(from).")
            }
            view.files[ix].path = to
            view.record(path: to, action: .rename(from: from), summary: "Renamed from \(from)", before: nil, at: now)
            result = "Renamed to \(to)."
        case .accept(let id), .discard(let id):
            guard let ix = view.pending.firstIndex(where: { $0.id == id }) else {
                throw BotActionError("This write was already decided.")
            }
            let pending = view.pending.remove(at: ix)
            var saved = false
            if case .accept = edit {
                saved = true
                if let preview = pending.preview {
                    if let file = view.files.firstIndex(where: { $0.path == pending.path }) {
                        view.files[file].text = preview
                        view.files[file].author = pending.author
                        view.files[file].updatedAt = now
                    } else {
                        view.files.append(
                            MemoryFile(
                                path: pending.path, text: preview, author: pending.author, createdAt: now,
                                updatedAt: now))
                    }
                }
                view.record(path: pending.path, action: .edit, summary: "Saved a held write", before: nil, at: now)
            }
            view.store.held.removeAll { $0.id == id }
            view.store.decided.append(HeldDecision(id: id, toolCallId: pending.toolCallId, saved: saved, at: now))
            result = saved ? "Saved." : "Discarded."
        case .undo(let changeId):
            guard let ix = view.changes.firstIndex(where: { $0.id == changeId }) else {
                throw BotActionError("No such change.")
            }
            view.changes[ix].undone = true
            if let before = view.changes[ix].before,
                let file = view.files.firstIndex(where: { $0.path == view.changes[ix].path })
            {
                view.files[file].text = before
            }
            result = "Undone."
        case .undoPass(let passId):
            if view.store.lastPass?.id == passId { view.store.lastPass?.outcome = .undone }
            result = "Undone."
        case .setConsolidation(let enabled, let hour):
            view.store.consolidation = Consolidation(enabled: enabled, hour: min(max(hour, 0), 23))
            result =
                enabled ? "Consolidation runs nightly at \(String(format: "%02d:00", hour))." : "Consolidation is off."
        case .runPass:
            view.store.passRequestedAt = now
            view.store.lastPass = MemoryPass(id: "pass-\(now)", at: now, outcome: .running, manual: true)
            result = "A pass is starting."
            hostLater { demo in
                guard var view = demo.memoryViews[scope], view.store.lastPass?.id == "pass-\(now)" else { return }
                view.store.lastPass?.outcome = .skipped
                view.store.lastPass?.endedAt = nowMs()
                demo.setMemoryView(view)
            }
        }
        view.files.sort { a, b in
            if (a.path == MemoryLimits.index) != (b.path == MemoryLimits.index) { return a.path == MemoryLimits.index }
            return a.path < b.path
        }
        view.store.files = view.files.count
        view.store.bytes = Int64(view.files.reduce(0) { $0 + $1.text.utf8.count })
        view.store.pending = view.pending.count
        view.store.preview = clip(view.files.first { $0.path == MemoryLimits.index }?.text ?? "", max: 1_200)
        view.store.updatedAt = now
        setMemoryView(view)
        return result
    }

    /// Store a view and its row, and send it to every watcher of its scope.
    func setMemoryView(_ view: MemoryView) {
        memoryViews[view.store.scope] = view
        if let ix = botRecords.memoryStores.firstIndex(where: { $0.scope == view.store.scope }) {
            botRecords.memoryStores[ix] = view.store
        } else {
            botRecords.memoryStores.append(view.store)
        }
        for watcher in memoryWatchers.values where watcher.scope == view.store.scope {
            watcher.continuation.yield(view)
        }
    }

    // MARK: Helpers

    /// A store nothing has written yet.
    private func emptyMemoryView(_ scope: MemoryScope) -> MemoryView {
        let host: String =
            switch scope {
            case .workspace(let botId): bot(id: botId)?.hostDeviceId ?? "dev-mac"
            case .project(let spaceId): spaces.first { $0.id == spaceId }?.deviceId ?? "dev-mac"
            default: "dev-mac"
            }
        return MemoryView(
            store: MemoryStore(id: "memory-\(scope)", scope: scope, hostDeviceId: host, createdAt: nowMs()),
            files: [], pending: [], changes: [])
    }

    private func index<T: Identifiable>(
        of id: T.ID, in list: KeyPath<BotRecords, [T]>, what: String
    ) throws -> Int {
        guard let ix = botRecords[keyPath: list].firstIndex(where: { $0.id == id }) else {
            throw BotActionError.missing(what)
        }
        return ix
    }

    /// The host acting on a control field, a moment after it was written.
    private func hostLater(_ act: @escaping @MainActor (DemoDataset) -> Void) {
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 1_200_000_000)
            guard let self else { return }
            act(self)
        }
    }

    func logActivity(
        _ botId: String, _ kind: ActivityKind, _ text: String, chatId: String? = nil, workId: String? = nil
    ) {
        let now = nowMs()
        botRecords.activity.insert(
            BotActivity(
                id: "act-\(now)-\(botRecords.activity.count)", botId: botId, at: now, kind: kind, text: text,
                chatId: chatId, workId: workId),
            at: 0)
    }
}

extension MemoryView {
    /// A change-log entry, newest first.
    fileprivate mutating func record(path: String, action: MemoryAction, summary: String, before: String?, at: Int64) {
        changes.insert(
            MemoryChange(
                id: "change-\(at)-\(changes.count)", at: at, author: .owner, path: path, action: action,
                summary: summary, before: before, undo: "v\(at)"),
            at: 0)
    }
}

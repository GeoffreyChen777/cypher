// WorkspaceStore's workspace actions. Owner fields are local registry writes
// of exactly the fields the engine's `mutate_bot_record` / `SetBotPaused`
// write (crates/engine/src/rpc/bots.rs, mutate.rs), with the same guards:
// instant, queued offline, and acted on by the host, which watches the rows.
// Everything the engine must do itself (create, settings, routines it
// validates, invites, memory) is an RPC over the relay.

import Foundation

extension WorkspaceStore {
    // MARK: Owner fields

    func resolvePlan(workId: String, approve: Bool) async throws {
        guard let work = botRecords.work.first(where: { $0.id == workId }) else { throw BotActionError.missing("plan") }
        guard work.awaitingApproval else { throw BotActionError.alreadyDecided }
        try updateRecord("botWork", workId, what: "plan", ["decision": decision(approve)])
    }

    func stopWork(workId: String) async throws {
        try updateRecord("botWork", workId, what: "plan", ["stopRequestedAt": .int(nowMs())])
    }

    func resolveApproval(approvalId: String, approve: Bool) async throws {
        try updateRecord("botApprovals", approvalId, what: "approval", ["decision": decision(approve)])
    }

    func runRoutine(routineId: String) async throws {
        try updateRecord("botRoutines", routineId, what: "routine", ["runRequestedAt": .int(nowMs())])
    }

    func setRoutineEnabled(routineId: String, enabled: Bool) async throws {
        try updateRecord(
            "botRoutines", routineId, what: "routine", ["enabled": .bool(enabled), "updatedAt": .int(nowMs())])
    }

    func updateConnection(connectionId: String, replyMode: ReplyMode, scope: BotScope?) async throws {
        if let error = scope?.validationError { throw BotActionError(error) }
        // A null field deletes it: no narrowed scope, the workspace's own.
        let scopeValue = scope.flatMap { JSONValue(encodable: $0) } ?? .null
        try updateRecord(
            "botConnections", connectionId, what: "connection",
            ["replyMode": .string(replyMode.rawValue), "scope": scopeValue])
    }

    func revokeConnection(connectionId: String) async throws {
        try updateRecord("botConnections", connectionId, what: "connection", ["revokeRequestedAt": .int(nowMs())])
    }

    func revokeInvite(inviteId: String) async throws {
        try updateRecord("botInvites", inviteId, what: "invite", ["revokeRequestedAt": .int(nowMs())])
    }

    func reviewExchange(exchangeId: String, send: Bool, text: String?) async throws {
        var review: [String: JSONValue] = ["send": .bool(send), "at": .int(nowMs())]
        if let text = text.map({ clip($0, max: BotCaps.answer) }), !text.isEmpty {
            review["text"] = .string(text)
        }
        try updateRecord("botExchanges", exchangeId, what: "question", ["review": .object(review)])
    }

    func setBotPaused(botId: String, paused: Bool) async throws {
        try updateRecord("bots", botId, what: "workspace", ["paused": .bool(paused)])
    }

    func deleteShortcut(shortcutId: String) async throws {
        doc.deleteRows([("botShortcuts", shortcutId)])
        afterLocalWrite()
    }

    func orderShortcuts(botId: String, ids: [String]) async throws {
        var writes: [(kind: String, id: String, op: RegistryOpType, set: [String: JSONValue])] = []
        for (order, id) in ids.enumerated() {
            guard let shortcut = botRecords.shortcuts.first(where: { $0.id == id }),
                shortcut.botId == botId, shortcut.order != Int64(order)
            else { continue }
            writes.append(("botShortcuts", id, .update, ["order": .int(Int64(order))]))
        }
        guard !writes.isEmpty else { return }
        doc.write(writes)
        afterLocalWrite()
    }

    /// Field sets are `update` ops: they never create a record, so a record
    /// deleted elsewhere says so instead.
    private func updateRecord(_ kind: String, _ id: String, what: String, _ set: [String: JSONValue]) throws {
        guard doc.rowExists(kind: kind, id: id) else { throw BotActionError.missing(what) }
        // The engine's backstop (`check_record`): the Edge refuses an op
        // over 16 KiB and a refused batch wedges sync, so a write this big
        // never leaves the phone. `clip` counts characters; this counts what
        // the engine counts, UTF-16 units of the encoded fields.
        if let data = try? JSONEncoder().encode(set),
            (String(data: data, encoding: .utf8)?.utf16.count ?? 0) > BotLimits.recordUnits
        {
            throw BotActionError("That's too long to send. Shorten it and try again.")
        }
        doc.write(kind: kind, id: id, op: .update, set: set)
        afterLocalWrite()
    }

    private func decision(_ approve: Bool) -> JSONValue {
        .object(["approve": .bool(approve), "at": .int(nowMs())])
    }

    // MARK: Engine calls

    func createBot(_ request: CreateBotRequest, on deviceId: String) async throws -> Bot {
        if let error = Bot.nameError(request.name) { throw BotActionError(error) }
        let name = botDeviceName(deviceId)
        guard deviceOnline(deviceId) else { throw BotActionError("\(name) is offline.") }
        do {
            return try await relayClient(for: deviceId).call(
                method: "CreateBot", params: try jsonParams(request), timeoutSeconds: 30)
        } catch {
            throw BotActionError.from(error, device: name)
        }
    }

    func updateBot(
        botId: String, name: String, icon: String?, color: String?, instructions: String?,
        scope: BotScope, capabilities: BotCapabilities?
    ) async throws {
        guard let bot = bot(id: botId) else { throw BotActionError.missing("workspace") }
        if let error = Bot.nameError(name) ?? scope.validationError { throw BotActionError(error) }
        var params: [String: Any] = [
            "op": "updateBot", "botId": botId, "name": name,
            "icon": icon ?? NSNull(), "color": color ?? NSNull(), "instructions": instructions ?? NSNull(),
            "scope": try jsonObject(scope),
        ]
        if let capabilities { params["capabilities"] = try jsonObject(capabilities) }
        try await mutate(bot, params, hostOnly: false)
    }

    func deleteBot(botId: String) async throws {
        guard let bot = bot(id: botId) else { return }
        // The host stops the workspace's runs and removes its folder; any
        // other device still tombstones its rows.
        try await mutate(bot, ["op": "deleteBot", "botId": botId], hostOnly: false)
    }

    func saveRoutine(_ routine: BotRoutine) async throws {
        guard let bot = bot(id: routine.botId) else { throw BotActionError.missing("workspace") }
        if let error = routine.validationError { throw BotActionError(error) }
        try await mutate(bot, ["op": "saveBotRoutine", "routine": try jsonObject(routine)], hostOnly: false)
    }

    func deleteRoutine(routineId: String) async throws {
        guard let routine = botRecords.routines.first(where: { $0.id == routineId }) else { return }
        let reached = try await mutateIfReachable(
            bot(id: routine.botId), ["op": "deleteBotRoutine", "routineId": routineId])
        guard !reached else { return }
        // No device to ask: tombstone the routine and its sessions here, as
        // the engine would (a worktree it made stays on disk).
        tombstone(("botRoutines", routineId), chats: [routine.threadChatId, routine.manualChatId].compactMap { $0 })
    }

    func deleteWork(workId: String) async throws {
        guard let work = botRecords.work.first(where: { $0.id == workId }) else { return }
        guard !work.isLive else { throw BotActionError.stillLive }
        let reached = try await mutateIfReachable(bot(id: work.botId), ["op": "deleteBotWork", "workId": workId])
        guard !reached else { return }
        // Its background session goes with it; the sessions it started in
        // projects are ordinary sessions and stay.
        tombstone(("botWork", workId), chats: [work.threadChatId].compactMap { $0 })
    }

    func createInvite(botId: String) async throws -> InviteCreated {
        guard let bot = bot(id: botId) else { throw BotActionError.missing("workspace") }
        return try await call(bot, method: "CreateBotInvite", params: ["botId": botId], hostOnly: true)
    }

    func redeemInvite(botId: String, code: String) async throws -> BotConnection {
        guard let bot = bot(id: botId) else { throw BotActionError.missing("workspace") }
        let code = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !code.isEmpty else { throw BotActionError("Paste the invite code.") }
        return try await call(bot, method: "RedeemBotInvite", params: ["botId": botId, "code": code], hostOnly: true)
    }

    /// Every engine holds every store, so any online device answers; the
    /// store's own host first (it runs the passes).
    func watchMemory(scope: MemoryScope) -> AsyncThrowingStream<MemoryView, Error> {
        AsyncThrowingStream { continuation in
            let task = Task { @MainActor [weak self] in
                guard let self else { return continuation.finish() }
                // Like `reach`: a device presence calls online that the
                // relay can't reach moves on to the next one; once a device
                // has answered, its stream's end or failure is the answer.
                let decoder = JSONDecoder()
                for device in self.memoryDevices(scope) {
                    var answered = false
                    do {
                        let params: [String: Any] = ["scope": try jsonObject(scope)]
                        for try await item in self.relayClient(for: device).subscribe(
                            method: "WatchMemory", params: params)
                        {
                            answered = true
                            // A frame this app can't read is skipped; the
                            // next change sends the whole view again.
                            if let view = try? decoder.decode(MemoryView.self, from: item) {
                                continuation.yield(view)
                            }
                        }
                        return continuation.finish()
                    } catch RelayError.hostOffline where !answered, RelayError.notConnected where !answered {
                        continue
                    } catch {
                        return continuation.finish(
                            throwing: BotActionError.from(error, device: self.botDeviceName(device)))
                    }
                }
                continuation.finish(throwing: BotActionError.devicesOffline)
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func editMemory(scope: MemoryScope, edit: MemoryEdit) async throws -> String {
        struct Reply: Decodable { var text: String? }
        let params: [String: Any] = ["scope": try jsonObject(scope), "edit": try jsonObject(edit)]
        let reply: Reply = try await call(
            devices: memoryDevices(scope), method: "EditMemory", params: params, offline: .devicesOffline)
        return reply.text ?? ""
    }

    // MARK: Routing

    /// The devices to ask for `bot`, in order: its host when online, then
    /// (unless only the host can do it) every other online device.
    private func botDevices(_ bot: Bot, hostOnly: Bool) -> [String] {
        var ids = deviceOnline(bot.hostDeviceId) ? [bot.hostDeviceId] : []
        if !hostOnly {
            ids += devices.map(\.id).filter { $0 != bot.hostDeviceId && deviceOnline($0) }
        }
        return ids
    }

    /// The store's host (a workspace's or project's device, else the device
    /// that made the store), then every other online device.
    private func memoryDevices(_ scope: MemoryScope) -> [String] {
        var preferred: String?
        switch scope {
        case .workspace(let botId): preferred = bot(id: botId)?.hostDeviceId
        case .project(let spaceId): preferred = spaces.first { $0.id == spaceId }?.deviceId
        case .you, .role: break
        }
        let host = preferred ?? botRecords.memoryStore(scope: scope)?.hostDeviceId
        var ids = host.map { deviceOnline($0) ? [$0] : [] } ?? []
        ids += devices.map(\.id).filter { $0 != host && deviceOnline($0) }
        return ids
    }

    private func call<T: Decodable & Sendable>(
        _ bot: Bot, method: String, params: [String: Any], hostOnly: Bool
    ) async throws -> T {
        try await call(
            devices: botDevices(bot, hostOnly: hostOnly), method: method, params: params,
            offline: hostOnly ? .hostOffline(bot) : .devicesOffline)
    }

    private func call<T: Decodable & Sendable>(
        devices ids: [String], method: String, params: [String: Any], offline: BotActionError
    ) async throws -> T {
        do {
            return try await reach(ids, method: method, params: params)
        } catch is Unreachable {
            throw offline
        }
    }

    /// No device in the list could be reached.
    private struct Unreachable: Error {}

    /// Ask each device in turn until one answers; only an unreachable one
    /// moves on (a refusal is the answer).
    private func reach<T: Decodable & Sendable>(
        _ ids: [String], method: String, params: [String: Any]
    ) async throws -> T {
        for id in ids {
            do {
                return try await relayClient(for: id).call(method: method, params: params, timeoutSeconds: 20)
            } catch RelayError.hostOffline, RelayError.notConnected {
                continue
            } catch {
                throw BotActionError.from(error, device: botDeviceName(id))
            }
        }
        throw Unreachable()
    }

    private func mutate(_ bot: Bot, _ params: [String: Any], hostOnly: Bool) async throws {
        let _: IgnoredReply = try await call(bot, method: "Mutate", params: params, hostOnly: hostOnly)
    }

    /// `Mutate` on a reachable device; false when none could be reached (the
    /// caller falls back to a local write). A refusal still throws.
    private func mutateIfReachable(_ bot: Bot?, _ params: [String: Any]) async throws -> Bool {
        guard let bot else { return false }
        do {
            let _: IgnoredReply = try await reach(botDevices(bot, hostOnly: false), method: "Mutate", params: params)
            return true
        } catch is Unreachable {
            return false
        }
    }

    /// Tombstone a record with chats it owns, their status rows and their
    /// subagent children, in one batch (the engine's `delete_chat` shape).
    private func tombstone(_ record: (kind: String, id: String), chats chatIds: [String]) {
        var keys: [(kind: String, id: String)] = [record]
        for chatId in chatIds {
            keys += [("chats", chatId), ("sessions", chatId)]
            for child in chats where child.child?.parentChatId == chatId {
                keys += [("chats", child.id), ("sessions", child.id)]
            }
        }
        doc.deleteRows(keys)
        afterLocalWrite()
    }
}

/// An Encodable as the JSON object a relay param holds.
func jsonObject<T: Encodable>(_ value: T) throws -> Any {
    try JSONSerialization.jsonObject(with: JSONEncoder().encode(value), options: .fragmentsAllowed)
}

/// An Encodable struct as a relay call's whole params.
func jsonParams<T: Encodable>(_ value: T) throws -> [String: Any] {
    guard let params = try jsonObject(value) as? [String: Any] else {
        throw EncodingError.invalidValue(value, .init(codingPath: [], debugDescription: "not an object"))
    }
    return params
}

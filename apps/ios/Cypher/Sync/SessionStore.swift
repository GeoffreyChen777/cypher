// v3 session replica. Commands are durable before local acknowledgement;
// transcripts come only from committed host-authored events.

import Foundation
import CryptoKit
import Loro
import Observation

@MainActor
@Observable
final class SessionStore {
    let chatId: String
    /// The chat's host device — nudge target for cold-host command drains.
    var hostDeviceId: String?
    private(set) var entries: [MessageEntry] = []
    /// Bumped on every change to `entries` / `pendingSends`. The transcript's
    /// row builder memoizes on it, so a body re-eval that was triggered by
    /// something else (scrolling) costs O(1) instead of re-deriving every row.
    private(set) var revision: UInt64 = 0
    /// Whether this chat's transcript has already been revealed once.
    ///
    /// Lives on the store, not the view: the reveal gate is `@State`, so any
    /// re-creation of TranscriptView reset it to "hidden" and blanked an
    /// already-visible transcript until the settle loop finished. The store is
    /// cached per chat, so it outlives that churn.
    @ObservationIgnored var hasRevealed = false
    /// Transcript parse/row cache — store-owned so parses survive view
    /// churn, and prewarmed off-main whenever a projection lands so opening
    /// the chat never parses markdown inside the first body pass.
    @ObservationIgnored let transcriptCache = TranscriptBuilderCache()
    private(set) var connected = false
    /// Client-minted ids of sends the host hasn't materialized yet.
    private(set) var pendingSends: [PendingSend] = []

    @ObservationIgnored private(set) var sync3Journal: Sync3Journal?
    @ObservationIgnored private var sync3Client: Sync3Client?
    private(set) var error: String?
    private let config: AppConfig
    @ObservationIgnored private var started = false
    @ObservationIgnored private var transportGeneration: UInt64 = 0
    @ObservationIgnored private var projectedCursor: Int64?
    @ObservationIgnored private var readInterest: UUID?
    @ObservationIgnored private weak var workspaceContext: Workspace3Context?

    /// Demo mode: no room, entries driven externally.
    private let offline: Bool
    /// Demo hook: invoked instead of the command plane when offline.
    @ObservationIgnored var demoResponder: ((String, Bool) -> Void)?

    init(chatId: String, config: AppConfig, offline: Bool = false, journalURL: URL? = nil) {
        self.chatId = chatId
        self.config = config
        self.offline = offline
        if !offline {
            do {
                let account = String(data: try JSONEncoder().encode([config.orgId, config.userId]), encoding: .utf8)!
                let key = SHA256.hash(data: try JSONEncoder().encode(
                    [config.edgeURL.absoluteString, config.orgId, config.userId, config.deviceId, chatId]
                )).map { String(format: "%02x", $0) }.joined()
                let directory = FileManager.default.urls(for: .applicationSupportDirectory,
                    in: .userDomainMask)[0].appendingPathComponent("CypherV3", isDirectory: true)
                let url = journalURL ?? directory.appendingPathComponent("\(key).sqlite")
                try FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                    withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
                sync3Journal = try Sync3Journal(url: url, account: account, room: chatId, actor: config.deviceId)
                pendingSends = try sync3Journal!.pendingSends()
            } catch {
                self.error = String(describing: error)
            }
        }
        AttachmentImageCache.shared.configure(config: config)
    }

    // MARK: Attachments (uploads target the chat's host device)

    @ObservationIgnored private var hostRelay: (deviceId: String, client: WorkspaceRemote)?

    /// Chunked upload of one staged image to the host device; returns the
    /// durable absolute path on that device (what the refs trailer carries).
    func uploadAttachment(name: String, data: Data) async throws -> String {
        guard let hostDeviceId else { throw RelayError.hostOffline }
        let relay: WorkspaceRemote
        if let hostRelay, hostRelay.deviceId == hostDeviceId {
            relay = hostRelay.client
        } else {
            relay = WorkspaceRemote(deviceId: hostDeviceId, config: config)
            hostRelay = (hostDeviceId, relay)
        }
        return try await uploadAttachmentChunked(relay: relay, name: name, data: data)
    }

    /// Demo-mode injection point (also used by previews).
    func setEntries(_ new: [MessageEntry]) {
        entries = new
        revision &+= 1
        transcriptCache.prewarm(entries: entries)
    }

    func start() {
        guard !started, !offline else { return }
        do {
            let context = try config.workspaceContext()
            readInterest = try context.want(chatId)
            workspaceContext = context
        } catch { self.error = error.localizedDescription; return }
        started = true
        project()
        connect()
    }

    private func connect() {
        if let journal = sync3Journal {
            transportGeneration &+= 1
            let generation = transportGeneration
            let repair = Sync3HTTPTransport(request: { [config, chatId] in
                try await config.sync3Request(chatId: chatId, socket: false)
            })
            let client = Sync3Client(
                journal: journal,
                request: { [config, chatId] in
                    try await config.sync3Request(chatId: chatId, socket: true)
                },
                repair: { try await repair.exchange($0) })
            client.onChange = { [weak self, weak client] in
                guard let self, let client, self.transportGeneration == generation else { return }
                self.connected = client.status.phase == "live"
                self.error = client.status.error
                self.project()
            }
            sync3Client = client
        }
    }

    /// Foreground recovery cancels the old transport before replacing it.
    func kickRoom() {
        guard started, !offline else { return }
        transportGeneration &+= 1
        let generation = transportGeneration
        let old = sync3Client
        sync3Client = nil
        old?.onChange = nil
        connected = false
        Task { [weak self] in
            await old?.stop()
            guard let self, self.started, self.transportGeneration == generation else { return }
            self.connect()
        }
    }

    func stop() {
        started = false
        if let readInterest { workspaceContext?.release(readInterest) }
        readInterest = nil
        if let hostRelay { Task { await hostRelay.client.close() } }
        hostRelay = nil
        transportGeneration &+= 1
        if let sync3Client {
            sync3Client.onChange = nil
            Task { await sync3Client.stop() }
        }
        sync3Client = nil
        connected = false
    }

    // MARK: Projection
    func project() {
        guard let journal = sync3Journal else { return }
        do {
            let cursor = try journal.cursor
            guard projectedCursor != cursor else { return }
            pendingSends = try journal.pendingSends()
            let snapshot = try journal.renderSnapshot()
            apply(try Self.decodeEntries(records: snapshot.messages, steerIDs: snapshot.steerIDs))
            projectedCursor = snapshot.through
        } catch {
            self.error = String(describing: error)
        }
    }

    private func apply(_ decoded: [MessageEntry]) {
        entries = decoded
        // Drop echoes the host has materialized.
        let ids = Set(entries.map(\.id))
        pendingSends.removeAll { ids.contains($0.messageId) }
        revision &+= 1
        // If no transcript view is open, settle the parses now (off-main) so
        // the eventual open is memo hits all the way down.
        transcriptCache.prewarm(entries: entries)
    }

    /// Whole-doc decode. `nil` means the doc has no map root yet — leave the
    /// previous projection standing rather than blanking a live transcript.
    nonisolated static func decodeEntries(from doc: LoroDoc) -> [MessageEntry]? {
        guard let root = doc.getDeepValue().mapValue else { return nil }
        // Commands are append-only and sync with the transcript. Join explicit
        // steer message IDs instead of guessing from timing/status, so the
        // optimistic echo and a reopened/cross-device transcript agree.
        // "applied" only means routed/queued, not consumed: don't expose it as
        // a delivery receipt. Old messages without a matching ID stay normal.
        let steerIDs = Set((root["commands"]?.listValue ?? []).compactMap { command -> String? in
            guard let command = command.mapValue,
                  command["kind"]?.stringValue == "steer",
                  let payload = command["payload"]?.mapValue,
                  payload["kind"]?.stringValue == "steer",
                  let id = payload["messageId"]?.stringValue, !id.isEmpty else { return nil }
            return id
        })
        let raw = (root["messages"]?.listValue ?? []).compactMap(entryFrom).map { entry in
            var entry = entry
            entry.isSteer = entry.role == .user && steerIDs.contains(entry.id)
            return entry
        }
        return joinContinuations(raw)
    }

    /// Only committed creation position establishes order. Device clocks and
    /// the order of dictionary iteration never participate in ordering.
    nonisolated static func decodeEntries(from projection: Sync3Projection) throws -> [MessageEntry] {
        let steerIDs = Set(projection.commands.values.compactMap { record -> String? in
            let payload = record["command"]?.objectValue?["payload"]?.objectValue
            return payload?["kind"] == .string("steer") ? payload?["messageId"]?.stringValue : nil
        })
        let records = projection.messages.values.sorted {
            ($0["createdSeq"]?.int64Value ?? 0) < ($1["createdSeq"]?.int64Value ?? 0)
        }
        return try decodeEntries(records: records, steerIDs: steerIDs)
    }

    nonisolated static func decodeEntries(records: [[String: JSONValue]], steerIDs: Set<String>) throws -> [MessageEntry] {
        return try joinContinuations(records.map { record in
            guard let entry = record["entry"]?.objectValue,
                  let id = entry["id"]?.stringValue,
                  let role = entry["role"]?.stringValue.flatMap(MessageRole.init(rawValue:)),
                  let createdAt = entry["createdAt"]?.int64Value,
                  let deviceId = entry["deviceId"]?.stringValue,
                  case .array(let rawParts) = entry["parts"] else { throw Sync3Error.protocolError("invalid_message") }
            let parts = try rawParts.map(decodePart)
            let status = entry["status"]?.stringValue.flatMap(MessageStatus.init(rawValue:))
            return MessageEntry(id: id, role: role, parts: parts, createdAt: createdAt,
                                deviceId: deviceId, status: status,
                                continuationOf: entry["continuationOf"]?.stringValue,
                                isSteer: role == .user && steerIDs.contains(id))
        })
    }

    nonisolated static func decodePart(_ value: JSONValue) throws -> MessagePart {
        guard let part = value.objectValue, let id = part["id"]?.stringValue else {
            throw Sync3Error.protocolError("invalid_part")
        }
        switch part["kind"]?.stringValue {
        case "text":
            guard let text = part["text"]?.stringValue else { throw Sync3Error.protocolError("invalid_part") }
            return .text(id: id, text: text)
        case "error":
            guard let text = part["message"]?.stringValue else { throw Sync3Error.protocolError("invalid_part") }
            return .error(id: id, message: text)
        case "input":
            guard let requestId = part["requestId"]?.stringValue,
                  let resolved = part["resolved"]?.boolValue, let questions = part["questions"] else {
                throw Sync3Error.protocolError("invalid_part")
            }
            return .input(id: id, requestId: requestId,
                questions: try JSONDecoder().decode([UserInputQuestion].self, from: JSONEncoder().encode(questions)),
                resolved: resolved)
        case "tool":
            guard let rawCall = part["call"]?.objectValue, let tag = rawCall["kind"]?.stringValue,
                  let resolved = part["resolved"]?.boolValue, let isError = part["isError"]?.boolValue else {
                throw Sync3Error.protocolError("invalid_part")
            }
            var fields: [String: AnyHashable] = [:]
            for (key, value) in rawCall where key != "kind" {
                switch value {
                case .string(let v): fields[key] = v
                case .bool(let v): fields[key] = v
                case .int(let v): fields[key] = v
                case .double(let v): fields[key] = v
                default: fields[key] = value
                }
            }
            var call = RenderToolCall(tag: tag, fields: fields)
            if tag == "unknown", rawCall["name"] == .string("subagent"),
               let input = rawCall["input"]?.objectValue, let agent = input["agent"]?.stringValue {
                call.subagent = SubagentCallMetadata(agent: String(agent.prefix(120)),
                    task: String((input["task"]?.stringValue ?? "").prefix(500)),
                    isAsync: input["async"]?.boolValue ?? false)
            }
            call.progress = SubagentProjection.boundedProgress(part["progress"]?.stringValue)
            call.details = part.filter { !["kind", "id", "call", "resolved", "isError"].contains($0.key) }
            return .tool(id: id, call: call, isError: isError, resolved: resolved)
        default:
            throw Sync3Error.protocolError("invalid_part")
        }
    }

    nonisolated private static func entryFrom(_ value: LoroValue) -> MessageEntry? {
        guard let m = value.mapValue,
              let id = m["id"]?.stringValue,
              let roleStr = m["role"]?.stringValue,
              let role = MessageRole(rawValue: roleStr) else { return nil }
        let parts = (m["parts"]?.listValue ?? []).compactMap(partFrom)
        return MessageEntry(id: id, role: role, parts: parts,
                            createdAt: m["createdAt"]?.i64Value ?? 0,
                            deviceId: m["deviceId"]?.stringValue ?? "",
                            status: m["status"]?.stringValue.flatMap(MessageStatus.init(rawValue:)),
                            continuationOf: m["continuationOf"]?.stringValue)
    }

    nonisolated private static func partFrom(_ value: LoroValue) -> MessagePart? {
        guard let m = value.mapValue,
              let id = m["id"]?.stringValue,
              let kind = m["kind"]?.stringValue else { return nil }
        switch kind {
        case "text":
            return .text(id: id, text: m["text"]?.stringValue ?? "")
        case "tool":
            guard let callMap = m["call"]?.mapValue else { return nil }
            let tag = callMap["kind"]?.stringValue ?? "unknown"
            var fields: [String: AnyHashable] = [:]
            for (k, v) in callMap where k != "kind" {
                if let s = v.stringValue { fields[k] = s }
                else if let b = v.boolValue { fields[k] = b }
                else if let i = v.i64Value { fields[k] = i }
                else if let list = v.listValue {
                    // ApplyPatch changes / Todo items — keep a JSON echo.
                    fields[k] = list.map { "\($0.jsonObject)" }
                }
            }
            // isError presence IS the resolution marker (schema.rs:96).
            let isError = m["isError"]?.boolValue
            var call = RenderToolCall(tag: tag, fields: fields)
            if tag == "unknown", callMap["name"]?.stringValue == "subagent",
               let input = callMap["input"]?.mapValue,
               let agent = input["agent"]?.stringValue, !agent.isEmpty {
                call.subagent = SubagentCallMetadata(agent: String(agent.prefix(120)),
                    task: String((input["task"]?.stringValue ?? "").prefix(500)),
                    isAsync: input["async"]?.boolValue ?? false)
                call.progress = SubagentProjection.boundedProgress(m["progress"]?.stringValue)
            }
            return .tool(id: id, call: call,
                         isError: isError ?? false, resolved: isError != nil)
        case "input":
            var questions: [UserInputQuestion] = []
            if let list = m["questions"]?.listValue,
               let data = try? JSONSerialization.data(withJSONObject: list.map(\.jsonObject)),
               let decoded = try? JSONDecoder().decode([UserInputQuestion].self, from: data) {
                questions = decoded
            }
            return .input(id: id, requestId: id, questions: questions,
                          resolved: m["resolved"]?.boolValue ?? false)
        case "error":
            return .error(id: id, message: m["message"]?.stringValue ?? "")
        default:
            return nil
        }
    }

    /// schema.rs join_continuation_entries: concatenate continuation parts onto
    /// the root in list order; orphans surface standalone.
    nonisolated static func joinContinuations(_ raw: [MessageEntry]) -> [MessageEntry] {
        var roots: [MessageEntry] = []
        var index: [String: Int] = [:]
        for entry in raw {
            if let rootId = entry.continuationOf, let ix = index[rootId] {
                roots[ix].parts.append(contentsOf: entry.parts)
            } else {
                index[entry.id] = roots.count
                roots.append(entry)
            }
        }
        return roots
    }

    // MARK: Derived

    var lastEntryId: String? { entries.last?.id }

    var liveEntry: MessageEntry? {
        entries.last(where: { $0.status == .streaming })
    }

    /// The unresolved input request to surface in the question panel.
    var openInputRequest: (entryId: String, requestId: String, questions: [UserInputQuestion])? {
        for entry in entries.reversed() {
            for part in entry.parts.reversed() {
                // An empty question list can't be answered, so it must not take
                // the composer's place — leaving the user with no way to type.
                if case .input(_, let requestId, let questions, let resolved) = part,
                   !resolved, !questions.isEmpty {
                    return (entry.id, requestId, questions)
                }
            }
        }
        return nil
    }

    // MARK: Command plane (ledger rule 1: append-only, own entries only)

    @discardableResult
    func sendRun(prompt: String, chat: Chat, attachments: [String] = [], agentPrompt: String? = nil) -> Bool {
        guard !CommentPrompt.blocksSlash(prompt, hasComments: agentPrompt != nil) else { return false }
        if offline {
            demoResponder?(prompt, false)
            return true
        }
        let messageId = UUID().uuidString.lowercased()
        let request = RunRequest(prompt: prompt,
                                 harness: chat.config?.harness,
                                 model: chat.config?.model,
                                 reasoning: chat.config?.reasoning,
                                 modelOptions: chat.config?.modelOptions ?? [:],
                                 cwd: chat.cwd ?? "~",
                                 sandbox: chat.config?.sandbox ?? "workspace-write",
                                 attachments: attachments)
        var payload: [String: Any] = [
            "kind": "run",
            "request": encodableJSON(request),
            "messageId": messageId,
        ]
        if let agentPrompt { payload["agentPrompt"] = agentPrompt }
        guard queueCommand(kind: "run", payload: payload) else { return false }
        pendingSends.append(PendingSend(messageId: messageId, text: prompt, at: nowMs()))
        revision &+= 1
        return true
    }

    @discardableResult
    func sendSteer(prompt: String, agentPrompt: String? = nil) -> Bool {
        guard !CommentPrompt.blocksSlash(prompt, hasComments: agentPrompt != nil) else { return false }
        if offline {
            demoResponder?(prompt, true)
            return true
        }
        let messageId = UUID().uuidString.lowercased()
        var payload: [String: Any] = [
            "kind": "steer",
            "prompt": prompt,
            "messageId": messageId,
        ]
        if let agentPrompt { payload["agentPrompt"] = agentPrompt }
        guard queueCommand(kind: "steer", payload: payload) else { return false }
        pendingSends.append(PendingSend(messageId: messageId, text: prompt, at: nowMs(), isSteer: true))
        revision &+= 1
        return true
    }

    @discardableResult
    func sendInterrupt() -> Bool {
        queueCommand(kind: "interrupt", payload: ["kind": "interrupt"])
    }

    @discardableResult
    func respondInput(requestId: String, answers: [UserInputAnswer]) -> Bool {
        queueCommand(kind: "respondInput", payload: [
            "kind": "respondInput",
            "requestId": requestId,
            "answers": answers.map(encodableJSON),
        ])
    }

    /// schema.rs queue_command, field for field.
    private func queueCommand(kind: String, payload: [String: Any]) -> Bool {
        // v3 is the durable command authority. Keep command creation in one
        // place so run/steer/interrupt/input responses cannot accidentally
        // fall back to an old document relay.
        guard let journal = sync3Journal else { return false }
        do {
            let commandId = UUID().uuidString.lowercased()
            var commandPayload = payload
            commandPayload["kind"] = kind
            if kind == "run", var request = commandPayload["request"] as? [String: Any] {
                // The closed v3 descriptor distinguishes a nullable field
                // from an omitted field. Swift Codable omits `resume` when it
                // is nil, so restore the explicit null required by the wire
                // contract before validation.
                if request["resume"] == nil { request["resume"] = NSNull() }
                if request["model"] == nil { request["model"] = NSNull() }
                if request["reasoning"] == nil { request["reasoning"] = NSNull() }
                if let attachments = request["attachments"] as? [String], attachments.isEmpty {
                    request.removeValue(forKey: "attachments")
                }
                commandPayload["request"] = request
            }
            let now = nowMs()
            var command: [String: JSONValue] = [
                "id": .string(commandId),
                "issuedBy": .string(config.deviceId),
                "issuedAt": .int(now),
                "expiresAt": .int(now + commandDefaultTtlMs),
                "status": .string("pending"),
                "resolution": .null,
                "payload": try jsonValue(commandPayload),
            ]
            if let turnId = lastEntryId {
                command["basedOn"] = .object(["turnId": .string(turnId), "frontier": .null])
            } else {
                command["basedOn"] = .null
            }
            let operation = try Sync3Operation(
                    id: UUID().uuidString.lowercased(),
                    actor: config.deviceId,
                    ownerEpoch: max(1, (try journal.ownerEpoch)),
                    event: [
                        "type": .string("commandQueued"),
                        "commandId": .string(commandId),
                        "command": .object(command),
                    ])
            try journal.enqueue(operation)
            sync3Client?.wake()
            nudgeHost()
            return true
        } catch {
            self.error = String(describing: error)
            return false
        }
    }

    /// Durable-nudge the host device so a cold host opens the doc and drains
    /// (doc_host.rs nudge_remote_host). Fire-and-forget; the command is
    /// durable in the doc regardless.
    private func nudgeHost() {
        // Read interest wakes the owning host; no legacy HTTP nudge exists.
        workspaceContext?.client.probe()
    }
}

private func encodableJSON<T: Encodable>(_ value: T) -> Any {
    guard let data = try? JSONEncoder().encode(value),
          let obj = try? JSONSerialization.jsonObject(with: data) else { return [:] }
    return obj
}

private func jsonValue(_ value: Any) throws -> JSONValue {
    try JSONDecoder().decode(JSONValue.self, from: JSONSerialization.data(withJSONObject: value))
}

private func jsonObject(_ value: JSONValue) -> Any {
    switch value {
    case .null: return NSNull()
    case .bool(let value): return value
    case .int(let value): return value
    case .double(let value): return value
    case .string(let value): return value
    case .array(let values): return values.map(jsonObject)
    case .object(let values): return values.mapValues(jsonObject)
    }
}

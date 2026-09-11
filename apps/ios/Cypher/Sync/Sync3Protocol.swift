// Experimental Sync v3. Typed event validation + deterministic projection.
// Normal SessionStore continues using chat2 until the migration gate passes.
import Foundation

enum Sync3Error: Error, Equatable { case protocolError(String) }
enum Sync3Wire {
    static let version: Int64 = 3
    static let maxFrameBytes = 256 * 1024
    static let maxSafeInteger: Int64 = 9_007_199_254_740_991
    static func fail(_ code: String) throws -> Never { throw Sync3Error.protocolError(code) }
    static func identifier(_ value: JSONValue?) throws -> String {
        guard case .string(let s) = value, !s.isEmpty, s.utf8.count <= 128,
              s.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0)
                  || (97...122).contains($0) || $0 == 45 || $0 == 95 })
        else { try fail("invalid_id") }
        return s
    }
    static func integer(_ value: JSONValue?) throws -> Int64 {
        guard case .int(let n) = value, n >= 0, n <= maxSafeInteger else { try fail("invalid_integer") }
        return n
    }
    static func entityIdentifier(_ value: JSONValue?) throws -> String {
        guard case .string(let s) = value, !s.isEmpty, s.utf8.count <= 200,
              s.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0)
                  || (97...122).contains($0) || [45, 95, 46, 58, 35, 126].contains($0) })
        else { try fail("invalid_id") }
        return s
    }
    static func string(_ value: JSONValue?) throws -> String {
        guard case .string(let s) = value else { try fail("invalid_text") }
        return s
    }
    static func shape(_ value: [String: JSONValue], _ keys: [String]) throws {
        guard Set(value.keys) == Set(keys) else { try fail("invalid_shape") }
    }
    static func validateJSON(_ value: JSONValue, depth: Int = 0) throws {
        guard depth <= 32 else { try fail("json_too_deep") }
        switch value {
        case .int(let n):
            guard n.magnitude <= UInt64(maxSafeInteger) else { try fail("invalid_number") }
        case .double(let n):
            guard n.isFinite, n.rounded() != n || abs(n) <= Double(maxSafeInteger) else { try fail("invalid_number") }
        case .object(let values):
            for child in values.values { try validateJSON(child, depth: depth + 1) }
        case .array(let values):
            for child in values { try validateJSON(child, depth: depth + 1) }
        default: break
        }
    }
    static func canonicalJSON(_ value: JSONValue) -> JSONValue {
        switch value {
        case .double(let n) where n.isFinite && n.rounded() == n && abs(n) <= Double(maxSafeInteger):
            return .int(Int64(n))
        case .object(let values): return .object(values.mapValues(canonicalJSON))
        case .array(let values): return .array(values.map(canonicalJSON))
        default: return value
        }
    }
    static func decode(_ data: Data) throws -> [String: JSONValue] {
        guard data.count <= maxFrameBytes else { try fail("frame_too_large") }
        return try JSONDecoder().decode([String: JSONValue].self, from: data)
    }
    static func encode<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(value)
        guard data.count <= maxFrameBytes else { try fail("frame_too_large") }
        return data
    }
}

struct Sync3Operation: Codable, Equatable, Sendable {
    let id: String
    let actor: String
    let ownerEpoch: Int64
    let event: [String: JSONValue]

    init(id: String, actor: String, ownerEpoch: Int64, event: [String: JSONValue]) throws {
        try Sync3Wire.validateJSON(.object(event), depth: 1)
        self.id = id; self.actor = actor; self.ownerEpoch = ownerEpoch
        self.event = event.mapValues(Sync3Wire.canonicalJSON)
        try validate()
    }
    init(from decoder: Decoder) throws {
        let object = try decoder.singleValueContainer().decode([String: JSONValue].self)
        try Sync3Wire.shape(object, ["id", "actor", "ownerEpoch", "event"])
        guard let event = object["event"]?.objectValue else { try Sync3Wire.fail("invalid_event") }
        try self.init(id: Sync3Wire.identifier(object["id"]), actor: Sync3Wire.identifier(object["actor"]),
                      ownerEpoch: Sync3Wire.integer(object["ownerEpoch"]), event: event)
    }
    func validate() throws {
        _ = try Sync3Wire.identifier(.string(id)); _ = try Sync3Wire.identifier(.string(actor))
        guard ownerEpoch > 0, ownerEpoch <= Sync3Wire.maxSafeInteger else { try Sync3Wire.fail("invalid_epoch") }
        let type = try Sync3Wire.string(event["type"])
        let fields: [String: [String]] = [
            "commandQueued": ["commandId", "command"], "commandAccepted": ["commandId", "runId"],
            "commandResolved": ["commandId", "status", "resolution"], "commandCancelled": ["commandId"],
            "runStarted": ["runId"], "messageCreated": ["runId", "messageId", "role", "deviceId", "createdAt", "continuationOf"],
            "partPut": ["messageId", "index", "part"], "textAppended": ["messageId", "partId", "offset", "text"],
            "messageFinished": ["messageId", "status"], "attachmentSealed": ["uploadId", "path", "fileName"],
            "runFinished": ["runId", "outcome"],
        ]
        guard let keys = fields[type] else { try Sync3Wire.fail("invalid_event") }
        try Sync3Wire.shape(event, ["type"] + keys)
        for key in ["commandId", "runId", "deviceId", "uploadId"] where event[key] != nil {
            if key == "runId", type == "messageCreated", event[key] == .null { continue }
            _ = try Sync3Wire.identifier(event[key])
        }
        for key in ["messageId", "partId"] where event[key] != nil {
            _ = try Sync3Wire.entityIdentifier(event[key])
        }
        for key in ["text", "path", "fileName"] where event[key] != nil {
            _ = try Sync3Wire.string(event[key])
        }
        if type == "textAppended" { _ = try Sync3Wire.integer(event["offset"]) }
        if type == "messageCreated", ![JSONValue.string("user"), .string("assistant"), .string("system")].contains(event["role"]) {
            try Sync3Wire.fail("invalid_role")
        }
        if type == "messageCreated" {
            _ = try Sync3Wire.integer(event["createdAt"])
            if event["continuationOf"] != .null {
                _ = try Sync3Wire.entityIdentifier(event["continuationOf"])
                if event["continuationOf"] == event["messageId"] { try Sync3Wire.fail("invalid_continuation") }
            }
        }
        if type == "partPut" {
            guard try Sync3Wire.integer(event["index"]) < 256 else { try Sync3Wire.fail("too_many_parts") }
            try Sync3CommandSchema.validatePart(event["part"]!)
        }
        if type == "messageFinished", ![JSONValue.null, .string("complete"), .string("aborted")].contains(event["status"]) {
            try Sync3Wire.fail("invalid_message_status")
        }
        if type == "attachmentSealed", event["path"] == .string("") || event["fileName"] == .string("") {
            try Sync3Wire.fail("invalid_attachment")
        }
        if type == "runFinished", !["completed", "failed", "interrupted"].contains(event["outcome"]?.stringValue ?? "") {
            try Sync3Wire.fail("invalid_outcome")
        }
        if type == "commandQueued" {
            guard let cmd = event["command"]?.objectValue else { try Sync3Wire.fail("invalid_command") }
            try Sync3CommandSchema.validate(.object(cmd))
            guard cmd["status"] == .string("pending"), cmd["resolution"] == .null else { try Sync3Wire.fail("command_not_pending") }
            guard cmd["id"] == event["commandId"], cmd["issuedBy"] == .string(actor) else { try Sync3Wire.fail("command_identity_mismatch") }
            if cmd["payload"]?.objectValue?["kind"] == .string("interrupt"),
               cmd["basedOn"]?.objectValue?["turnId"]?.stringValue == nil {
                try Sync3Wire.fail("interrupt_requires_target")
            }
        }
        if type == "commandResolved" {
            guard ["applied", "rejected", "expired", "superseded"].contains(event["status"]?.stringValue ?? "") else {
                try Sync3Wire.fail("invalid_command_resolution")
            }
            if event["resolution"] != .null { _ = try Sync3Wire.string(event["resolution"]) }
        }
        try Sync3Wire.validateJSON(.object(event), depth: 1)
        guard try Sync3Wire.encode(self).count <= Sync3Wire.maxFrameBytes / 2 else { try Sync3Wire.fail("operation_too_large") }
    }
}

struct Sync3Row: Codable, Equatable, Sendable { let seq: Int64; let operation: Sync3Operation }
struct Sync3Projection: Codable, Equatable, Sendable {
    var commands: [String: [String: JSONValue]] = [:]
    var runs: [String: [String: JSONValue]] = [:]
    var messages: [String: [String: JSONValue]] = [:]
    var attachments: [String: [String: JSONValue]] = [:]

    var tables: [String: [String: [String: JSONValue]]] {
        ["commands": commands, "runs": runs, "messages": messages, "attachments": attachments]
    }
    /// Validate disk rows before the reducer touches them. A damaged cache
    /// becomes a recovery error, never a forced-unwrap process crash.
    mutating func install(kind: String, id: String, record: [String: JSONValue]) throws {
        _ = try Sync3Wire.entityIdentifier(.string(id))
        func nullableID(_ value: JSONValue?) throws {
            if value != .null { _ = try Sync3Wire.identifier(value) }
        }
        switch kind {
        case "commands":
            try Sync3Wire.shape(record, ["command", "actor", "runId"])
            try Sync3CommandSchema.validate(record["command"]!)
            guard let command = record["command"]?.objectValue, command["id"] == .string(id),
                  command["issuedBy"] == record["actor"] else { try Sync3Wire.fail("invalid_projection") }
            _ = try Sync3Wire.identifier(record["actor"])
            try nullableID(record["runId"]); commands[id] = record
        case "runs":
            try Sync3Wire.shape(record, ["outcome"])
            guard record["outcome"] == .null ||
                    ["completed", "failed", "interrupted"].contains(record["outcome"]?.stringValue ?? "") else {
                try Sync3Wire.fail("invalid_projection")
            }
            runs[id] = record
        case "messages":
            try Sync3Wire.shape(record, ["runId", "entry"]); try nullableID(record["runId"])
            guard let entry = record["entry"]?.objectValue, entry["id"] == .string(id),
                  case .array(let parts) = entry["parts"], parts.count <= 256 else { try Sync3Wire.fail("invalid_message") }
            let required: Set<String> = ["id", "role", "parts", "createdAt", "deviceId"]
            guard required.isSubset(of: Set(entry.keys)),
                  Set(entry.keys).isSubset(of: required.union(["status", "continuationOf"])) else { try Sync3Wire.fail("invalid_shape") }
            _ = try Sync3Wire.identifier(entry["deviceId"]); _ = try Sync3Wire.integer(entry["createdAt"])
            guard ["user", "assistant", "system"].contains(entry["role"]?.stringValue ?? "") else { try Sync3Wire.fail("invalid_role") }
            if let status = entry["status"], ![JSONValue.string("streaming"), .string("complete"), .string("aborted")].contains(status) {
                try Sync3Wire.fail("invalid_message_status")
            }
            if let parent = entry["continuationOf"] {
                _ = try Sync3Wire.entityIdentifier(parent)
                guard parent != entry["id"] else { try Sync3Wire.fail("invalid_continuation") }
            }
            var ids = Set<String>()
            for part in parts {
                try Sync3CommandSchema.validatePart(part, stored: true)
                guard ids.insert(part.objectValue!["id"]!.stringValue!).inserted else { try Sync3Wire.fail("part_exists") }
            }
            try putMessage(id, record)
        case "attachments":
            try Sync3Wire.shape(record, ["path", "fileName"])
            guard !(try Sync3Wire.string(record["path"])).isEmpty, !(try Sync3Wire.string(record["fileName"])).isEmpty else {
                try Sync3Wire.fail("invalid_attachment")
            }
            attachments[id] = record
        default: try Sync3Wire.fail("invalid_entity_kind")
        }
    }
    private func live(_ run: String) throws {
        guard runs[run]?["outcome"] == .null else { try Sync3Wire.fail("run_not_live") }
    }
    private func writable(_ id: String) throws -> [String: JSONValue] {
        guard let msg = messages[id] else { try Sync3Wire.fail("unknown_message") }
        if let run = msg["runId"]?.stringValue { try live(run) }
        guard msg["entry"]?.objectValue?["status"] == .string("streaming") else { try Sync3Wire.fail("message_finished") }
        return msg
    }
    private mutating func putMessage(_ id: String, _ record: [String: JSONValue]) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard try encoder.encode(record).count <= 256 * 1024 else { try Sync3Wire.fail("message_too_large") }
        messages[id] = record
    }
    mutating func apply(_ op: Sync3Operation, owner: String, ownerEpoch: Int64) throws {
        try op.validate()
        guard op.ownerEpoch == ownerEpoch else { try Sync3Wire.fail("stale_owner_epoch") }
        let e = op.event, type = e["type"]!.stringValue!
        if type != "commandQueued", type != "commandCancelled", op.actor != owner { try Sync3Wire.fail("not_owner") }
        switch type {
        case "commandQueued":
            let id = e["commandId"]!.stringValue!
            guard commands[id] == nil else { try Sync3Wire.fail("command_exists") }
            commands[id] = ["command": e["command"]!, "actor": .string(op.actor), "runId": .null]
        case "commandAccepted":
            let id = e["commandId"]!.stringValue!
            guard var cmd = commands[id] else { try Sync3Wire.fail("unknown_command") }
            guard cmd["command"]?.objectValue?["status"] == .string("pending") else { try Sync3Wire.fail("command_resolved") }
            guard cmd["runId"] == .null else { try Sync3Wire.fail("command_already_accepted") }
            cmd["runId"] = e["runId"]; commands[id] = cmd
        case "commandResolved", "commandCancelled":
            let id = e["commandId"]!.stringValue!
            guard var cmd = commands[id], var entry = cmd["command"]?.objectValue else { try Sync3Wire.fail("unknown_command") }
            if type == "commandCancelled" {
                guard cmd["actor"] == .string(op.actor) else { try Sync3Wire.fail("not_command_author") }
                guard cmd["runId"] == .null, entry["status"] == .string("pending") else { try Sync3Wire.fail("command_not_cancellable") }
                entry["status"] = .string("cancelled")
            } else {
                guard entry["status"] == .string("pending") else { try Sync3Wire.fail("command_resolved") }
                if e["status"] == .string("applied"), cmd["runId"] == .null { try Sync3Wire.fail("command_not_accepted") }
                entry["status"] = e["status"]; entry["resolution"] = e["resolution"]
            }
            cmd["command"] = .object(entry); commands[id] = cmd
        case "runStarted":
            let run = e["runId"]!.stringValue!
            guard runs[run] == nil else { try Sync3Wire.fail("run_exists") }
            guard commands.values.contains(where: {
                $0["runId"] == e["runId"] && ["pending", "applied"].contains($0["command"]?.objectValue?["status"]?.stringValue ?? "")
            }) else { try Sync3Wire.fail("run_not_accepted") }
            runs[run] = ["outcome": .null]
        case "messageCreated":
            let id = e["messageId"]!.stringValue!
            if let run = e["runId"]?.stringValue { try live(run) }
            guard messages[id] == nil else { try Sync3Wire.fail("message_exists") }
            var entry: [String: JSONValue] = ["id": .string(id), "role": e["role"]!, "parts": .array([]),
                                             "createdAt": e["createdAt"]!, "deviceId": e["deviceId"]!, "status": .string("streaming")]
            if e["continuationOf"] != .null { entry["continuationOf"] = e["continuationOf"] }
            messages[id] = ["runId": e["runId"]!, "entry": .object(entry)]
        case "partPut":
            let id = e["messageId"]!.stringValue!
            var msg = try writable(id), entry = msg["entry"]!.objectValue!
            guard case .array(var parts) = entry["parts"] else { try Sync3Wire.fail("invalid_message") }
            let index = Int(try Sync3Wire.integer(e["index"])), part = e["part"]!, p = part.objectValue!
            guard index <= parts.count else { try Sync3Wire.fail("part_gap") }
            if index < parts.count {
                let old = parts[index].objectValue!
                guard old["id"] == p["id"], old["kind"] == p["kind"] else { try Sync3Wire.fail("part_identity_mismatch") }
                if p["kind"] == .string("text"), old["text"] != p["text"] { try Sync3Wire.fail("text_requires_delta") }
                if p["kind"] == .string("input") {
                    guard old["requestId"] == p["requestId"], old["questions"] == p["questions"] else { try Sync3Wire.fail("question_changed") }
                }
                if [JSONValue.string("tool"), .string("input")].contains(p["kind"]),
                   old["resolved"] == .bool(true), p["resolved"] == .bool(false) { try Sync3Wire.fail("part_resolved") }
                parts[index] = part
            } else {
                guard !parts.contains(where: { $0.objectValue?["id"] == p["id"] }) else { try Sync3Wire.fail("part_exists") }
                parts.append(part)
            }
            entry["parts"] = .array(parts); msg["entry"] = .object(entry); try putMessage(id, msg)
        case "textAppended":
            let id = e["messageId"]!.stringValue!
            var msg = try writable(id), entry = msg["entry"]!.objectValue!
            guard case .array(var parts) = entry["parts"],
                  let index = parts.firstIndex(where: { $0.objectValue?["id"] == e["partId"] }) else { try Sync3Wire.fail("unknown_part") }
            var part = parts[index].objectValue!
            guard part["kind"] == .string("text"), let text = part["text"]?.stringValue else { try Sync3Wire.fail("not_text") }
            guard Int64(text.utf8.count) == (try Sync3Wire.integer(e["offset"])) else { try Sync3Wire.fail("text_offset_mismatch") }
            part["text"] = .string(text + e["text"]!.stringValue!); parts[index] = .object(part)
            entry["parts"] = .array(parts); msg["entry"] = .object(entry); try putMessage(id, msg)
        case "messageFinished":
            let id = e["messageId"]!.stringValue!
            var msg = try writable(id), entry = msg["entry"]!.objectValue!
            if e["status"] == .null { entry.removeValue(forKey: "status") } else { entry["status"] = e["status"] }
            msg["entry"] = .object(entry); try putMessage(id, msg)
        case "attachmentSealed":
            let id = e["uploadId"]!.stringValue!, value = ["path": e["path"]!, "fileName": e["fileName"]!]
            if let old = attachments[id], old != value { try Sync3Wire.fail("attachment_conflict") }
            attachments[id] = value
        case "runFinished":
            let run = e["runId"]!.stringValue!
            try live(run)
            guard !messages.values.contains(where: { $0["runId"] == e["runId"] && $0["entry"]?.objectValue?["status"] == .string("streaming") }) else {
                try Sync3Wire.fail("unfinished_messages")
            }
            runs[run] = ["outcome": e["outcome"]!]
        default: try Sync3Wire.fail("invalid_event")
        }
    }
}

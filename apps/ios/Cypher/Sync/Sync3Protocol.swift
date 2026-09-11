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
            "runStarted": ["runId"], "messageCreated": ["runId", "messageId", "role"],
            "textAppended": ["messageId", "offset", "text"], "toolStarted": ["runId", "toolId", "name"],
            "toolFinished": ["toolId", "failed", "summary"], "inputRequested": ["runId", "requestId", "prompt"],
            "runFinished": ["runId", "outcome"],
        ]
        guard let keys = fields[type] else { try Sync3Wire.fail("invalid_event") }
        try Sync3Wire.shape(event, ["type"] + keys)
        for key in ["commandId", "runId", "messageId", "toolId", "requestId"] where event[key] != nil {
            _ = try Sync3Wire.identifier(event[key])
        }
        for key in ["text", "name", "summary", "prompt"] where event[key] != nil {
            _ = try Sync3Wire.string(event[key])
        }
        if type == "textAppended" { _ = try Sync3Wire.integer(event["offset"]) }
        if type == "messageCreated", ![JSONValue.string("user"), .string("assistant")].contains(event["role"]) {
            try Sync3Wire.fail("invalid_role")
        }
        if type == "toolFinished", event["failed"]?.boolValue == nil { try Sync3Wire.fail("invalid_tool_result") }
        if type == "runFinished", !["completed", "failed", "interrupted"].contains(event["outcome"]?.stringValue ?? "") {
            try Sync3Wire.fail("invalid_outcome")
        }
        if type == "commandQueued" {
            guard let cmd = event["command"]?.objectValue else { try Sync3Wire.fail("invalid_command") }
            switch cmd["type"]?.stringValue {
            case "send", "steer":
                try Sync3Wire.shape(cmd, ["type", "text"]); _ = try Sync3Wire.string(cmd["text"])
            case "interrupt": try Sync3Wire.shape(cmd, ["type"])
            case "respondInput":
                try Sync3Wire.shape(cmd, ["type", "requestId", "answer"]); _ = try Sync3Wire.identifier(cmd["requestId"])
            default: try Sync3Wire.fail("invalid_command")
            }
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
    var tools: [String: [String: JSONValue]] = [:]
    var inputs: [String: [String: JSONValue]] = [:]

    var tables: [String: [String: [String: JSONValue]]] {
        ["commands": commands, "runs": runs, "messages": messages, "tools": tools, "inputs": inputs]
    }
    /// Validate disk rows before the reducer touches them. A damaged cache
    /// becomes a recovery error, never a forced-unwrap process crash.
    mutating func install(kind: String, id: String, record: [String: JSONValue]) throws {
        _ = try Sync3Wire.identifier(.string(id))
        func nullableID(_ value: JSONValue?) throws {
            if value != .null { _ = try Sync3Wire.identifier(value) }
        }
        switch kind {
        case "commands":
            try Sync3Wire.shape(record, ["command", "actor", "runId"])
            _ = try Sync3Operation(id: "p", actor: Sync3Wire.identifier(record["actor"]), ownerEpoch: 1,
                                  event: ["type": .string("commandQueued"), "commandId": .string(id), "command": record["command"]!])
            try nullableID(record["runId"]); commands[id] = record
        case "runs":
            try Sync3Wire.shape(record, ["outcome"])
            guard record["outcome"] == .null ||
                    ["completed", "failed", "interrupted"].contains(record["outcome"]?.stringValue ?? "") else {
                try Sync3Wire.fail("invalid_projection")
            }
            runs[id] = record
        case "messages":
            try Sync3Wire.shape(record, ["runId", "role", "text"])
            _ = try Sync3Wire.identifier(record["runId"]); _ = try Sync3Wire.string(record["text"])
            guard ["user", "assistant"].contains(record["role"]?.stringValue ?? "") else { try Sync3Wire.fail("invalid_role") }
            messages[id] = record
        case "tools":
            try Sync3Wire.shape(record, ["runId", "name", "failed", "summary"])
            _ = try Sync3Wire.identifier(record["runId"]); _ = try Sync3Wire.string(record["name"])
            if record["failed"] == .null {
                guard record["summary"] == .null else { try Sync3Wire.fail("invalid_projection") }
            } else {
                guard record["failed"]?.boolValue != nil else { try Sync3Wire.fail("invalid_tool_result") }
                _ = try Sync3Wire.string(record["summary"])
            }
            tools[id] = record
        case "inputs":
            try Sync3Wire.shape(record, ["runId", "prompt"])
            _ = try Sync3Wire.identifier(record["runId"]); _ = try Sync3Wire.string(record["prompt"])
            inputs[id] = record
        default: try Sync3Wire.fail("invalid_entity_kind")
        }
    }
    private func live(_ run: String) throws {
        guard runs[run]?["outcome"] == .null else { try Sync3Wire.fail("run_not_live") }
    }
    mutating func apply(_ op: Sync3Operation, owner: String, ownerEpoch: Int64) throws {
        try op.validate()
        guard op.ownerEpoch == ownerEpoch else { try Sync3Wire.fail("stale_owner_epoch") }
        let e = op.event, type = e["type"]!.stringValue!
        if type != "commandQueued", op.actor != owner { try Sync3Wire.fail("not_owner") }
        switch type {
        case "commandQueued":
            let id = e["commandId"]!.stringValue!
            guard commands[id] == nil else { try Sync3Wire.fail("command_exists") }
            commands[id] = ["command": e["command"]!, "actor": .string(op.actor), "runId": .null]
        case "commandAccepted":
            let id = e["commandId"]!.stringValue!
            guard var cmd = commands[id] else { try Sync3Wire.fail("unknown_command") }
            guard cmd["runId"] == .null else { try Sync3Wire.fail("command_already_accepted") }
            cmd["runId"] = e["runId"]; commands[id] = cmd
        case "runStarted":
            let run = e["runId"]!.stringValue!
            guard runs[run] == nil else { try Sync3Wire.fail("run_exists") }
            guard commands.values.contains(where: { $0["runId"] == e["runId"] }) else { try Sync3Wire.fail("run_not_accepted") }
            runs[run] = ["outcome": .null]
        case "messageCreated":
            let id = e["messageId"]!.stringValue!, run = e["runId"]!.stringValue!
            try live(run)
            guard messages[id] == nil else { try Sync3Wire.fail("message_exists") }
            messages[id] = ["runId": .string(run), "role": e["role"]!, "text": .string("")]
        case "textAppended":
            let id = e["messageId"]!.stringValue!
            guard var msg = messages[id], let text = msg["text"]?.stringValue else { try Sync3Wire.fail("unknown_message") }
            try live(msg["runId"]!.stringValue!)
            guard Int64(text.utf8.count) == (try Sync3Wire.integer(e["offset"])) else { try Sync3Wire.fail("text_offset_mismatch") }
            msg["text"] = .string(text + e["text"]!.stringValue!); messages[id] = msg
        case "toolStarted":
            let id = e["toolId"]!.stringValue!
            try live(e["runId"]!.stringValue!)
            guard tools[id] == nil else { try Sync3Wire.fail("tool_exists") }
            tools[id] = ["runId": e["runId"]!, "name": e["name"]!, "failed": .null, "summary": .null]
        case "toolFinished":
            let id = e["toolId"]!.stringValue!
            guard var tool = tools[id] else { try Sync3Wire.fail("unknown_tool") }
            try live(tool["runId"]!.stringValue!)
            guard tool["failed"] == .null else { try Sync3Wire.fail("tool_finished") }
            tool["failed"] = e["failed"]; tool["summary"] = e["summary"]; tools[id] = tool
        case "inputRequested":
            let id = e["requestId"]!.stringValue!
            try live(e["runId"]!.stringValue!)
            guard inputs[id] == nil else { try Sync3Wire.fail("input_exists") }
            inputs[id] = ["runId": e["runId"]!, "prompt": e["prompt"]!]
        case "runFinished":
            let run = e["runId"]!.stringValue!
            try live(run); runs[run] = ["outcome": e["outcome"]!]
        default: try Sync3Wire.fail("invalid_event")
        }
    }
}

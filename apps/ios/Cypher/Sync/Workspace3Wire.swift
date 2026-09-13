import Foundation

struct Workspace3Scope: Codable, Equatable, Sendable {
    let endpoint: String
    let org: String
    let user: String
    let actor: String

    func validate() throws {
        guard Workspace3Wire.id(org), Workspace3Wire.id(actor),
              !user.isEmpty, user.utf8.count <= 256,
              !user.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else {
            try Workspace3Wire.fail("invalid_workspace_scope")
        }
        if endpoint != "local" {
            guard let url = URLComponents(string: endpoint), ["http", "https"].contains(url.scheme),
                  url.host != nil, url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
                  !endpoint.contains(where: \.isWhitespace) else { try Workspace3Wire.fail("invalid_workspace_scope") }
        }
    }
}

struct Workspace3Page: Codable {
    let through: UInt64
    let next: UInt64
    let done: Bool
    let rows: [Workspace3Row]
    func validate(after: UInt64) throws {
        guard through <= Workspace3Wire.maxSafe, after <= next, next <= through,
              done == (next == through), rows.count <= 32,
              try Workspace3Wire.data(self).count <= Workspace3Wire.frameBytes - 512 else {
            try Workspace3Wire.fail("invalid_workspace_page")
        }
        var previous = after, seen = Set<String>()
        for row in rows {
            try Workspace3Wire.row(row)
            guard row.seq > previous, row.seq <= next, seen.insert("\(row.kind):\(row.id)").inserted else {
                try Workspace3Wire.fail("invalid_workspace_page")
            }
            previous = row.seq
        }
        guard (rows.last?.seq ?? after) == next, next != after || done else { try Workspace3Wire.fail("invalid_workspace_page") }
    }
}

enum Workspace3Wire {
    static let maxSafe: UInt64 = 9_007_199_254_740_991
    static let frameBytes = 256 * 1024
    static let rowBytes = 64 * 1024
    static func fail(_ code: String) throws -> Never { throw Sync3Error.protocolError(code) }
    static func shape(_ object: [String: JSONValue], _ required: Set<String>, optional: Set<String> = []) throws {
        let keys = Set(object.keys)
        guard required.isSubset(of: keys), keys.isSubset(of: required.union(optional)) else { try fail("invalid_shape") }
    }
    static func data<T: Encodable>(_ value: T) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(value)
    }
    static func id(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 128 && value.utf8.allSatisfy { asciiAlpha($0) || (48...57).contains($0) || $0 == 95 || $0 == 45 }
    }
    static func rowID(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 256 && value.utf8.allSatisfy {
            asciiAlpha($0) || (48...57).contains($0) || "_.:@/-".utf8.contains($0)
        }
    }
    static func asciiAlpha(_ c: UInt8) -> Bool { (65...90).contains(c) || (97...122).contains(c) }
    static func kind(_ value: String) -> Bool { ["devices", "spaces", "chats"].contains(value) }
    static func field(_ value: String) -> Bool {
        guard let first = value.utf8.first else { return false }
        return asciiAlpha(first) && value.utf8.count <= 64 &&
            value.utf8.allSatisfy { asciiAlpha($0) || (48...57).contains($0) } &&
            !["constructor", "prototype", "__proto__"].contains(value)
    }
    static func clock(_ value: String) throws -> (Int64, Int64) {
        let bytes = Array(value.utf8)
        guard bytes.count >= 22, bytes[13] == 45, bytes[20] == 45,
              bytes.prefix(13).allSatisfy({ (48...57).contains($0) }),
              bytes[14..<20].allSatisfy({ (48...57).contains($0) }),
              id(String(decoding: bytes[21...], as: UTF8.self)),
              let ms = Int64(String(decoding: bytes.prefix(13), as: UTF8.self)),
              let counter = Int64(String(decoding: bytes[14..<20], as: UTF8.self)) else { try fail("invalid_clock") }
        return (ms, counter)
    }
    static func value(_ value: JSONValue) throws {
        var remaining = 32768
        func visit(_ value: JSONValue, _ depth: Int) throws {
            remaining -= 1
            guard depth <= 32, remaining >= 0 else { try fail("json_too_complex") }
            switch value {
            case .int(let v):
                guard v >= -Int64(maxSafe), v <= Int64(maxSafe) else { try fail("invalid_number") }
            case .double(let v):
                guard v.isFinite, v.rounded(.towardZero) != v || abs(v) <= Double(maxSafe) else { try fail("invalid_number") }
            case .array(let array): for child in array { try visit(child, depth + 1) }
            case .object(let object):
                for (key, child) in object { try visit(.string(key), depth + 1); try visit(child, depth + 1) }
            default: break
            }
        }
        try visit(value, 0)
    }
    static func operation(_ op: Workspace3Op, actor: String, local: Bool = false) throws {
        _ = try clock(op.hlc)
        guard kind(op.kind), rowID(op.id), op.hlc.hasSuffix("-\(actor)"),
              (op.op == .delete) == (op.set == nil) else { try fail("invalid_workspace_operation") }
        if !local && op.kind == "devices" && op.op != .delete && op.id != actor {
            guard op.op == .update, op.set?.keys.allSatisfy({ $0 == "name" }) == true else { try fail("not_device_author") }
        }
        for (key, v) in op.set ?? [:] {
            guard field(key) else { try fail("invalid_field") }
            try value(v)
        }
        if let id = op.set?["id"], id != .string(op.id) { try fail("row_identity_mismatch") }
        guard try data(op).count <= 16 * 1024 else { try fail("operation_too_large") }
    }
    static func row(_ row: Workspace3Row, optimistic: Bool = false) throws {
        guard kind(row.kind), rowID(row.id), optimistic || row.seq > 0, row.seq <= maxSafe,
              try data(row).count <= rowBytes else { try fail("invalid_workspace_row") }
        if let id = row.fields["id"], id != .string(row.id) { try fail("row_identity_mismatch") }
        for (key, v) in row.fields {
            guard field(key), row.clocks[key] != nil else { try fail("invalid_workspace_row") }
            try value(v)
        }
        for (key, c) in row.clocks {
            guard field(key) else { try fail("invalid_field") }; _ = try clock(c)
        }
        if let c = row.delHlc { _ = try clock(c) }
        guard !row.deleted || (row.delHlc != nil && row.fields.isEmpty && row.clocks.isEmpty) else { try fail("invalid_tombstone") }
    }
    static func rows(_ value: JSONValue?) throws -> [Workspace3Row] {
        guard case .array(let values) = value, values.count <= 32 else { try fail("invalid_workspace_rows") }
        return try values.map {
            guard case .object(let object) = $0 else { try fail("invalid_workspace_row") }
            try shape(object, ["kind", "id", "seq", "deleted", "fields", "clocks"], optional: ["delHlc"])
            let row = try JSONDecoder().decode(Workspace3Row.self, from: data($0))
            try self.row(row)
            return row
        }
    }
    static func decode(_ text: String) throws -> [String: JSONValue] {
        guard text.utf8.count <= frameBytes else { try fail("frame_too_large") }
        let object = try JSONDecoder().decode([String: JSONValue].self, from: Data(text.utf8))
        try value(.object(object))
        guard object["version"] == .int(3), let type = object["type"]?.stringValue else { try fail("upgrade_required") }
        let required: Set<String>
        var optional: Set<String> = []
        switch type {
        case "welcome": required = ["user", "org", "connection", "leaseMs", "through", "next", "done", "rows"]
        case "page": required = ["through", "next", "done", "rows"]
        case "pushed": required = ["id", "requestHash", "through", "rows"]
        case "changed": required = ["through"]
        case "probeOk": required = ["id", "through"]
        case "demand", "watching": required = ["chats"]
        case "presence": required = ["actor", "role", "connection", "expiresAt", "state"]
        case "peerClosed": required = ["actor", "connection"]
        case "routed": required = ["id", "token", "window"]
        case "call": required = ["token", "from", "method", "params", "window"]; optional = ["input"]
        case "reply": required = ["id", "sequence", "done", "value"]
        case "credit": required = ["token", "through"]
        case "input": required = ["token", "sequence", "done", "value"]
        case "inputCredit": required = ["id", "token", "through"]
        case "cancel": required = ["token"]
        case "error": required = ["code"]; optional = ["id", "token"]
        default: try fail("unknown_message")
        }
        try shape(object, required.union(["version", "type"]), optional: optional)
        for key in ["id", "actor", "from", "connection", "org"] where object[key] != nil {
            guard object[key]?.stringValue.map(id) == true else { try fail("invalid_identity") }
        }
        for key in ["through", "next", "sequence", "expiresAt", "leaseMs"] where object[key] != nil { _ = try Sync3Wire.integer(object[key]) }
        if let token = object["token"] {
            guard let t = token.stringValue, !t.isEmpty, t.utf8.count <= 1024 else { try fail("invalid_route") }
        }
        if let window = object["window"], window != .int(2) { try fail("invalid_credit_window") }
        for key in ["done", "input"] where object[key] != nil {
            guard object[key]?.boolValue != nil else { try fail("invalid_workspace_frame") }
        }
        if let role = object["role"], ![JSONValue.string("host"), .string("viewer")].contains(role) { try fail("invalid_role") }
        if let chats = object["chats"] {
            guard case .array(let ids) = chats, ids.count <= (type == "watching" ? 8 : 512),
                  ids.allSatisfy({ $0.stringValue.map(id) == true }) else { try fail("invalid_topics") }
        }
        for key in ["params", "value", "state"] {
            if let v = object[key], try data(v).count > 64 * 1024 { try fail("rpc_value_too_large") }
        }
        if type == "presence", object["state"]?.objectValue == nil { try fail("invalid_presence") }
        if type == "call" {
            guard let method = object["method"]?.stringValue, let first = method.utf8.first,
                  asciiAlpha(first), method.utf8.count <= 96,
                  method.utf8.allSatisfy({ asciiAlpha($0) || (48...57).contains($0) }) else { try fail("invalid_method") }
        }
        if type == "error" {
            guard let code = object["code"]?.stringValue, !code.isEmpty, code.utf8.count <= 128 else { try fail("invalid_error") }
        }
        return object
    }
}

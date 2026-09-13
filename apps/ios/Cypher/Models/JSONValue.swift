import Foundation

/// Lossless explicit null versus absent fields. Used by native wire codecs,
/// SQLite bodies and application RPC; independent of any replication model.
enum JSONValue: Hashable, Sendable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])
}

extension JSONValue: Codable {
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let i = try? c.decode(Int64.self) { self = .int(i) }
        else if let d = try? c.decode(Double.self) { self = .double(d) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else if let o = try? c.decode([String: JSONValue].self) { self = .object(o) }
        else { throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a JSON value") }
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let v): try c.encode(v)
        case .int(let v): try c.encode(v)
        case .double(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .object(let v): try c.encode(v)
        }
    }
}
extension JSONValue {
    var stringValue: String? {
        if case .string(let v) = self { return v }
        return nil
    }
    var boolValue: Bool? {
        if case .bool(let v) = self { return v }
        return nil
    }
    var int64Value: Int64? {
        switch self {
        case .int(let v): return v
        case .double(let v):
            guard v.isFinite, v >= Double(Int64.min), v < Double(Int64.max) else { return nil }
            return Int64(v)
        default: return nil
        }
    }
    var objectValue: [String: JSONValue]? {
        if case .object(let v) = self { return v }
        return nil
    }
    var isNull: Bool { self == .null }
    init?<T: Encodable>(encodable: T) {
        guard let data = try? JSONEncoder().encode(encodable),
              let value = try? JSONDecoder().decode(JSONValue.self, from: data) else { return nil }
        self = value
    }
}

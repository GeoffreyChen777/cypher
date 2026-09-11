import Foundation

/// Closed shape descriptor shared with Rust/Edge, not a general JSON Schema interpreter.
enum Sync3CommandSchema {
    private static let schema: JSONValue? = {
        guard let url = Bundle.main.url(forResource: "Sync3CommandSchema", withExtension: "json"),
              let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(JSONValue.self, from: data)
    }()
    static func validate(_ value: JSONValue) throws {
        try Sync3Wire.validateJSON(value, depth: 3)
        guard let schema else { try Sync3Wire.fail("missing_command_schema") }
        guard matches(value, schema) else { try Sync3Wire.fail("invalid_command") }
    }
    private static func matches(_ value: JSONValue, _ schema: JSONValue) -> Bool {
        if let kind = schema.stringValue {
            switch kind {
            case "id": return (try? Sync3Wire.identifier(value)) != nil
            case "entityId": return (try? Sync3Wire.entityIdentifier(value)) != nil
            case "string": return value.stringValue != nil
            case "nonemptyString": return value.stringValue.map { !$0.isEmpty } ?? false
            case "uint": return (try? Sync3Wire.integer(value)) != nil
            case "bool": return value.boolValue != nil
            case "null": return value == .null
            case "json": return true // Operation validation bounds the entire JSON tree.
            default: return false
            }
        }
        guard let s = schema.objectValue else { return false }
        if let inner = s["nullable"] { return value == .null || matches(value, inner) }
        if case .array(let variants) = s["oneOf"] { return variants.filter { matches(value, $0) }.count == 1 }
        if case .array(let variants) = s["enum"] { return variants.contains(value) }
        let min = (try? Sync3Wire.integer(s["min"])) ?? 0
        let max = (try? Sync3Wire.integer(s["max"])) ?? 256
        if let inner = s["array"] {
            guard case .array(let values) = value else { return false }
            return values.count >= min && values.count <= max && values.allSatisfy { matches($0, inner) }
        }
        if let inner = s["map"] {
            guard let values = value.objectValue else { return false }
            return values.count <= max && values.values.allSatisfy { matches($0, inner) }
        }
        if let fields = s["object"]?.objectValue {
            guard let values = value.objectValue else { return false }
            let optional: [JSONValue]
            if case .array(let list) = s["optional"] { optional = list } else { optional = [] }
            return Set(values.keys).isSubset(of: Set(fields.keys)) && fields.allSatisfy { key, rule in
                if let child = values[key] { return matches(child, rule) }
                return optional.contains(.string(key))
            }
        }
        return false
    }
}

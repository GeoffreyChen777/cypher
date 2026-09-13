// Shared with crates/rpc/src/workspace3/codec.rs. Byte chunks, not text
// chunks: even a split Unicode scalar cannot corrupt the logical JSON value.
import Foundation
import CoreFoundation

enum Workspace3RPCCodec {
    static let maximumBytes = 8 * 1024 * 1024
    static let chunkBytes = 32 * 1024
    static let name = "json-base64-v3"
    static func foundation(_ object: Any) throws -> JSONValue {
        var bytes = maximumBytes, nodes = 32768
        func convert(_ object: Any, _ depth: Int) throws -> JSONValue {
            nodes -= 1
            guard depth <= 64, nodes >= 0 else { try Workspace3Wire.fail("rpc_payload_too_complex") }
            if object is NSNull { return .null }
            if let string = object as? String {
                guard string.utf8.count <= bytes else { try Workspace3Wire.fail("rpc_payload_too_large") }
                bytes -= string.utf8.count
                return .string(string)
            }
            if let number = object as? NSNumber {
                if CFGetTypeID(number) == CFBooleanGetTypeID() { return .bool(number.boolValue) }
                let value = number.doubleValue
                guard value.isFinite else { try Workspace3Wire.fail("invalid_number") }
                if value.rounded(.towardZero) == value, value >= Double(Int64.min), value < Double(Int64.max) {
                    return .int(number.int64Value)
                }
                return .double(value)
            }
            if let array = object as? [Any] { return .array(try array.map { try convert($0, depth + 1) }) }
            if let dict = object as? [String: Any] {
                var result: [String: JSONValue] = [:]
                for (key, value) in dict { _ = try convert(key, depth + 1); result[key] = try convert(value, depth + 1) }
                return .object(result)
            }
            try Workspace3Wire.fail("invalid_rpc_value")
        }
        return try convert(object, 0)
    }

    struct Encoder {
        private let bytes: Data
        private var offset = 0
        init(_ value: JSONValue) throws {
            // Refuse excess input before JSONEncoder duplicates a huge tree.
            var budget = maximumBytes
            try Workspace3RPCCodec.budget(value, remaining: &budget, depth: 0)
            bytes = try Workspace3Wire.data(value)
            guard bytes.count <= maximumBytes else { try Workspace3Wire.fail("rpc_payload_too_large") }
        }
        var complete: Bool { offset == bytes.count }
        mutating func next() -> JSONValue? {
            guard !complete else { return nil }
            let next = min(offset + chunkBytes, bytes.count)
            let value: JSONValue = .object([
                "codec": .string(name), "length": .int(Int64(bytes.count)), "offset": .int(Int64(offset)),
                "end": .bool(next == bytes.count), "data": .string(bytes[offset..<next].base64EncodedString())
            ])
            offset = next
            return value
        }
    }
    struct Decoder {
        private var bytes = Data()
        private var length: Int?
        private var poisoned = false
        var incomplete: Bool { length != nil }
        mutating func push(_ value: JSONValue) throws -> JSONValue? {
            guard !poisoned else { try Workspace3Wire.fail("invalid_rpc_payload") }
            poisoned = true
            do {
                let result = try consume(value)
                poisoned = false
                return result
            } catch {
                bytes = Data()
                throw error
            }
        }
        private mutating func consume(_ value: JSONValue) throws -> JSONValue? {
            guard let p = value.objectValue else { try Workspace3Wire.fail("invalid_rpc_payload") }
            try Workspace3Wire.shape(p, ["codec", "length", "offset", "end", "data"])
            let count = try Sync3Wire.integer(p["length"]), offset = try Sync3Wire.integer(p["offset"])
            guard p["codec"] == .string(name), count > 0, count <= maximumBytes, offset == bytes.count, offset < count,
                  length == nil || length == Int(count), let end = p["end"]?.boolValue,
                  let encoded = p["data"]?.stringValue, encoded.utf8.count <= ((chunkBytes + 2) / 3) * 4,
                  let data = Data(base64Encoded: encoded), data.base64EncodedString() == encoded,
                  !data.isEmpty, data.count <= chunkBytes, data.count <= count - offset,
                  end == (offset + Int64(data.count) == count), end || data.count == chunkBytes else {
                try Workspace3Wire.fail("invalid_rpc_payload")
            }
            length = Int(count)
            bytes.append(data)
            guard end else { return nil }
            let value = try JSONDecoder().decode(JSONValue.self, from: bytes)
            bytes = Data(); length = nil
            return value
        }
    }
    private static func budget(_ value: JSONValue, remaining: inout Int, depth: Int) throws {
        guard depth <= 64 else { try Workspace3Wire.fail("rpc_payload_too_complex") }
        func spend(_ n: Int) throws {
            guard n <= remaining else { try Workspace3Wire.fail("rpc_payload_too_large") }
            remaining -= n
        }
        switch value {
        case .string(let text):
            try spend(2)
            for scalar in text.unicodeScalars {
                switch scalar.value {
                case 8, 9, 10, 12, 13, 34, 92: try spend(2)
                case 0...31: try spend(6)
                default: try spend(scalar.utf8.count)
                }
            }
        case .array(let array):
            try spend(2 + max(0, array.count - 1))
            for child in array { try budget(child, remaining: &remaining, depth: depth + 1) }
        case .object(let object):
            try spend(2 + max(0, object.count - 1) + object.count)
            for (key, child) in object {
                try budget(.string(key), remaining: &remaining, depth: depth + 1)
                try budget(child, remaining: &remaining, depth: depth + 1)
            }
        default: try spend(try Workspace3Wire.data(value).count)
        }
    }
}

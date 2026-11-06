// What a durable command's signature covers: the Swift twin of
// crates/doc/src/command_auth.rs. The phone signs the commands it queues
// with its device-trust identity key, so a host that only accepts paired
// devices runs them. Shared vectors: protocol/vectors/command-signing-v1.json
// (scripts/tests/command-signing-vectors.swift).
//
// Bytes: "cypher-command-v1\0" ‖ str(chatId) ‖ value(entry without status,
// resolution and sig), values encoded from the Loro value tree with map keys
// sorted by their UTF-8 bytes.

import Foundation

/// A Loro value, without Loro (the vector check compiles this file alone).
indirect enum CommandValue: Equatable {
    case null
    case bool(Bool)
    case i64(Int64)
    case double(Double)
    case string(String)
    case binary(Data)
    case list([CommandValue])
    case map([String: CommandValue])
    /// A container reference: never in a written entry.
    case unsupported
}

enum CommandSigning {
    static let domain = Data("cypher-command-v1\u{0}".utf8)
    static let sigField = "sig"
    static let unsignedFields: Set<String> = ["status", "resolution", sigField]

    static func signingBytes(chatId: String, fields: [String: CommandValue]) -> Data {
        var out = domain
        putString(chatId, into: &out)
        put(.map(fields.filter { !unsignedFields.contains($0.key) }), into: &out)
        return out
    }

    private static func putLength(_ count: Int, into out: inout Data) {
        withUnsafeBytes(of: UInt32(count).bigEndian) { out.append(contentsOf: $0) }
    }

    private static func putString(_ value: String, into out: inout Data) {
        let utf8 = Data(value.utf8)
        putLength(utf8.count, into: &out)
        out.append(utf8)
    }

    private static func put(_ value: CommandValue, into out: inout Data) {
        switch value {
        case .null:
            out.append(0x00)
        case .bool(let b):
            out.append(contentsOf: [0x01, b ? 1 : 0])
        case .i64(let n):
            out.append(0x02)
            withUnsafeBytes(of: n.bigEndian) { out.append(contentsOf: $0) }
        case .double(let f):
            out.append(0x03)
            withUnsafeBytes(of: f.bitPattern.bigEndian) { out.append(contentsOf: $0) }
        case .string(let s):
            out.append(0x04)
            putString(s, into: &out)
        case .binary(let bytes):
            out.append(0x05)
            putLength(bytes.count, into: &out)
            out.append(bytes)
        case .list(let items):
            out.append(0x06)
            putLength(items.count, into: &out)
            for item in items { put(item, into: &out) }
        case .map(let map):
            out.append(0x07)
            putLength(map.count, into: &out)
            let keys = map.keys.sorted { Array($0.utf8).lexicographicallyPrecedes(Array($1.utf8)) }
            for key in keys {
                putString(key, into: &out)
                put(map[key]!, into: &out)
            }
        case .unsupported:
            out.append(0xff)
        }
    }
}

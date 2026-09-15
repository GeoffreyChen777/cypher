// Inactive P1 codec, not authorization. See docs/ephemeral-stream-v1.md.
// Deliberately not registered in ChatRoomClient.
import Foundation
import CoreFoundation

enum StreamPreviewWire {
    static let capability = "ephemeral-stream-v1"
    static let delta: UInt8 = 0x20
    static let snapshot: UInt8 = 0x21
    static let resume: UInt8 = 0x22
    static let finished: UInt8 = 0x23
    static let start: UInt8 = 0x24
    static let state: UInt8 = 0x25
    static let receipt: UInt8 = 0x26
    static let maxFrameBytes = 65_536
    static let maxTextBytes = 61_440

    private static func identifier(_ value: Any?) -> Bool {
        guard let value = value as? String, (1...128).contains(value.utf8.count) else { return false }
        return value.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0)
                || [46, 95, 58, 45].contains($0)
        }
    }

    private static func integer(_ value: Any?) -> UInt64? {
        guard let n = value as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        let d = n.doubleValue
        guard d.isFinite, d >= 0, d <= 9_007_199_254_740_991, d.rounded(.towardZero) == d else { return nil }
        return UInt64(d)
    }

    static func decode(_ data: Data) -> ChatWireFrame? {
        guard data.count >= 5, data.count <= maxFrameBytes else { return nil }
        let prefix = [UInt8](data.prefix(5))
        let length = Int(UInt32(prefix[1]) | UInt32(prefix[2]) << 8 | UInt32(prefix[3]) << 16 | UInt32(prefix[4]) << 24)
        guard length <= chatFrameMaxHeaderBytes, 5 + length <= data.count else { return nil }
        let rawHeader = data.subdata(in: 5..<(5 + length))
        // Foundation otherwise auto-detects UTF-16/32 and accepts a BOM, unlike
        // the UTF-8-only contract. Require an ordinary JSON object's first token.
        guard String(data: rawHeader, encoding: .utf8) != nil,
              rawHeader.first(where: { ![9, 10, 13, 32].contains($0) }) == 123,
              let frame = ChatWire.decode(data),
              [delta, snapshot, resume, finished, start, state, receipt].contains(frame.kind) else { return nil }
        if [start, state].contains(frame.kind) {
            let h = frame.header
            let keys = frame.kind == start ? ["chatId", "runId", "segmentId"]
                : h["mode"] as? String == "preview" ? ["chatId", "mode", "runId", "segmentId", "epoch"] : ["chatId", "mode"]
            guard frame.payload.isEmpty, h.count == keys.count, keys.allSatisfy({ h[$0] != nil }),
                  keys.filter({ $0 != "mode" }).allSatisfy({ identifier(h[$0]) }) else { return nil }
            if frame.kind == state && !["legacy", "ready", "preview"].contains(h["mode"] as? String ?? "") { return nil }
            return frame
        }
        var keys = ["chatId", "runId", "segmentId", "epoch", "revision", "baseSeq"]
        if frame.kind == delta { keys.append("prevRevision") }
        if frame.kind == finished { keys.append("batchId") }
        let h = frame.header
        guard h.count == keys.count, keys.allSatisfy({ h[$0] != nil }),
              keys.prefix(4).allSatisfy({ identifier(h[$0]) }),
              let revision = integer(h["revision"]), integer(h["baseSeq"]) != nil,
              frame.payload.count <= maxTextBytes,
              String(data: frame.payload, encoding: .utf8) != nil else { return nil }
        if frame.kind == delta {
            guard let previous = integer(h["prevRevision"]), previous + 1 == revision,
                  !frame.payload.isEmpty else { return nil }
        }
        if frame.kind == finished && !identifier(h["batchId"]) { return nil }
        if [resume, finished, receipt].contains(frame.kind) && !frame.payload.isEmpty { return nil }
        return frame
    }

    static func encode(_ kind: UInt8, header: [String: Any], text: String = "") -> Data? {
        guard text.utf8.count <= maxTextBytes, JSONSerialization.isValidJSONObject(header) else { return nil }
        let bytes = ChatWire.encode(kind, header: header, payload: Data(text.utf8))
        return decode(bytes) == nil ? nil : bytes
    }
}

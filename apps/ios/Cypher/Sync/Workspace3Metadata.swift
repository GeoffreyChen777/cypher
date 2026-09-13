import Foundation

// Native metadata values. No reseed/per-field operation clocks, legacy
// snapshots, pending batches or transport behavior live in this model.
private struct MetadataKey: CodingKey {
    let stringValue: String
    var intValue: Int? { nil }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}
private func metadataFields(_ decoder: Decoder, _ allowed: Set<String>) throws {
    let fields = try decoder.container(keyedBy: MetadataKey.self)
    guard fields.allKeys.allSatisfy({ allowed.contains($0.stringValue) }) else {
        throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "unknown_metadata_field"))
    }
}
struct Workspace3Row: Hashable, Codable, Sendable {
    var kind: String
    var id: String
    var seq: UInt64
    var deleted: Bool
    var delHlc: String?
    var fields: [String: JSONValue]
    var clocks: [String: String]
    enum CodingKeys: String, CodingKey { case kind, id, seq, deleted, delHlc, fields, clocks }
    init(kind: String, id: String, seq: UInt64, deleted: Bool, delHlc: String? = nil, fields: [String: JSONValue], clocks: [String: String]) {
        self.kind = kind; self.id = id; self.seq = seq; self.deleted = deleted
        self.delHlc = delHlc; self.fields = fields; self.clocks = clocks
    }
    init(from decoder: Decoder) throws {
        try metadataFields(decoder, ["kind", "id", "seq", "deleted", "delHlc", "fields", "clocks"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decode(String.self, forKey: .kind); id = try c.decode(String.self, forKey: .id)
        seq = try c.decode(UInt64.self, forKey: .seq); deleted = try c.decode(Bool.self, forKey: .deleted)
        delHlc = try c.decodeIfPresent(String.self, forKey: .delHlc)
        fields = try c.decode([String: JSONValue].self, forKey: .fields)
        clocks = try c.decode([String: String].self, forKey: .clocks)
    }
}
enum Workspace3OpType: String, Codable, Sendable { case upsert, update, delete }
struct Workspace3Op: Hashable, Codable, Sendable {
    var kind: String
    var id: String
    var op: Workspace3OpType
    var set: [String: JSONValue]?
    var hlc: String
    enum CodingKeys: String, CodingKey { case kind, id, op, set, hlc }
    init(kind: String, id: String, op: Workspace3OpType, set: [String: JSONValue]?, hlc: String) {
        self.kind = kind; self.id = id; self.op = op; self.set = set; self.hlc = hlc
    }
    init(from decoder: Decoder) throws {
        try metadataFields(decoder, ["kind", "id", "op", "set", "hlc"])
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decode(String.self, forKey: .kind); id = try c.decode(String.self, forKey: .id)
        op = try c.decode(Workspace3OpType.self, forKey: .op)
        set = try c.decodeIfPresent([String: JSONValue].self, forKey: .set)
        hlc = try c.decode(String.self, forKey: .hlc)
    }
}
enum Workspace3Metadata {
    static func clock(ms: Int64, counter: UInt32, actor: String) -> String {
        func pad(_ value: String, _ width: Int) -> String {
            String(repeating: "0", count: max(0, width - value.count)) + value
        }
        return "\(pad(String(ms),13))-\(pad(String(counter),6))-\(actor)"
    }
    static func apply(_ row: Workspace3Row?, _ op: Workspace3Op) -> (row: Workspace3Row?, changed: Bool) {
        func newer(_ value: String, _ previous: String?) -> Bool { previous.map { value > $0 } ?? true }
        if op.op == .delete {
            let previous = row?.deleted == true ? row?.delHlc :
                (Array((row?.clocks ?? [:]).values) + (row?.delHlc.map { [$0] } ?? [])).max()
            if row != nil && !newer(op.hlc, previous) { return (row, false) }
            return (Workspace3Row(kind: op.kind, id: op.id, seq: row?.seq ?? 0, deleted: true,
                                  delHlc: op.hlc, fields: [:], clocks: [:]), true)
        }
        if (row == nil && op.op == .update) ||
            (row?.deleted == true && (op.op == .update || !newer(op.hlc, row?.delHlc))) { return (row, false) }
        var base = row?.deleted == false ? row! :
            Workspace3Row(kind: op.kind, id: op.id, seq: row?.seq ?? 0, deleted: false,
                          delHlc: row?.delHlc, fields: [:], clocks: [:])
        var changed = row == nil || row?.deleted == true
        for (key, value) in op.set ?? [:] where newer(op.hlc, base.clocks[key]) {
            if value == .null { base.fields.removeValue(forKey: key) }
            else { base.fields[key] = value }
            base.clocks[key] = op.hlc
            changed = true
        }
        return (base, changed)
    }
}

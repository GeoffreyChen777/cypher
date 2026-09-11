// v3 durable outbox + applied projection. ACK never skips an unread event.
// Uses the same SQLite schema as the Rust client; not the legacy DocDisk LRU.
import Foundation
import SQLite3

@MainActor
final class Sync3Journal {
    private var db: OpaquePointer?
    let actor: String
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    init(url: URL, account: String, room: String, actor: String) throws {
        self.actor = actor
        _ = try Sync3Wire.identifier(.string(actor))
        guard !account.isEmpty, !room.isEmpty else { try Sync3Wire.fail("invalid_identity") }
        guard sqlite3_open(url.path, &db) == SQLITE_OK else {
            if let db { sqlite3_close(db) }; db = nil
            try Sync3Wire.fail("storage_open")
        }
        do {
            sqlite3_busy_timeout(db, 5_000)
            let format = try query("PRAGMA user_version").first?[0]
            let prototype = try query("SELECT name FROM sqlite_master WHERE type='table' AND name='sync3_projection'")
            guard prototype.isEmpty, format == "0" || format == "4" else { try Sync3Wire.fail("unsupported_journal_format") }
            for path in [url.path, url.path + "-wal", url.path + "-shm"] where FileManager.default.fileExists(atPath: path) {
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path)
            }
            try execute("PRAGMA journal_mode=WAL")
            try execute("PRAGMA synchronous=FULL")
            try execute("""
                CREATE TABLE IF NOT EXISTS sync3_meta(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                account TEXT NOT NULL,room TEXT NOT NULL,actor TEXT NOT NULL,
                epoch INTEGER NOT NULL DEFAULT 0,owner TEXT NOT NULL DEFAULT '',
                owner_epoch INTEGER NOT NULL DEFAULT 0,cursor INTEGER NOT NULL DEFAULT 0)
                """)
            try execute("""
                CREATE TABLE IF NOT EXISTS sync3_outbox(
                ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
                operation TEXT NOT NULL,acked_seq INTEGER)
                """)
            try execute("CREATE TABLE IF NOT EXISTS sync3_events(seq INTEGER PRIMARY KEY,id TEXT UNIQUE NOT NULL,operation TEXT NOT NULL)")
            try execute("""
                CREATE TABLE IF NOT EXISTS sync3_entities(
                kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,run_id TEXT,seq INTEGER NOT NULL,
                PRIMARY KEY(kind,id))
                """)
            try execute("CREATE INDEX IF NOT EXISTS sync3_entity_run ON sync3_entities(kind,run_id)")
            try execute("CREATE INDEX IF NOT EXISTS sync3_entity_seq ON sync3_entities(seq)")
            try transaction {
                try execute("INSERT OR IGNORE INTO sync3_meta(singleton,account,room,actor) VALUES(1,?,?,?)", [account, room, actor])
                let identity = try query("SELECT account,room,actor FROM sync3_meta WHERE singleton=1")
                guard identity.first == [account, room, actor] else { try Sync3Wire.fail("scope_mismatch") }
                try execute("PRAGMA user_version=4")
            }
        } catch {
            sqlite3_close(db); db = nil; throw error
        }
    }
    deinit { if let db { sqlite3_close(db) } }

    var cursor: Int64 { get throws { try number("cursor") } }
    var epoch: Int64 { get throws { try number("epoch") } }
    var projection: Sync3Projection {
        get throws {
            var projection = Sync3Projection()
            for row in try query("SELECT kind,id,body FROM sync3_entities ORDER BY kind,id") {
                let record = try JSONDecoder().decode([String: JSONValue].self, from: Data(row[2]!.utf8))
                try projection.install(kind: row[0]!, id: row[1]!, record: record)
            }
            return projection
        }
    }
    func hello() throws -> [String: JSONValue] {
        ["type": .string("hello"), "version": .int(3), "actor": .string(actor),
         "epoch": .int(try epoch), "after": .int(try cursor)]
    }
    @discardableResult
    func acceptState(_ state: [String: JSONValue]) throws -> Int64 {
        try Sync3Wire.shape(state, ["type", "version", "epoch", "owner", "ownerEpoch", "head"])
        guard state["type"] == .string("state"), state["version"] == .int(3) else { try Sync3Wire.fail("invalid_state") }
        let newEpoch = try Sync3Wire.integer(state["epoch"]), ownerEpoch = try Sync3Wire.integer(state["ownerEpoch"])
        let head = try Sync3Wire.integer(state["head"]), owner = try Sync3Wire.identifier(state["owner"])
        guard newEpoch > 0, ownerEpoch > 0 else { try Sync3Wire.fail("invalid_state") }
        try transaction {
            let currentEpoch = try epoch
            guard currentEpoch == 0 || currentEpoch == newEpoch else { try Sync3Wire.fail("epoch_mismatch") }
            guard head >= (try cursor) else { try Sync3Wire.fail("server_behind") }
            let oldOwnerEpoch = try number("owner_epoch")
            guard ownerEpoch >= oldOwnerEpoch else { try Sync3Wire.fail("owner_epoch_regressed") }
            if currentEpoch != 0, ownerEpoch == oldOwnerEpoch,
               try query("SELECT owner FROM sync3_meta WHERE singleton=1").first?[0] != owner {
                try Sync3Wire.fail("owner_changed_without_epoch")
            }
            try execute("UPDATE sync3_meta SET epoch=?,owner=?,owner_epoch=? WHERE singleton=1", [String(newEpoch), owner, String(ownerEpoch)])
        }
        return head
    }
    func enqueue(_ operation: Sync3Operation) throws {
        try operation.validate()
        guard operation.actor == actor else { try Sync3Wire.fail("actor_mismatch") }
        try transaction {
            let pending = try query("SELECT operation FROM sync3_outbox WHERE id=?", [operation.id])
            let committed = try query("SELECT operation FROM sync3_events WHERE id=?", [operation.id])
            if let body = (pending.first ?? committed.first)?.first ?? nil {
                guard try decodeOperation(body) == operation else { try Sync3Wire.fail("operation_id_conflict") }
                return
            }
            try execute("INSERT INTO sync3_outbox(id,operation) VALUES(?,?)", [operation.id, try json(operation)])
        }
    }
    func pending() throws -> [Sync3Operation] {
        var result: [Sync3Operation] = []
        var bytes = try Sync3Wire.encode(["type": JSONValue.string("push"), "version": .int(3),
                                         "operations": .array([])]).count
        for row in try query("SELECT operation FROM sync3_outbox WHERE acked_seq IS NULL ORDER BY ordinal LIMIT 64") {
            let operation = try decodeOperation(row[0]!)
            let count = try Sync3Wire.encode(operation).count + (result.isEmpty ? 0 : 1)
            if bytes + count > Sync3Wire.maxFrameBytes { break }
            bytes += count
            result.append(operation)
        }
        return result
    }
    func acknowledge(_ ack: [String: JSONValue]) throws {
        try Sync3Wire.shape(ack, ["type", "version", "epoch", "receipts"])
        guard ack["type"] == .string("ack"), ack["version"] == .int(3),
              ack["epoch"] == .int(try epoch), (try epoch) > 0,
              case .array(let receipts) = ack["receipts"], receipts.count <= 64 else { try Sync3Wire.fail("invalid_ack") }
        try transaction {
            for receipt in receipts {
                guard case .object(let r) = receipt else { try Sync3Wire.fail("invalid_receipt") }
                try Sync3Wire.shape(r, ["id", "seq"])
                let id = try Sync3Wire.identifier(r["id"]), seq = try Sync3Wire.integer(r["seq"])
                guard seq > 0 else { try Sync3Wire.fail("invalid_receipt") }
                let old = try query("SELECT acked_seq FROM sync3_outbox WHERE id=?", [id])
                if let row = old.first {
                    if let oldSeq = row[0], oldSeq != String(seq) { try Sync3Wire.fail("receipt_conflict") }
                    try execute("UPDATE sync3_outbox SET acked_seq=? WHERE id=?", [String(seq), id])
                } else {
                    let applied = try query("SELECT seq FROM sync3_events WHERE id=?", [id])
                    guard applied.first?[0] == String(seq) else { try Sync3Wire.fail("unknown_receipt") }
                }
            }
        }
    }
    func applyPage(_ page: [String: JSONValue]) throws {
        try Sync3Wire.shape(page, ["type", "version", "epoch", "through", "next", "rows", "done"])
        guard page["type"] == .string("page"), page["version"] == .int(3),
              page["epoch"] == .int(try epoch), (try epoch) > 0,
              case .array(let values) = page["rows"], values.count <= 64,
              let done = page["done"]?.boolValue else { try Sync3Wire.fail("invalid_page") }
        let through = try Sync3Wire.integer(page["through"]), next = try Sync3Wire.integer(page["next"])
        guard next <= through, done == (next == through) else { try Sync3Wire.fail("invalid_page") }
        _ = try Sync3Wire.encode(page)
        let rows = try values.map { value -> Sync3Row in
            guard let object = value.objectValue else { try Sync3Wire.fail("invalid_row") }
            try Sync3Wire.shape(object, ["seq", "operation"])
            return try JSONDecoder().decode(Sync3Row.self, from: Sync3Wire.encode(value))
        }
        try transaction {
            var position = try cursor
            guard next >= position else { try Sync3Wire.fail("stale_page") }
            var previous: Int64?
            for row in rows {
                guard row.seq > 0, row.seq <= next,
                      previous == nil || row.seq == previous! + 1 else { try Sync3Wire.fail("cursor_gap") }
                previous = row.seq
                if row.seq <= position {
                    let existing = try query("SELECT operation FROM sync3_events WHERE seq=?", [String(row.seq)])
                    guard let body = existing.first?[0], try decodeOperation(body) == row.operation else {
                        try Sync3Wire.fail("history_conflict")
                    }
                    continue
                }
                guard row.seq == position + 1 else { try Sync3Wire.fail("cursor_gap") }
                try applyOperation(row.operation, seq: row.seq)
                if let pending = try query("SELECT operation,acked_seq FROM sync3_outbox WHERE id=?", [row.operation.id]).first {
                    guard try decodeOperation(pending[0]!) == row.operation,
                          pending[1] == nil || pending[1] == String(row.seq) else { try Sync3Wire.fail("receipt_conflict") }
                    try execute("DELETE FROM sync3_outbox WHERE id=?", [row.operation.id])
                }
                try execute("INSERT INTO sync3_events VALUES(?,?,?)", [String(row.seq), row.operation.id, try json(row.operation)])
                position = row.seq
            }
            guard position == next, !rows.isEmpty || done else { try Sync3Wire.fail("cursor_gap") }
            try execute("UPDATE sync3_meta SET cursor=? WHERE singleton=1", [String(position)])
        }
    }
    /// Load only the mutated entity and its reducer dependencies. This runs
    /// inside applyPage's transaction, never writes a whole-chat JSON blob.
    private func applyOperation(_ operation: Sync3Operation, seq: Int64) throws {
        try operation.validate()
        var projection = Sync3Projection()
        func load(_ kind: String, _ id: String) throws {
            guard let row = try query("SELECT body FROM sync3_entities WHERE kind=? AND id=?", [kind, id]).first else { return }
            let record = try JSONDecoder().decode([String: JSONValue].self, from: Data(row[0]!.utf8))
            try projection.install(kind: kind, id: id, record: record)
        }
        let e = operation.event
        switch e["type"]!.stringValue! {
        case "commandQueued", "commandAccepted", "commandResolved", "commandCancelled": try load("commands", e["commandId"]!.stringValue!)
        case "runStarted":
            let run = e["runId"]!.stringValue!
            try load("runs", run)
            if let row = try query("SELECT id FROM sync3_entities WHERE kind='commands' AND run_id=? AND json_extract(body,'$.command.status') IN ('pending','applied') LIMIT 1", [run]).first {
                try load("commands", row[0]!)
            }
        case "runFinished": try load("runs", e["runId"]!.stringValue!)
        case "messageCreated":
            try load("messages", e["messageId"]!.stringValue!); try load("runs", e["runId"]!.stringValue!)
        case "textAppended":
            let id = e["messageId"]!.stringValue!
            try load("messages", id)
            if let run = projection.messages[id]?["runId"]?.stringValue { try load("runs", run) }
        case "toolStarted":
            try load("tools", e["toolId"]!.stringValue!); try load("runs", e["runId"]!.stringValue!)
        case "toolFinished":
            let id = e["toolId"]!.stringValue!
            try load("tools", id)
            if let run = projection.tools[id]?["runId"]?.stringValue { try load("runs", run) }
        case "inputRequested":
            try load("inputs", e["requestId"]!.stringValue!); try load("runs", e["runId"]!.stringValue!)
        default: try Sync3Wire.fail("invalid_event")
        }
        let before = projection.tables
        // The authenticated server authorized historical epochs at commit.
        try projection.apply(operation, owner: operation.actor, ownerEpoch: operation.ownerEpoch)
        for (kind, records) in projection.tables {
            for (id, record) in records where before[kind]?[id] != record {
                let body = try json(record)
                guard body.utf8.count <= Sync3Wire.maxFrameBytes * 4 else { try Sync3Wire.fail("entity_too_large") }
                try execute("""
                    INSERT INTO sync3_entities(kind,id,body,run_id,seq) VALUES(?,?,?,?,?)
                    ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,run_id=excluded.run_id,seq=excluded.seq
                    """, [kind, id, body, record["runId"]?.stringValue, String(seq)])
            }
        }
    }
    private func number(_ column: String) throws -> Int64 {
        // Internal static column names only; never accept a wire field here.
        guard ["cursor", "epoch", "owner_epoch"].contains(column),
              let row = try query("SELECT \(column) FROM sync3_meta WHERE singleton=1").first,
              let raw = row[0], let n = Int64(raw) else { try Sync3Wire.fail("storage_read") }
        return n
    }
    private func json<T: Encodable>(_ value: T) throws -> String {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return String(decoding: try encoder.encode(value), as: UTF8.self)
    }
    private func decodeOperation(_ text: String) throws -> Sync3Operation {
        try JSONDecoder().decode(Sync3Operation.self, from: Data(text.utf8))
    }
    private func transaction(_ body: () throws -> Void) throws {
        try execute("BEGIN IMMEDIATE")
        do { try body(); try execute("COMMIT") }
        catch { try? execute("ROLLBACK"); throw error }
    }
    private func execute(_ sql: String, _ params: [String?] = []) throws {
        _ = try query(sql, params)
    }
    private func query(_ sql: String, _ params: [String?] = []) throws -> [[String?]] {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else { try Sync3Wire.fail("storage_prepare") }
        defer { sqlite3_finalize(statement) }
        for (index, value) in params.enumerated() {
            let rc: Int32
            if let value { rc = sqlite3_bind_text(statement, Int32(index + 1), value, -1, transient) }
            else { rc = sqlite3_bind_null(statement, Int32(index + 1)) }
            guard rc == SQLITE_OK else { try Sync3Wire.fail("storage_bind") }
        }
        var result: [[String?]] = []
        while true {
            let rc = sqlite3_step(statement)
            if rc == SQLITE_DONE { return result }
            guard rc == SQLITE_ROW else { try Sync3Wire.fail("storage_write") }
            result.append((0..<sqlite3_column_count(statement)).map { column in
                guard let text = sqlite3_column_text(statement, column) else { return nil }
                return String(cString: text)
            })
        }
    }
}

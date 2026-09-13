// Same SQLite schema and authority boundaries as Rust workspace3::Journal.
// No legacy snapshot import, reseed, or optimistic cursor advancement.
import Foundation
import CryptoKit
import SQLite3

struct Workspace3Pending {
    let id: String
    let request: String
    let hash: String
}
struct Workspace3Window {
    let next: String?
    let rows: [Workspace3Row]
}

@MainActor
final class Workspace3Journal {
    let scope: Workspace3Scope
    private var db: OpaquePointer?
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    static func defaultURL(scope: Workspace3Scope) throws -> URL {
        let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("CypherV3", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let key = SHA256.hash(data: try Workspace3Wire.data(scope)).map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent("workspace3-\(key).sqlite")
    }
    init(url: URL, scope: Workspace3Scope) throws {
        try scope.validate()
        self.scope = scope
        guard sqlite3_open(url.path, &db) == SQLITE_OK else {
            if let db { sqlite3_close(db) }; db = nil; try Workspace3Wire.fail("storage_open")
        }
        do {
            sqlite3_busy_timeout(db, 5000)
            let version = try query("PRAGMA user_version").first?.first
            let tables = try query("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").flatMap { $0 }
            guard (version == "0" && tables.isEmpty) || (version == "3" && tables.contains("workspace3_meta")) else {
                try Workspace3Wire.fail("unsupported_workspace_format")
            }
            for path in [url.path, url.path + "-wal", url.path + "-shm"] where FileManager.default.fileExists(atPath: path) {
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path)
            }
            for sql in [
                "PRAGMA journal_mode=WAL", "PRAGMA synchronous=FULL",
                "CREATE TABLE IF NOT EXISTS workspace3_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),scope TEXT NOT NULL,cursor INTEGER NOT NULL DEFAULT 0,clock_ms INTEGER NOT NULL DEFAULT 0,clock_counter INTEGER NOT NULL DEFAULT 0)",
                "CREATE TABLE IF NOT EXISTS workspace3_rows(kind TEXT NOT NULL,id TEXT NOT NULL,seq INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(kind,id))",
                "CREATE TABLE IF NOT EXISTS workspace3_outbox(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,request TEXT NOT NULL,hash TEXT NOT NULL)",
                "CREATE TABLE IF NOT EXISTS workspace3_pending(batch TEXT NOT NULL,position INTEGER NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(batch,position),FOREIGN KEY(batch) REFERENCES workspace3_outbox(id) ON DELETE CASCADE)",
                "CREATE INDEX IF NOT EXISTS workspace3_pending_row ON workspace3_pending(kind,id)",
                "PRAGMA foreign_keys=ON"
            ] { try execute(sql) }
            try transaction {
                try execute("INSERT OR IGNORE INTO workspace3_meta(singleton,scope) VALUES(1,?)", [try json(scope)])
                guard let stored = try query("SELECT scope FROM workspace3_meta WHERE singleton=1").first?.first,
                      try JSONDecoder().decode(Workspace3Scope.self, from: Data(stored.utf8)) == scope else {
                    try Workspace3Wire.fail("workspace_scope_mismatch")
                }
                try Workspace3Wire.shape(JSONDecoder().decode([String: JSONValue].self, from: Data(stored.utf8)),
                                         ["endpoint", "org", "user", "actor"])
                try execute("PRAGMA user_version=3")
            }
        } catch {
            sqlite3_close(db); db = nil; throw error
        }
    }
    deinit { if let db { sqlite3_close(db) } }
    var cursor: UInt64 {
        get throws {
            guard let text = try query("SELECT cursor FROM workspace3_meta WHERE singleton=1").first?.first,
                  let n = UInt64(text), n <= Workspace3Wire.maxSafe else { try Workspace3Wire.fail("invalid_cursor") }
            return n
        }
    }
    func canonical(kind: String, id: String) throws -> Workspace3Row? {
        guard let body = try query("SELECT body FROM workspace3_rows WHERE kind=? AND id=?", [kind, id]).first?.first else { return nil }
        let row = try JSONDecoder().decode(Workspace3Row.self, from: Data(body.utf8))
        try Workspace3Wire.row(row)
        return row
    }
    func row(kind: String, id: String) throws -> Workspace3Row? {
        var row = try canonical(kind: kind, id: id)
        try each("SELECT p.body FROM workspace3_pending p JOIN workspace3_outbox o ON o.id=p.batch WHERE p.kind=? AND p.id=? ORDER BY o.ordinal,p.position", [kind, id]) {
            let op = try JSONDecoder().decode(Workspace3Op.self, from: Data($0[0].utf8))
            try Workspace3Wire.operation(op, actor: scope.actor, local: scope.endpoint == "local")
            row = Workspace3Metadata.apply(row, op).row
        }
        return row
    }
    func window(kind: String, after: String = "", limit: Int = 32) throws -> Workspace3Window {
        guard Workspace3Wire.kind(kind), (1...32).contains(limit) else { try Workspace3Wire.fail("invalid_workspace_window") }
        let ids = try query("SELECT id FROM workspace3_rows WHERE kind=? AND id>? UNION SELECT id FROM workspace3_pending WHERE kind=? AND id>? ORDER BY id LIMIT ?",
                            [kind, after, kind, after, String(limit)])
        var rows: [Workspace3Row] = [], next: String?, bytes = 1024
        for id in ids {
            if let row = try row(kind: kind, id: id[0]) {
                let size = try Workspace3Wire.data(row).count
                if bytes + size > Workspace3Wire.frameBytes { break }
                rows.append(row); bytes += size
            }
            next = id[0]
        }
        return Workspace3Window(next: next, rows: rows)
    }
    func pending() throws -> Workspace3Pending? {
        guard let row = try query("SELECT id,request,hash FROM workspace3_outbox ORDER BY ordinal LIMIT 1").first else { return nil }
        guard hash(row[1]) == row[2] else { try Workspace3Wire.fail("workspace_pending_corrupt") }
        return Workspace3Pending(id: row[0], request: row[1], hash: row[2])
    }
    func mutate(_ operations: [Workspace3Op], now: Int64) throws {
        guard operations.count <= 4096, now >= 0 else { try Workspace3Wire.fail("workspace_mutation_too_large") }
        try transaction { for op in operations { try stage(op, now: now) } }
    }
    private func stage(_ supplied: Workspace3Op, now: Int64) throws {
        guard let c = try query("SELECT clock_ms,clock_counter FROM workspace3_meta WHERE singleton=1").first,
              var ms = Int64(c[0]), var counter = Int64(c[1]), ms >= 0, ms < 10_000_000_000_000,
              (0...999999).contains(counter) else {
            try Workspace3Wire.fail("invalid_clock")
        }
        if now > ms { ms = now; counter = 0 }
        else if counter == 999999 { ms += 1; counter = 0 }
        else { counter += 1 }
        guard ms < 10_000_000_000_000 else { try Workspace3Wire.fail("clock_exhausted") }
        let op = Workspace3Op(kind: supplied.kind, id: supplied.id, op: supplied.op, set: supplied.set,
                            hlc: Workspace3Metadata.clock(ms: ms, counter: UInt32(counter), actor: scope.actor))
        try Workspace3Wire.operation(op, actor: scope.actor, local: scope.endpoint == "local")
        guard let count = try query("SELECT COUNT(*) FROM workspace3_pending").first?.first, let n = Int(count), n < 1024 else {
            try Workspace3Wire.fail("workspace_outbox_full")
        }
        let next = Workspace3Metadata.apply(try row(kind: op.kind, id: op.id), op).row
        if let next { try Workspace3Wire.row(next, optimistic: true) }
        if scope.endpoint == "local" {
            if var next {
                let through = try cursor
                guard through < Workspace3Wire.maxSafe else { try Workspace3Wire.fail("sequence_exhausted") }
                next.seq = through + 1
                try put(next)
                try execute("UPDATE workspace3_meta SET cursor=? WHERE singleton=1", [String(next.seq)])
            }
        } else {
            let id = UUID().uuidString.lowercased()
            struct Push: Encodable { let version = 3; let type = "push"; let id: String; let ops: [Workspace3Op] }
            let request = try json(Push(id: id, ops: [op]))
            try execute("INSERT INTO workspace3_outbox(id,request,hash) VALUES(?,?,?)", [id, request, hash(request)])
            try execute("INSERT INTO workspace3_pending VALUES(?,0,?,?,?)", [id, op.kind, op.id, try json(op)])
        }
        try execute("UPDATE workspace3_meta SET clock_ms=?,clock_counter=? WHERE singleton=1", [String(ms), String(counter)])
    }
    func applyPage(after: UInt64, page: Workspace3Page) throws {
        guard scope.endpoint != "local" else { try Workspace3Wire.fail("local_authority_cannot_join_cloud") }
        try page.validate(after: after)
        try transaction {
            guard try cursor == after else { try Workspace3Wire.fail("workspace_cursor_changed") }
            for row in page.rows { try put(row) }
            try execute("UPDATE workspace3_meta SET cursor=? WHERE singleton=1", [String(page.next)])
        }
    }
    func acknowledge(id: String, hash: String, through: UInt64, rows: [Workspace3Row]) throws {
        guard through <= Workspace3Wire.maxSafe, rows.count <= 3 else { try Workspace3Wire.fail("invalid_workspace_ack") }
        try transaction {
            guard try query("SELECT hash FROM workspace3_outbox WHERE id=?", [id]).first?.first == hash else {
                try Workspace3Wire.fail("workspace_ack_mismatch")
            }
            var seen = Set<String>()
            for row in rows {
                guard row.seq <= through, seen.insert("\(row.kind):\(row.id)").inserted,
                      !(try query("SELECT body FROM workspace3_pending WHERE batch=? AND kind=? AND id=?", [id, row.kind, row.id])).isEmpty else {
                    try Workspace3Wire.fail("workspace_ack_mismatch")
                }
                try put(row)
            }
            try each("SELECT body FROM workspace3_pending WHERE batch=?", [id]) {
                let op = try JSONDecoder().decode(Workspace3Op.self, from: Data($0[0].utf8))
                if !seen.contains("\(op.kind):\(op.id)"),
                   try (op.op != .update || canonical(kind: op.kind, id: op.id) != nil) {
                    try Workspace3Wire.fail("workspace_ack_missing_row")
                }
            }
            try execute("DELETE FROM workspace3_outbox WHERE id=?", [id])
        }
    }
    private func put(_ row: Workspace3Row) throws {
        try Workspace3Wire.row(row)
        if let old = try canonical(kind: row.kind, id: row.id) {
            if old.seq > row.seq { return }
            if old.seq == row.seq {
                guard old == row else { try Workspace3Wire.fail("workspace_row_conflict") }
                return
            }
        }
        for clock in Array(row.clocks.values) + (row.delHlc.map { [$0] } ?? []) {
            let (ms, counter) = try Workspace3Wire.clock(clock)
            try execute("UPDATE workspace3_meta SET clock_ms=?,clock_counter=? WHERE singleton=1 AND (clock_ms<? OR (clock_ms=? AND clock_counter<?))",
                        [String(ms), String(counter), String(ms), String(ms), String(counter)])
        }
        try execute("INSERT INTO workspace3_rows VALUES(?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,body=excluded.body",
                    [row.kind, row.id, String(row.seq), try json(row)])
    }
    private func json<T: Encodable>(_ value: T) throws -> String { String(decoding: try Workspace3Wire.data(value), as: UTF8.self) }
    private func hash(_ value: String) -> String { SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined() }
    private func transaction(_ body: () throws -> Void) throws {
        try execute("BEGIN IMMEDIATE")
        do { try body(); try execute("COMMIT") }
        catch { try? execute("ROLLBACK"); throw error }
    }
    private func execute(_ sql: String, _ args: [String] = []) throws { try each(sql, args) { _ in } }
    private func query(_ sql: String, _ args: [String] = []) throws -> [[String]] {
        var rows: [[String]] = []
        try each(sql, args) { rows.append($0) }
        return rows
    }
    private func each(_ sql: String, _ args: [String] = [], body: ([String]) throws -> Void) throws {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else { try Workspace3Wire.fail("storage_prepare") }
        defer { sqlite3_finalize(statement) }
        for (index, arg) in args.enumerated() {
            let result = arg.withCString { sqlite3_bind_text(statement, Int32(index + 1), $0, Int32(arg.utf8.count), transient) }
            guard result == SQLITE_OK else { try Workspace3Wire.fail("storage_bind") }
        }
        while true {
            let step = sqlite3_step(statement)
            if step == SQLITE_DONE { break }
            guard step == SQLITE_ROW else { try Workspace3Wire.fail("storage_step") }
            var row: [String] = []
            for column in 0..<sqlite3_column_count(statement) {
                let count = Int(sqlite3_column_bytes(statement, column))
                guard count <= Workspace3Wire.frameBytes, let ptr = sqlite3_column_text(statement, column) else {
                    try Workspace3Wire.fail("storage_value")
                }
                guard let value = String(data: Data(bytes: ptr, count: count), encoding: .utf8) else { try Workspace3Wire.fail("storage_encoding") }
                row.append(value)
            }
            try body(row)
        }
    }
}

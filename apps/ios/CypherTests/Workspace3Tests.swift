import XCTest
import SQLite3
import CryptoKit
@testable import Cypher

@MainActor
final class Workspace3Tests: XCTestCase {
    private func location() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        return directory.appendingPathComponent("workspace.sqlite")
    }
    private func scope(_ endpoint: String = "https://edge.test", actor: String = "phone") -> Workspace3Scope {
        Workspace3Scope(endpoint: endpoint, org: "org", user: "user", actor: actor)
    }
    private func op(_ id: String = "chat", title: String = "hello") -> Workspace3Op {
        Workspace3Op(kind: "chats", id: id, op: .upsert, set: ["id": .string(id), "title": .string(title)],
                   hlc: "")
    }
    private func canonical(_ pending: Workspace3Pending, seq: UInt64) throws -> Workspace3Row {
        struct Push: Decodable { let ops: [Workspace3Op] }
        let push = try JSONDecoder().decode(Push.self, from: Data(pending.request.utf8))
        var row = try XCTUnwrap(Workspace3Metadata.apply(nil, push.ops[0]).row)
        row.seq = seq
        return row
    }
    func testOfflineRestartExactReceiptAndContiguousCursor() throws {
        let url = try location()
        var journal: Workspace3Journal? = try Workspace3Journal(url: url, scope: scope())
        try journal!.mutate([op(title: "完整🙂")], now: 100)
        let pending = try XCTUnwrap(journal!.pending())
        XCTAssertEqual(pending.hash, SHA256.hash(data: Data(pending.request.utf8)).map { String(format: "%02x", $0) }.joined())
        XCTAssertEqual(try journal!.row(kind: "chats", id: "chat")?.fields["title"], .string("完整🙂"))
        journal = nil
        journal = try Workspace3Journal(url: url, scope: scope())
        XCTAssertEqual(try journal!.pending()?.request, pending.request)
        let row = try canonical(pending, seq: 1)
        XCTAssertThrowsError(try journal!.acknowledge(id: pending.id, hash: "wrong", through: 1, rows: [row]))
        XCTAssertNotNil(try journal!.pending())
        try journal!.acknowledge(id: pending.id, hash: pending.hash, through: 1, rows: [row])
        XCTAssertNil(try journal!.pending())
        XCTAssertEqual(try journal!.cursor, 0, "ACK is not a contiguous cursor")
        try journal!.applyPage(after: 0, page: Workspace3Page(through: 1, next: 1, done: true, rows: [row]))
        XCTAssertEqual(try journal!.cursor, 1)
        XCTAssertThrowsError(try journal!.applyPage(after: 1, page: Workspace3Page(through: 0, next: 0, done: true, rows: [])))
        XCTAssertThrowsError(try Workspace3Journal(url: url, scope: scope(actor: "other")))
    }
    func testCascadeAndReceiptRollbackAreAtomic() throws {
        let url = try location(), journal = try Workspace3Journal(url: url, scope: scope())
        var bad = op("second"); bad.set?["id"] = .string("wrong")
        XCTAssertThrowsError(try journal.mutate([op(), bad], now: 100))
        XCTAssertNil(try journal.pending())
        XCTAssertNil(try journal.row(kind: "chats", id: "chat"))
        try journal.mutate([op()], now: 100)
        let pending = try XCTUnwrap(journal.pending()), row = try canonical(pending, seq: 1)
        XCTAssertEqual(row.clocks["title"], "0000000000100-000000-phone", "failed cascade did not consume a clock")
        var db: OpaquePointer?
        XCTAssertEqual(sqlite3_open(url.path, &db), SQLITE_OK)
        defer { sqlite3_close(db) }
        XCTAssertEqual(sqlite3_exec(db, "CREATE TRIGGER reject_ack BEFORE DELETE ON workspace3_outbox BEGIN SELECT RAISE(ABORT,'fail'); END", nil, nil, nil), SQLITE_OK)
        XCTAssertThrowsError(try journal.acknowledge(id: pending.id, hash: pending.hash, through: 1, rows: [row]))
        XCTAssertNil(try journal.canonical(kind: "chats", id: "chat"))
        XCTAssertNotNil(try journal.pending())
        XCTAssertEqual(try journal.cursor, 0)
    }
    func testLocalAuthorityAndBoundedWindowsCannotRebindToCloud() throws {
        let url = try location(), journal = try Workspace3Journal(url: url, scope: scope("local"))
        try journal.mutate((0..<37).map { op(String(format: "chat-%02d", $0)) }, now: 100)
        XCTAssertNil(try journal.pending())
        var next = "", ids: [String] = []
        while true {
            let page = try journal.window(kind: "chats", after: next, limit: 10)
            XCTAssertLessThanOrEqual(page.rows.count, 10)
            guard let after = page.next else { break }
            ids += page.rows.map(\.id); next = after
        }
        XCTAssertEqual(ids.count, 37); XCTAssertEqual(Set(ids).count, 37)
        XCTAssertThrowsError(try journal.applyPage(after: 37, page: Workspace3Page(through: 37, next: 37, done: true, rows: [])))
        XCTAssertThrowsError(try Workspace3Journal(url: url, scope: scope()))
    }
    func testObservedClocksBeatRemoteFutureAndUnsafeShapesFail() throws {
        let journal = try Workspace3Journal(url: location(), scope: scope())
        let row = Workspace3Row(kind: "chats", id: "chat", seq: 1, deleted: false,
                              fields: ["title": .string("remote")], clocks: ["title": "9000000000000-999999-host"])
        try journal.applyPage(after: 0, page: Workspace3Page(through: 1, next: 1, done: true, rows: [row]))
        try journal.mutate([op(title: "local")], now: 100)
        XCTAssertEqual(try journal.row(kind: "chats", id: "chat")?.fields["title"], .string("local"))
        XCTAssertThrowsError(try Workspace3Wire.decode(#"{"version":3,"type":"changed","through":9007199254740992}"#))
        XCTAssertThrowsError(try Workspace3Wire.decode(#"{"version":3,"type":"changed","through":1,"extra":true}"#))
        XCTAssertThrowsError(try Workspace3Wire.decode(#"{"version":3,"type":"reply","id":"x","sequence":0,"done":"true","value":null}"#))
        XCTAssertNil(JSONValue.double(1e100).int64Value)
    }
    func testRPCFragmentsPreserveUnicodeAndRejectReorderingAndPoison() throws {
        let value = JSONValue.object(["text": .string(String(repeating: "🙂中文\n\"\\\0", count: 60000))])
        var encoder = try Workspace3RPCCodec.Encoder(value), decoder = Workspace3RPCCodec.Decoder()
        var parts: [JSONValue] = []
        while let part = encoder.next() {
            XCTAssertLessThan(try Workspace3Wire.data(part).count, 64 * 1024)
            parts.append(part)
        }
        for (index, part) in parts.enumerated() {
            let result = try decoder.push(part)
            if index == parts.count - 1 { XCTAssertEqual(result, value) }
            else { XCTAssertNil(result) }
        }
        XCTAssertFalse(decoder.incomplete)
        var bad = Workspace3RPCCodec.Decoder()
        XCTAssertThrowsError(try bad.push(parts[1]))
        XCTAssertThrowsError(try bad.push(parts[0]), "poisoned decoder cannot restart")
        var null = try Workspace3RPCCodec.Encoder(.null)
        XCTAssertEqual(try decoder.push(XCTUnwrap(null.next())), .null)
        XCTAssertThrowsError(try Workspace3RPCCodec.Encoder(.string(String(repeating: "x", count: Workspace3RPCCodec.maximumBytes))))
    }
    func testHeaderOnlyWorkspaceRequestAndInvalidatedIdentity() async throws {
        let config = AppConfig(edgeURL: URL(string: "https://edge.test")!, mode: .dev, userId: "user",
                               orgId: "org", deviceId: "phone", deviceName: "Phone", devBearer: "private-token")
        let request = try await config.workspace3Request()
        XCTAssertEqual(request.url?.absoluteString, "wss://edge.test/workspace3/org/ws")
        XCTAssertNil(request.url?.query)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer private-token")
        for socket in [false, true] {
            let conversation = try await config.sync3Request(chatId: "chat", socket: socket)
            XCTAssertEqual(conversation.value(forHTTPHeaderField: "x-cypher-expected-user"), "user")
            XCTAssertNil(conversation.url?.query)
        }
        config.invalidate()
        do { _ = try await config.workspace3Request(); XCTFail("retired credentials reused") } catch {}
    }
    func testNormalStoreUsesNativeMutationAndDescendantCascade() async throws {
        let journal = try Workspace3Journal(url: location(), scope: scope("local"))
        try journal.mutate([Workspace3Op(kind: "spaces", id: "project", op: .upsert,
            set: ["deviceId": .string("host"), "path": .string("/project")], hlc: "")], now: 1)
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev, userId: "user", orgId: "org", deviceId: "phone", deviceName: "Phone")
        let store = WorkspaceStore(config: config, initialJournal: journal)
        let parent = try XCTUnwrap(store.createChat(space: store.spaces[0], config: ChatConfig(harness: "pi", model: nil, reasoning: nil, sandbox: nil)))
        XCTAssertNil(try journal.row(kind: "chats", id: parent)?.fields["roomGen"])
        let relation: JSONValue = .object(["parentChatId": .string(parent), "parentRunId": .string("run"),
            "agent": .string("planner"), "task": .string("task"), "mode": .string("async"), "profile": .object([:])])
        try journal.mutate([Workspace3Op(kind: "chats", id: "child", op: .upsert,
            set: ["deviceId": .string("host"), "child": relation], hlc: "")], now: 1)
        store.consume(.metadata([("chats", "child")]))
        XCTAssertNotNil(store.chats.first { $0.id == "child" })
        store.deleteChat(chatId: parent)
        XCTAssertTrue(try XCTUnwrap(journal.canonical(kind: "chats", id: parent)).deleted)
        XCTAssertTrue(try XCTUnwrap(journal.canonical(kind: "chats", id: "child")).deleted)
        XCTAssertTrue(store.chats.isEmpty)
        XCTAssertNil(try journal.pending())
    }
    func testNormalStoreStorageFailureDoesNotReportSuccessfulCreate() throws {
        let url = try location(), journal = try Workspace3Journal(url: url, scope: scope("local"))
        try journal.mutate([Workspace3Op(kind: "spaces", id: "project", op: .upsert,
            set: ["deviceId": .string("host"), "path": .string("/project")], hlc: "")], now: 1)
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev, userId: "user", orgId: "org", deviceId: "phone", deviceName: "Phone")
        let store = WorkspaceStore(config: config, initialJournal: journal)
        var db: OpaquePointer?; XCTAssertEqual(sqlite3_open(url.path, &db), SQLITE_OK)
        defer { sqlite3_close(db) }
        XCTAssertEqual(sqlite3_exec(db, "CREATE TRIGGER fail_write BEFORE INSERT ON workspace3_rows BEGIN SELECT RAISE(ABORT,'full'); END", nil, nil, nil), SQLITE_OK)
        XCTAssertNil(store.createChat(space: store.spaces[0], config: ChatConfig(harness: "pi", model: nil, reasoning: nil, sandbox: nil)))
        XCTAssertTrue(store.chats.isEmpty); XCTAssertNotNil(store.error)
        XCTAssertEqual(try journal.cursor, 1)
    }
    func testPresenceIsReplaceableAndPeerClosureIsConnectionSpecific() throws {
        let journal = try Workspace3Journal(url: location(), scope: scope("local"))
        try journal.mutate([Workspace3Op(kind: "chats", id: "chat", op: .upsert,
            set: ["deviceId": .string("host")], hlc: "")], now: 1)
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev, userId: "user", orgId: "org", deviceId: "phone", deviceName: "Phone")
        let store = WorkspaceStore(config: config, initialJournal: journal)
        func presence(_ connection: String) {
            store.consume(.frame(1, ["type": .string("presence"), "role": .string("host"),
                "actor": .string("host"), "connection": .string(connection), "expiresAt": .int(nowMs() + 45000),
                "state": .object(["sessions": .array([.object(["chatId": .string("chat"), "deviceId": .string("host"),
                    "status": .string("working"), "updatedAt": .string("2026-01-01T00:00:00Z")])])])]))
        }
        presence("old"); presence("new")
        XCTAssertTrue(store.deviceOnline("host")); XCTAssertEqual(store.sessions["chat"]?.status, .working)
        store.consume(.frame(1, ["type": .string("peerClosed"), "actor": .string("host"), "connection": .string("old")]))
        XCTAssertTrue(store.deviceOnline("host"))
        XCTAssertEqual(try journal.cursor, 1)
        store.consume(.frame(1, ["type": .string("peerClosed"), "actor": .string("host"), "connection": .string("new")]))
        XCTAssertFalse(store.deviceOnline("host")); XCTAssertNil(store.sessions["chat"])
    }
    func testRemoteCloseCancelsConnectionWaitWithoutRetiringSharedContext() async throws {
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: UUID().uuidString.lowercased(),
            deviceName: "Test", devBearer: "test@test")
        let context = try config.workspaceContext()
        XCTAssertTrue(context === (try config.workspaceContext()))
        let remote = WorkspaceRemote(deviceId: "host", config: config)
        let request = Task<String, Error> { try await remote.call(method: "ListFolders", params: [:]) }
        try await Task.sleep(for: .milliseconds(50))
        await remote.close()
        do { _ = try await request.value; XCTFail("closed facade completed a call") } catch {}
        XCTAssertTrue(context === (try config.workspaceContext()), "other consumers keep the same account connection")
        config.invalidate()
        XCTAssertThrowsError(try config.workspaceContext())
        context.retire()
        await context.client.stop()
    }
}

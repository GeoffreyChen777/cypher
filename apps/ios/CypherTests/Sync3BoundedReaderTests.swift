import XCTest
import SQLite3
@testable import Cypher

@MainActor
final class Sync3BoundedReaderTests: XCTestCase {
    private func configuration() -> AppConfig {
        AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "phone", deviceName: "Phone")
    }
    private func append(_ journal: Sync3Journal, event: [String: JSONValue]) throws {
        let seq = try journal.cursor + 1
        try commit(journal, operation: Sync3Operation(id: "event-\(seq)", actor: "host", ownerEpoch: 1, event: event))
    }
    private func commit(_ journal: Sync3Journal, operation: Sync3Operation) throws {
        let seq = try journal.cursor + 1
        let rows = try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode([Sync3Row(seq: seq, operation: operation)]))
        try journal.applyPage(["type": .string("page"), "version": .int(3), "epoch": .int(1),
            "through": .int(seq), "next": .int(seq), "done": .bool(true), "rows": rows])
    }
    private func message(_ journal: Sync3Journal, id: String, text: String, continuation: String? = nil) throws {
        try append(journal, event: ["type": .string("messageCreated"), "runId": .null,
            "messageId": .string(id), "role": .string("user"), "deviceId": .string("host"),
            "createdAt": .int(1), "continuationOf": continuation.map(JSONValue.string) ?? .null])
        try append(journal, event: ["type": .string("partPut"), "messageId": .string(id), "index": .int(0),
            "part": .object(["kind": .string("text"), "id": .string("text"), "text": .string(text)])])
        try append(journal, event: ["type": .string("messageFinished"), "messageId": .string(id), "status": .string("complete")])
    }
    func testNormalStoreRendersNestedIdentitiesAcrossByteLimitedPagesAndRestart() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("journal.sqlite")
        let config = configuration()
        let store = SessionStore(chatId: "chat", config: config, journalURL: url)
        let journal = try XCTUnwrap(store.sync3Journal)
        try journal.acceptState(["type": .string("state"), "version": .int(3), "epoch": .int(1),
            "owner": .string("host"), "ownerEpoch": .int(1), "head": .int(0)])
        let text = String(repeating: "完整🙂", count: 4096)
        for index in 0..<75 { try message(journal, id: "message-\(index)", text: "\(index):\(text)") }
        XCTAssertLessThan(try journal.messageWindow().messages.count, 32, "byte budget, not just row count, must page")
        store.project()
        XCTAssertNil(store.error)
        XCTAssertEqual(store.entries.map(\.id), (0..<75).map { "message-\($0)" })
        for (index, entry) in store.entries.enumerated() {
            guard case .text(_, let actual) = entry.parts.first else { return XCTFail("missing rendered text") }
            XCTAssertEqual(actual, "\(index):\(text)")
        }
        let revision = store.revision
        store.project()
        XCTAssertEqual(store.revision, revision, "same cursor must not rebuild")
        let reopened = SessionStore(chatId: "chat", config: config, journalURL: url)
        reopened.project()
        XCTAssertNil(reopened.error)
        XCTAssertEqual(reopened.entries, store.entries)

        // Late refresh must not drop either old pages or the new message.
        try message(journal, id: "last", text: "after reopen")
        reopened.project()
        XCTAssertEqual(reopened.entries.count, 76)
        XCTAssertEqual(reopened.entries.last?.id, "last")
    }

    func testNormalStoreJoinsContinuationAndPreservesSteerEchoIdentity() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SessionStore(chatId: "chat", config: configuration(), journalURL: directory.appendingPathComponent("journal.sqlite"))
        let journal = try XCTUnwrap(store.sync3Journal)
        try journal.acceptState(["type": .string("state"), "version": .int(3), "epoch": .int(1),
            "owner": .string("host"), "ownerEpoch": .int(1), "head": .int(0)])
        XCTAssertTrue(store.sendSteer(prompt: "keep this"))
        let queued = try XCTUnwrap(journal.pending().first)
        let id = try XCTUnwrap(store.pendingSends.first?.messageId)
        try commit(journal, operation: queued)
        try message(journal, id: id, text: "keep this")
        store.project()
        XCTAssertNil(store.error)
        XCTAssertTrue(store.pendingSends.isEmpty)
        XCTAssertEqual(store.entries.first?.id, id)
        XCTAssertEqual(store.entries.first?.isSteer, true)
        // Continuations preserve their logical root after native record decoding.
        try message(journal, id: "\(id)#c1", text: "continued", continuation: id)
        store.project()
        XCTAssertEqual(store.entries.count, 1)
        XCTAssertEqual(store.entries.first?.id, id)
        XCTAssertEqual(store.entries.first?.isSteer, true)
    }

    func testRenderDoesNotLoadUnrelatedProjectionTablesOrSilentlyDropCorruptMessages() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("journal.sqlite")
        let store = SessionStore(chatId: "chat", config: configuration(), journalURL: url)
        let journal = try XCTUnwrap(store.sync3Journal)
        try journal.acceptState(["type": .string("state"), "version": .int(3), "epoch": .int(1),
            "owner": .string("host"), "ownerEpoch": .int(1), "head": .int(0)])
        try message(journal, id: "visible", text: "must stay visible")
        var db: OpaquePointer?
        XCTAssertEqual(sqlite3_open(url.path, &db), SQLITE_OK)
        defer { sqlite3_close(db) }
        XCTAssertEqual(sqlite3_exec(db, """
            INSERT INTO sync3_entities(kind,id,body,seq) VALUES('executions','irrelevant','not JSON',1)
            """, nil, nil, nil), SQLITE_OK)
        XCTAssertThrowsError(try journal.projection)
        store.project()
        XCTAssertNil(store.error, "renderer must not use whole projection")
        XCTAssertEqual(store.entries.map(\.id), ["visible"])
        XCTAssertEqual(sqlite3_exec(db, """
            UPDATE sync3_entities SET body='{}' WHERE kind='messages';
            UPDATE sync3_meta SET cursor=cursor+1;
            """, nil, nil, nil), SQLITE_OK)
        store.project()
        XCTAssertNotNil(store.error, "invalid records must be errors, not compactMap omissions")
        XCTAssertEqual(store.entries.map(\.id), ["visible"], "failed refresh retains the prior valid view")
    }
    func testLargeMessageSetIsReadThroughBoundedIndexedWindows() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: url) }
        let journal = try Sync3Journal(url: url, account: "account", room: "room", actor: "actor")
        _ = try journal.acceptState(["type":.string("state"), "version":.int(3), "epoch":.int(1),
            "owner":.string("actor"), "ownerEpoch":.int(1), "head":.int(75)])
        for index in 1...75 {
            let event = try Sync3Operation(id: "message-\(index)", actor: "actor", ownerEpoch: 1,
                event: ["type":.string("messageCreated"), "runId":.null,
                    "messageId":.string("message-\(index)"), "role":.string("assistant"),
                    "deviceId":.string("actor"), "createdAt":.int(Int64(index)), "continuationOf":.null])
            try journal.applyPage(["type":.string("page"), "version":.int(3), "epoch":.int(1),
                "through":.int(Int64(index)), "next":.int(Int64(index)), "done":.bool(true),
                "rows":.array([.object(["seq":.int(Int64(index)), "operation":.init(encodable: event)!])])])
        }
        let rows = try journal.allMessagesBounded()
        XCTAssertEqual(rows.count, 75)
        XCTAssertEqual(rows.first?["createdSeq"], .int(1))
        XCTAssertEqual(rows.last?["createdSeq"], .int(75))
    }
}

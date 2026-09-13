import XCTest
@testable import Cypher

@MainActor
final class SessionCommandTests: XCTestCase {
    private func makeStore() -> SessionStore {
        // Not started, no host target/token: in-memory doc only, no network
        // or disk and no Runtime/LLM calls.
        SessionStore(chatId: UUID().uuidString, config: AppConfig(
            edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Phone"))
    }

    private func commands(_ store: SessionStore) -> [[String: JSONValue]] {
        (try? store.sync3Journal?.pending().compactMap { operation in
            guard operation.event["type"] == .string("commandQueued"),
                  let command = operation.event["command"]?.objectValue else { return nil }
            return command
        }) ?? []
    }

    func testResumeQueuesPiOnExistingSessionWithoutChangingHostConfig() throws {
        let store = makeStore()
        let chat = Chat(id: store.chatId, deviceId: "linux-host", title: "Desktop session",
                        archived: false, cwd: "/srv/project", branch: "topic", checkoutId: nil,
                        config: ChatConfig(harness: "pi", model: "provider/model", reasoning: "high",
                                           modelOptions: ["custom": .string("keep")], sandbox: "workspace-write"),
                        lastMessagePreview: nil, lastMessageAt: nil, createdAt: 0, spaceId: "project",
                        lastSeenAt: nil)
        XCTAssertTrue(store.sendRun(prompt: "Continue this task", chat: chat, attachments: ["/tmp/image.jpg"]))
        let rows = commands(store)
        XCTAssertEqual(rows.count, 1)
        let row = try XCTUnwrap(rows.first)
        XCTAssertEqual(row["issuedBy"]?.stringValue, "ios-test")
        XCTAssertEqual(row["status"]?.stringValue, "pending")
        let request = row["payload"]?.objectValue?["request"]?.objectValue
        XCTAssertEqual(request?["harness"]?.stringValue, "pi")
        XCTAssertEqual(request?["cwd"]?.stringValue, "/srv/project")
        XCTAssertEqual(request?["model"]?.stringValue, "provider/model")
        XCTAssertEqual(request?["modelOptions"]?.objectValue?["custom"]?.stringValue, "keep")
        XCTAssertEqual(store.pendingSends.count, 1)
        XCTAssertEqual(store.pendingSends.first?.text, "Continue this task")
        XCTAssertEqual(store.pendingSends.first?.isSteer, false)
        XCTAssertTrue(store.entries.isEmpty, "The phone queues commands; it never fabricates host replies")
    }

    func testSteerStopAndAnswerAreAppendOnlyCommands() {
        let store = makeStore()
        store.setEntries([MessageEntry(id: "turn-1", role: .user, parts: [],
                                       createdAt: 1, deviceId: "ios-test")])
        XCTAssertTrue(store.sendSteer(prompt: "Also add tests"))
        XCTAssertTrue(store.sendInterrupt())
        XCTAssertTrue(store.respondInput(requestId: "question-1", answers: []))
        let rows = commands(store)
        XCTAssertEqual(rows.compactMap { $0["payload"]?.objectValue?["kind"]?.stringValue }, ["steer", "interrupt", "respondInput"])
        XCTAssertEqual(Set(rows.compactMap { $0["id"]?.stringValue }).count, 3)
        XCTAssertEqual(rows.last?["payload"]?.objectValue?["requestId"]?.stringValue, "question-1")
        XCTAssertEqual(store.pendingSends.count, 1, "Only message-bearing commands have optimistic echoes")
        XCTAssertEqual(store.pendingSends.first?.isSteer, true)
    }

    func testOfflineQueueRestoresEchoAfterRestartAndUsesOwnerEpochNotLogEpoch() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("journal.sqlite")
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Phone")
        var store: SessionStore? = SessionStore(chatId: "restart", config: config, journalURL: url)
        let journal = try XCTUnwrap(store?.sync3Journal)
        try journal.acceptState(["type": .string("state"), "version": .int(3),
            "epoch": .int(7), "owner": .string("host"), "ownerEpoch": .int(3), "head": .int(0)])
        XCTAssertTrue(store!.sendSteer(prompt: "do not lose this"))
        let operation = try XCTUnwrap(journal.pending().first)
        XCTAssertEqual(operation.ownerEpoch, 3)
        let id = try XCTUnwrap(store?.pendingSends.first?.messageId)
        // A committed ACK alone must not retire the visible echo.
        try journal.acknowledge(["type": .string("ack"), "version": .int(3), "epoch": .int(7),
            "receipts": .array([.object(["id": .string(operation.id), "seq": .int(1)])])])
        store = nil
        let reopened = SessionStore(chatId: "restart", config: config, journalURL: url)
        XCTAssertNil(reopened.error)
        XCTAssertEqual(reopened.pendingSends.map(\.messageId), [id])
        XCTAssertEqual(reopened.pendingSends.first?.text, "do not lose this")
        XCTAssertEqual(reopened.pendingSends.first?.isSteer, true)
    }

    func testCorruptStorageDoesNotFallBackToAnotherTransport() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("journal.sqlite")
        try Data("not a database".utf8).write(to: url)
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Phone")
        let store = SessionStore(chatId: "broken", config: config, journalURL: url)
        XCTAssertNotNil(store.error)
        XCTAssertNil(store.sync3Journal)
        XCTAssertFalse(store.sendSteer(prompt: "must not appear sent"))
        XCTAssertTrue(store.pendingSends.isEmpty)
        XCTAssertEqual(try Data(contentsOf: url), Data("not a database".utf8))
    }
}

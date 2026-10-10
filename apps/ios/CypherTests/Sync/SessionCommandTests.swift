import XCTest
import Loro
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

    private func commands(_ store: SessionStore) -> [[String: LoroValue]] {
        (store.doc.getDeepValue().mapValue?["commands"]?.listValue ?? []).compactMap(\.mapValue)
    }

    func testResumeQueuesPiOnExistingSessionWithoutChangingHostConfig() {
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
        XCTAssertEqual(rows[0]["issuedBy"]?.stringValue, "ios-test")
        XCTAssertEqual(rows[0]["status"]?.stringValue, "pending")
        let request = rows[0]["payload"]?.mapValue?["request"]?.mapValue
        XCTAssertEqual(request?["harness"]?.stringValue, "pi")
        XCTAssertEqual(request?["cwd"]?.stringValue, "/srv/project")
        XCTAssertEqual(request?["model"]?.stringValue, "provider/model")
        XCTAssertEqual(request?["modelOptions"]?.mapValue?["custom"]?.stringValue, "keep")
        XCTAssertEqual(store.pendingSends.count, 1)
        XCTAssertEqual(store.pendingSends.first?.text, "Continue this task")
        XCTAssertEqual(store.pendingSends.first?.isSteer, false)
        XCTAssertTrue(store.entries.isEmpty, "The phone queues commands; it never fabricates host replies")
    }

    func testSteerStopAndAnswerAreAppendOnlyCommands() {
        let store = makeStore()
        XCTAssertTrue(store.sendSteer(prompt: "Also add tests"))
        XCTAssertTrue(store.sendInterrupt())
        XCTAssertTrue(store.respondInput(requestId: "question-1", answers: []))
        let rows = commands(store)
        XCTAssertEqual(rows.compactMap { $0["kind"]?.stringValue }, ["steer", "interrupt", "respondInput"])
        XCTAssertEqual(Set(rows.compactMap { $0["id"]?.stringValue }).count, 3)
        XCTAssertEqual(rows.last?["payload"]?.mapValue?["requestId"]?.stringValue, "question-1")
        XCTAssertEqual(store.pendingSends.count, 1, "Only message-bearing commands have optimistic echoes")
        XCTAssertEqual(store.pendingSends.first?.isSteer, true)
    }
}

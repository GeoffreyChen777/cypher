import XCTest
import Loro
@testable import Cypher

@MainActor
final class SessionCommandTests: XCTestCase {
    private func makeStore() -> SessionStore {
        // Not started, no host target/token: in-memory doc only, no network
        // or disk and no Runtime/LLM calls.
        SessionStore(
            chatId: UUID().uuidString,
            config: AppConfig(
                edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
                userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Phone"))
    }

    private func commands(_ store: SessionStore) -> [[String: LoroValue]] {
        (store.doc.getDeepValue().mapValue?["commands"]?.listValue ?? []).compactMap(\.mapValue)
    }

    func testResumeQueuesPiOnExistingSessionWithoutChangingHostConfig() {
        let store = makeStore()
        let chat = Chat(
            id: store.chatId, deviceId: "linux-host", title: "Desktop session",
            archived: false, cwd: "/srv/project", branch: "topic", checkoutId: nil,
            config: ChatConfig(
                harness: "pi", model: "provider/model", reasoning: "high",
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

    private func question(_ requestId: String, resolved: Bool) -> MessageEntry {
        .fixture(
            "entry-\(requestId)",
            parts: [
                .input(
                    id: requestId, requestId: requestId,
                    questions: [
                        UserInputQuestion(
                            id: "q1", header: "Pick", question: "Which?", options: ["a", "b"],
                            multiSelect: false)
                    ], resolved: resolved)
            ])
    }

    func testAnsweredQuestionRetiresOnTapUntilResolved() {
        let store = makeStore()
        store.setEntries([question("req-1", resolved: false)])
        XCTAssertEqual(store.openInputRequest?.requestId, "req-1")
        let answer = [UserInputAnswer(questionId: "q1", labels: ["a"])]
        XCTAssertTrue(store.respondInput(requestId: "req-1", answers: answer))
        XCTAssertNil(store.openInputRequest, "The panel must not wait out the host round trip")
        XCTAssertTrue(store.respondInput(requestId: "req-1", answers: answer))
        XCTAssertEqual(commands(store).count, 1, "A second tap must not queue a duplicate answer")

        // The host resolves it: the bookkeeping is dropped.
        store.setEntries([question("req-1", resolved: true)])
        XCTAssertTrue(store.answeredInputs.isEmpty)
        XCTAssertNil(store.openInputRequest)
    }

    func testFailedAnswerBringsTheQuestionBack() {
        let store = makeStore()
        store.setEntries([question("req-1", resolved: false)])
        XCTAssertTrue(store.respondInput(requestId: "req-1", answers: []))
        XCTAssertNil(store.openInputRequest)
        store.reopenInput("req-1", reason: "Try again")
        XCTAssertEqual(store.openInputRequest?.requestId, "req-1")
        XCTAssertEqual(store.inputAnswerFailure, "Try again")
        XCTAssertTrue(store.respondInput(requestId: "req-1", answers: []))
        XCTAssertNil(store.inputAnswerFailure)
    }

    func testCommandOutcomesReadAnswersAndStopsWithTheirResolution() {
        let root: [String: LoroValue] = [
            "commands": .list(value: [
                .map(value: [
                    "id": .string(value: "c1"), "kind": .string(value: "respondInput"),
                    "status": .string(value: "rejected"),
                    "resolution": .string(value: "no pending input request"),
                ]),
                .map(value: [
                    "id": .string(value: "c2"), "kind": .string(value: "respondInput"),
                    "status": .string(value: "pending"),
                ]),
                .map(value: [
                    "id": .string(value: "c3"), "kind": .string(value: "steer"),
                    "status": .string(value: "applied"),
                ]),
                .map(value: [
                    "id": .string(value: "c4"), "kind": .string(value: "interrupt"),
                    "status": .string(value: "applied"), "resolution": .string(value: "no live run"),
                ]),
            ])
        ]
        XCTAssertEqual(
            SessionStore.inputCommandOutcomes(root: root),
            [
                "c1": CommandOutcome(status: "rejected", resolution: "no pending input request"),
                "c2": CommandOutcome(status: "pending"),
                "c4": CommandOutcome(status: "applied", resolution: "no live run"),
            ])
    }

    private func sentCommandId(_ store: SessionStore, kind: String) -> String? {
        commands(store).last { $0["kind"]?.stringValue == kind }?["id"]?.stringValue
    }

    func testAnAnswerShowsItsHostOutcome() throws {
        let store = makeStore()
        store.setEntries([question("req-1", resolved: false)])
        XCTAssertNil(store.sentAnswerOutcome)
        XCTAssertTrue(store.respondInput(requestId: "req-1", answers: []))
        let id = try XCTUnwrap(sentCommandId(store, kind: "respondInput"))
        XCTAssertEqual(store.sentAnswerOutcome, .sending, "Not in a projection yet")
        store.adoptCommandOutcomes([id: CommandOutcome(status: "pending")])
        XCTAssertEqual(store.sentAnswerOutcome?.text, "Sending…")
        // The host turns it down: the panel stays retired and says why.
        store.adoptCommandOutcomes([
            id: CommandOutcome(
                status: "rejected", resolution: "the running turn is no longer waiting on this question")
        ])
        store.setEntries([question("req-1", resolved: false)])
        XCTAssertNil(store.openInputRequest)
        XCTAssertEqual(
            store.sentOutcomeLine, "No longer waiting: the running turn is no longer waiting on this question")
        // Applied and resolved: nothing left to say.
        store.adoptCommandOutcomes([id: CommandOutcome(status: "applied")])
        store.setEntries([question("req-1", resolved: true)])
        XCTAssertEqual(store.sentAnswerOutcome, .done)
        XCTAssertNil(store.sentOutcomeLine)
    }

    func testAStopShowsItsHostOutcomeUntilTheNextMessage() throws {
        let store = makeStore()
        XCTAssertTrue(store.sendInterrupt())
        let id = try XCTUnwrap(sentCommandId(store, kind: "interrupt"))
        XCTAssertNil(store.sentOutcomeLine, "Stopping… waits out the quiet window")
        store.adoptCommandOutcomes([id: CommandOutcome(status: "rejected", resolution: "chat not hosted here")])
        XCTAssertEqual(store.sentOutcomeLine, "Didn't stop: chat not hosted here")
        store.adoptCommandOutcomes([id: CommandOutcome(status: "applied", resolution: "no live run")])
        XCTAssertNil(store.sentOutcomeLine)
        store.adoptCommandOutcomes([id: CommandOutcome(status: "expired")])
        XCTAssertEqual(store.sentOutcomeLine, "Didn't stop: the Stop expired before the session took it")
        XCTAssertTrue(store.sendSteer(prompt: "go on"))
        XCTAssertNil(store.sentOutcomeLine, "A new message ends the line")
    }
}

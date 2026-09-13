import XCTest
import Loro
@testable import Cypher

@MainActor
final class SteerBubbleTests: XCTestCase {
    private func entry(_ id: String, role: MessageRole = .user, steer: Bool = false) -> MessageEntry {
        MessageEntry(id: id, role: role, parts: [.text(id: "text", text: "Continue")],
                     createdAt: 10, deviceId: "host", status: .complete, isSteer: steer)
    }

    private func rows(_ entries: [MessageEntry], pending: [PendingSend] = []) -> [TranscriptRow] {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        return TranscriptRowBuilder.rows(entries: entries, pendingSends: pending,
                                         parsers: &parsers, completed: &completed)
    }

    private func message(_ doc: LoroDoc, id: String, role: String = "user") throws {
        let map = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try map.insert(key: "id", v: id)
        try map.insert(key: "role", v: role)
        try map.insert(key: "parts", v: LoroValue.fromJSON([
            ["id": "text", "kind": "text", "text": "Continue"]
        ]))
    }

    private func command(_ doc: LoroDoc, id: String?, kind: String = "steer",
                         payloadKind: String = "steer", status: String = "applied") throws {
        let map = try doc.getList(id: "commands").pushContainer(child: LoroMap())
        try map.insert(key: "kind", v: kind)
        try map.insert(key: "status", v: status)
        var payload: [String: Any] = ["kind": payloadKind]
        if let id { payload["messageId"] = id }
        try map.insert(key: "payload", v: LoroValue.fromJSON(payload))
    }

    func testExplicitSteerIntentSurvivesSnapshotReopenAndAnotherDevice() throws {
        let doc = LoroDoc()
        try message(doc, id: "ordinary")
        try message(doc, id: "steer")
        try command(doc, id: "steer")
        doc.commit()
        let reopened = LoroDoc()
        try reopened.importWith(bytes: doc.export(mode: .snapshot), origin: "remote")
        let decoded = try XCTUnwrap(SessionStore.decodeEntries(from: reopened))
        XCTAssertEqual(decoded.map(\.isSteer), [false, true])
        XCTAssertEqual(decoded[1].status, nil, "Command application is not an agent receipt")
    }

    func testLegacyMalformedOrUnrelatedCommandsCannotGuessAMessageIsSteer() throws {
        let doc = LoroDoc()
        try message(doc, id: "legacy")
        try message(doc, id: "normal")
        try message(doc, id: "mismatch")
        try message(doc, id: "assistant", role: "assistant")
        try command(doc, id: nil)
        try command(doc, id: "")
        try command(doc, id: "normal", kind: "run", payloadKind: "run")
        try command(doc, id: "mismatch", payloadKind: "run")
        try command(doc, id: "assistant")
        doc.commit()
        let decoded = try XCTUnwrap(SessionStore.decodeEntries(from: doc))
        XCTAssertEqual(decoded.map(\.isSteer), [false, false, false, false])
    }

    func testSteerLabelDescribesIntentNotCommandReceiptOrExecutionOutcome() throws {
        let doc = LoroDoc()
        for status in ["pending", "applied", "expired", "failed"] {
            try message(doc, id: status)
            try command(doc, id: status, status: status)
        }
        doc.commit()
        XCTAssertTrue(try XCTUnwrap(SessionStore.decodeEntries(from: doc)).allSatisfy(\.isSteer))
    }

    func testSteerUsesSameTurnGapButNormalMessageStillStartsANewExchange() throws {
        let preceding = entry("reply", role: .assistant)
        let ordinary = try XCTUnwrap(rows([preceding, entry("user")]).last)
        let steer = try XCTUnwrap(rows([preceding, entry("user", steer: true)]).last)
        XCTAssertEqual(ordinary.topGap, 36)
        XCTAssertEqual(steer.topGap, 14)
        XCTAssertEqual(ordinary.id, steer.id, "Presentation changes must not replace row identity")
        XCTAssertNotEqual(ordinary.version, steer.version, "Late command sync must refresh the presentation")
        XCTAssertEqual(rows([entry("first", steer: true)]).first?.topGap, TranscriptView.gapTurn + 10)
    }

    func testOptimisticEchoAndMaterializedSteerKeepIdentityStyleAndGap() throws {
        let pending = PendingSend(messageId: "steer", text: "Continue", at: 10, isSteer: true)
        let preceding = entry("reply", role: .assistant)
        let optimistic = try XCTUnwrap(rows([preceding], pending: [pending]).last)
        let materialized = rows([preceding, entry("steer", steer: true)], pending: [pending])
        let confirmed = try XCTUnwrap(materialized.last)
        XCTAssertEqual(materialized.count, 2, "Do not render both the echo and host message")
        XCTAssertEqual(optimistic.id, confirmed.id)
        XCTAssertEqual(optimistic.topGap, 14)
        XCTAssertEqual(confirmed.topGap, 14)
        XCTAssertNil(optimistic.timestamp)
        XCTAssertNotNil(confirmed.timestamp)
        for row in [optimistic, confirmed] {
            guard case .user(let text, let isSteer) = row.kind else { return XCTFail("Missing bubble") }
            XCTAssertEqual(text, "Continue")
            XCTAssertTrue(isSteer)
        }
        let normal = try XCTUnwrap(rows([preceding], pending: [
            PendingSend(messageId: "normal", text: "Continue", at: 11)
        ]).last)
        XCTAssertEqual(normal.topGap, 36)
    }

    func testDemoSendPathAlsoKeepsExplicitSteerIntent() {
        let store = SessionStore(chatId: "demo", config: AppConfig(
            edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "phone", deviceName: "Phone"), offline: true)
        var received: (String, Bool)?
        store.demoResponder = { received = ($0, $1) }
        XCTAssertTrue(store.sendSteer(prompt: "Continue"))
        XCTAssertEqual(received?.0, "Continue")
        XCTAssertEqual(received?.1, true)
        XCTAssertTrue(store.pendingSends.isEmpty)
    }
}

import Loro
import XCTest
@testable import Cypher

/// A session loads from its end: the store reads one page of messages and
/// pages back as the reader scrolls (SessionStore's window).
@MainActor
final class SessionWindowTests: XCTestCase {
    private let open = SessionStore.openMessages
    private let page = SessionStore.pageMessages

    private func config() -> AppConfig {
        AppConfig(
            edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Phone")
    }

    private func entry(_ id: String, role: MessageRole = .user, continuationOf: String? = nil) -> MessageEntry {
        MessageEntry(
            id: id, role: role, parts: [.text(id: "t", text: id)], createdAt: 1, deviceId: "d",
            status: .complete, continuationOf: continuationOf)
    }

    private func push(_ entry: MessageEntry, to doc: LoroDoc) throws {
        let map = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try map.insert(key: "id", v: entry.id)
        try map.insert(key: "role", v: entry.role.rawValue)
        if let root = entry.continuationOf { try map.insert(key: "continuationOf", v: root) }
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        let part = try parts.pushContainer(child: LoroMap())
        try part.insert(key: "id", v: "t")
        try part.insert(key: "kind", v: "text")
        try part.insert(key: "text", v: entry.id)
        doc.commit()
    }

    func testAWindowOpensOnTheLastFewMessages() {
        let doc = BenchRunner.buildDoc(turns: 120)  // 240 messages
        let read = SessionStore.readWindow(of: doc, held: nil)
        XCTAssertEqual(read.start, 240 - open)
        XCTAssertEqual(read.entries.count, open)
        XCTAssertEqual(read.entries.first?.id, "a117")
        XCTAssertEqual(read.entries.last?.id, "a119")
    }

    func testAHeldWindowKeepsItsStartAsMessagesArrive() throws {
        let doc = BenchRunner.buildDoc(turns: 120)
        XCTAssertEqual(SessionStore.readWindow(of: doc, held: 160).entries.count, 80)
        try push(entry("u120"), to: doc)
        let read = SessionStore.readWindow(of: doc, held: 160)
        XCTAssertEqual(read.start, 160)
        XCTAssertEqual(read.entries.map(\.id).suffix(2), ["a119", "u120"])
    }

    func testAWindowNeverHoldsFewerThanItOpensWith() {
        XCTAssertEqual(SessionStore.windowStart(held: nil, count: open - 1), 0)
        XCTAssertEqual(SessionStore.windowStart(held: 10, count: 100), 10)
        // Messages rewound from under the window's end: back to the last few.
        XCTAssertEqual(SessionStore.windowStart(held: 98, count: 100), 100 - open)
        // From under its start: the last few again.
        XCTAssertEqual(SessionStore.windowStart(held: 230, count: 100), 100 - open)
    }

    func testAContinuationReachesBackToItsRoot() throws {
        let doc = LoroDoc()
        try push(entry("root", role: .assistant), to: doc)
        for ix in 0..<open + 5 { try push(entry("u\(ix)"), to: doc) }
        try push(entry("more", role: .assistant, continuationOf: "root"), to: doc)
        let read = SessionStore.readWindow(of: doc, held: nil)
        XCTAssertEqual(read.start, 0)
        XCTAssertEqual(read.entries.first?.id, "root")
        XCTAssertEqual(read.entries.first?.parts.count, 2, "The continuation's parts join the root")
        XCTAssertFalse(read.entries.contains { $0.id == "more" })
    }

    func testAFedStorePagesBackWhileHeldAndFollowsTheTailOnRelease() {
        let store = SessionStore(chatId: "fed", config: config(), offline: true)
        store.setEntries(BenchRunner.syntheticEntries(turns: 120))
        XCTAssertEqual(store.entries.count, open)
        XCTAssertTrue(store.hasEarlier)

        store.loadEarlier()
        XCTAssertEqual(store.entries.count, open, "Unheld, the window follows the tail")

        store.holdWindow()
        store.loadEarlier()  // fed: lands at once
        XCTAssertEqual(store.entries.count, open + page)
        while store.hasEarlier { store.loadEarlier() }
        XCTAssertEqual(store.entries.count, 240)
        XCTAssertEqual(store.entries.first?.id, "u0")

        store.setEntries(store.fedEntries + [entry("u120")])
        XCTAssertEqual(store.entries.count, 241, "A held window grows at the tail")

        store.releaseWindow()
        XCTAssertEqual(store.entries.count, open)
        XCTAssertEqual(store.entries.last?.id, "u120")
        XCTAssertTrue(store.hasEarlier)
    }

    func testADocStoreHydratesItsLastFewMessagesParsedOffTheMainThread() async throws {
        let chatId = "window-\(UUID().uuidString.lowercased())"
        DocDisk.saveChat2(doc: BenchRunner.buildDoc(turns: 120), id: chatId, cursor: 0)
        let store = SessionStore(chatId: chatId, config: config())
        defer {
            store.stop()
            DocDisk.finishWrites()
            try? FileManager.default.removeItem(at: DocDisk.chat2URL(for: chatId))
        }
        store.start()
        try await until { !store.entries.isEmpty }
        XCTAssertEqual(store.entries.count, open)
        XCTAssertEqual(store.entries.first?.id, "a117")
        XCTAssertTrue(store.hasEarlier)
        XCTAssertNotNil(
            store.transcriptCache.settledParses["a117#t0"], "The window's markdown is parsed before it lands")
        // The scrubber's rounds above the window arrive after it.
        try await until { store.earlierRounds.count == 118 }
        XCTAssertEqual(store.earlierRounds.first?.rowId, "u0")
        XCTAssertEqual(store.earlierRounds.last?.messageIndex, 234)

        store.holdWindow()
        store.loadEarlier()
        store.loadEarlier()  // a page is on its way: no second one
        try await until { store.entries.count == open + page }
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(store.entries.count, open + page)
        XCTAssertEqual(store.entries.first?.id, "a97")
        XCTAssertNotNil(store.transcriptCache.settledParses["a97#t0"])
        XCTAssertEqual(store.earlierRounds.count, 98)

        store.loadEarlier(through: 10)  // a scrubber jump to round 6
        try await until { store.entries.first?.id == "u5" }
        XCTAssertEqual(store.earlierRounds.map(\.rowId), ["u0", "u1", "u2", "u3", "u4"])
        store.releaseWindow()
        try await until { store.entries.count == open }
        try await until { store.earlierRounds.count == 118 }
    }

    private func until(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async throws {
        for _ in 0..<250 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTFail("Timed out", file: file, line: line)
    }
}

import XCTest
@testable import Cypher

/// Temporary side chats: the transcript feed, the direct transport and the
/// promoted title.
@MainActor
final class SideChatTests: XCTestCase {
    func testTranscriptFeedAppliesDeltasLikeTheEngine() throws {
        func entry(_ id: String, _ text: String) -> [String: Any] {
            ["id": id, "role": "assistant", "createdAt": 1, "deviceId": "d",
             "parts": [["kind": "text", "id": "t0", "text": text]]]
        }
        var feed = TranscriptFeed()
        try feed.apply(["reset": [entry("a", "hello")]])
        try feed.apply(["upsert": [["after": "a", "entry": entry("b", "你")]], "count": 2])
        // `len` is the part's UTF-8 byte length (Rust String::len).
        try feed.apply(["append": [["entry": "b", "part": "t0", "text": "好", "len": 6]], "count": 2])
        XCTAssertEqual(feed.messages.map(\.id), ["a", "b"])
        guard case .text(_, let text, _) = feed.messages[1].parts[0] else { return XCTFail() }
        XCTAssertEqual(text, "你好")
        try feed.apply(["upsert": [["entry": entry("z", "head")]], "remove": ["a"], "count": 2])
        XCTAssertEqual(feed.messages.map(\.id), ["z", "b"])

        XCTAssertThrowsError(try feed.apply(["append": [["entry": "b", "part": "t0", "text": "!", "len": 99]],
                                             "count": 2]))
        var fresh = TranscriptFeed()
        XCTAssertThrowsError(try fresh.apply(["upsert": [["after": "missing", "entry": entry("c", "")]], "count": 1]))
        XCTAssertThrowsError(try fresh.apply(["count": 3]), "count mismatch")
    }

    func testDirectTransportSendsEchoAndSettle() {
        let store = SessionStore(chatId: "side-1", config: DemoDataset.dummyConfig, offline: true)
        var sent: [(String, String)] = []
        var stops = 0
        store.directTransport = SessionStore.DirectTransport(
            send: { prompt, id in sent.append((prompt, id)); return true },
            interrupt: { stops += 1; return true },
            respondInput: { _, _ in true })
        let chat = Chat(id: "side-1", deviceId: "d", title: nil, archived: false, cwd: "/w", branch: nil,
                        checkoutId: nil, config: nil, lastMessagePreview: nil, lastMessageAt: nil,
                        createdAt: 0, spaceId: nil, lastSeenAt: nil)
        XCTAssertTrue(store.sendRun(prompt: "why?", chat: chat))
        XCTAssertTrue(store.sendInterrupt())
        XCTAssertEqual(sent.map(\.0), ["why?"])
        XCTAssertEqual(stops, 1)
        XCTAssertEqual(store.pendingSends.map(\.messageId), [sent[0].1])
        store.setEntries([.fixture(sent[0].1, role: .user, parts: [.text(id: "t", text: "why?")])])
        XCTAssertTrue(store.pendingSends.isEmpty, "the host's entry retires the echo")
    }

    func testPromotedSideChatTitle() {
        XCTAssertEqual(SideChatStore.promotedTitle("  why does the veil fade twice on reopen here  "),
                       "why does the veil fade")
        XCTAssertEqual(SideChatStore.promotedTitle(""), "Side chat")
    }
}

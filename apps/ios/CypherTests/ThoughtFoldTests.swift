import XCTest
import Loro
@testable import Cypher

/// The model's thinking syncs as a `reasoning` part and folds behind a
/// "Thinking…"/"Thought" toggle, collapsed by default — transcript.rs
/// `a_thought_*` tests, ported.
@MainActor
final class ThoughtFoldTests: XCTestCase {
    private func entry(_ parts: [MessagePart], status: MessageStatus = .complete) -> MessageEntry {
        MessageEntry(id: "m1", role: .assistant, parts: parts, createdAt: 7, deviceId: "d",
                     status: status, continuationOf: nil)
    }

    private func rows(_ entries: [MessageEntry]) -> [TranscriptRow] {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        return TranscriptRowBuilder.rows(entries: entries, pendingSends: [],
                                         parsers: &parsers, completed: &completed)
    }

    func testTheDocsReasoningPartDecodesFromItsOwnKey() throws {
        // The shape the desktop engine writes: a streamed LoroText under
        // `reasoning`, and no `text`.
        let doc = LoroDoc()
        let message = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try message.insert(key: "id", v: "m1")
        try message.insert(key: "role", v: "assistant")
        try message.insert(key: "status", v: "complete")
        let parts = try message.insertContainer(key: "parts", child: LoroList())
        let part = try parts.pushContainer(child: LoroMap())
        try part.insert(key: "id", v: "r0")
        try part.insert(key: "kind", v: "reasoning")
        let thought = try part.insertContainer(key: "reasoning", child: LoroText())
        try thought.insert(pos: 0, s: "Weigh the options.")
        doc.commit()
        let entries = try XCTUnwrap(SessionStore.decodeEntries(from: doc))
        XCTAssertEqual(entries[0].parts, [.reasoning(id: "r0", text: "Weigh the options.")])
    }

    func testAThoughtFoldsBehindACollapsedToggle() {
        let built = rows([entry([.reasoning(id: "r0", text: "Plan.\n\n```sh\nls\n```"),
                                 .text(id: "t1", text: "Done.")])])
        XCTAssertEqual(built.map(\.id), ["m1#r0.thought", "m1#r0.0", "m1#r0.1", "m1#t1.0"])
        guard case .thought(let hidden, let live) = built[0].kind else {
            return XCTFail("the part opens on the toggle")
        }
        XCTAssertEqual(hidden, 2)
        XCTAssertFalse(live)
        XCTAssertTrue(built[0].turnStart)
        XCTAssertEqual(built.map(\.muted), [false, true, true, false])

        let folded = TranscriptRowBuilder.foldClosedToggles(built, open: [])
        XCTAssertEqual(folded.map(\.id), ["m1#r0.thought", "m1#t1.0"])
        XCTAssertEqual(folded[1].topGap, TranscriptView.gapBlock)
        let open = TranscriptRowBuilder.foldClosedToggles(built, open: ["m1#r0.thought"])
        XCTAssertEqual(open.map(\.id), built.map(\.id))
    }

    func testAThoughtIsLiveOnlyWhileItIsTheStreamingTail() {
        let thinking = rows([entry([.reasoning(id: "r0", text: "Hmm")], status: .streaming)])
        guard case .thought(_, true) = thinking[0].kind else { return XCTFail("live toggle") }
        let answering = rows([entry([.reasoning(id: "r0", text: "Hmm"), .text(id: "t1", text: "So")],
                                    status: .streaming)])
        guard case .thought(_, false) = answering[0].kind else { return XCTFail("settled toggle") }
        XCTAssertNotEqual(thinking[0].version, answering[0].version)
    }

    func testAReplyThatEndedThinkingKeepsItsTimestampOnTheToggle() {
        let built = rows([entry([.text(id: "t0", text: "Partial"), .reasoning(id: "r1", text: "Then…")],
                                status: .aborted)])
        XCTAssertEqual(built.last?.timestamp, 7)
        let folded = TranscriptRowBuilder.foldClosedToggles(built, open: [])
        XCTAssertEqual(folded.map(\.id), ["m1#t0.0", "m1#r1.thought"])
        XCTAssertEqual(folded.last?.timestamp, 7)
        XCTAssertNotEqual(folded.last?.version, built[1].version)
    }

    func testEmptyThinkingAddsNoRows() {
        XCTAssertTrue(rows([entry([.reasoning(id: "r0", text: "  \n")])]).isEmpty)
    }
}

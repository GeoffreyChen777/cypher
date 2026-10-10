import XCTest
import Loro
@testable import Cypher

/// The model's thinking syncs as a `reasoning` part and folds behind a
/// "Thinking…"/"Thought" toggle, collapsed by default — transcript.rs
/// `a_thought_*` tests, ported.
@MainActor
final class ThoughtFoldTests: XCTestCase {
    private func entry(_ parts: [MessagePart], status: MessageStatus = .complete) -> MessageEntry {
        .fixture("m1", parts: parts, createdAt: 7, status: status)
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
        let built = buildRows([
            entry([
                .reasoning(id: "r0", text: "Plan.\n\n```sh\nls\n```"),
                .text(id: "t1", text: "Done."),
            ])
        ])
        XCTAssertEqual(built.map(\.id), ["m1#r0.thought", "m1#r0.0", "m1#r0.1", "m1#t1.0"])
        guard case .thought(let hidden, let live, _) = built[0].kind else {
            return XCTFail("the part opens on the toggle")
        }
        XCTAssertEqual(hidden, 2)
        XCTAssertFalse(live)
        XCTAssertTrue(built[0].turnStart)
        XCTAssertEqual(built.map(\.muted), [false, true, true, false])

        let folded = TranscriptRowBuilder.foldClosedToggles(built, pins: [:])
        XCTAssertEqual(folded.map(\.id), ["m1#r0.thought", "m1#t1.0"])
        XCTAssertEqual(folded[1].topGap, TranscriptView.gapBlock)
        let open = TranscriptRowBuilder.foldClosedToggles(built, pins: ["m1#r0.thought": true])
        XCTAssertEqual(open.map(\.id), built.map(\.id))
    }

    func testAThoughtIsLiveOnlyWhileItIsTheStreamingTail() {
        let thinking = buildRows([entry([.reasoning(id: "r0", text: "Hmm")], status: .streaming)])
        guard case .thought(_, true, _) = thinking[0].kind else { return XCTFail("live toggle") }
        let answering = buildRows([
            entry(
                [.reasoning(id: "r0", text: "Hmm"), .text(id: "t1", text: "So")],
                status: .streaming)
        ])
        guard case .thought(_, false, _) = answering[0].kind else { return XCTFail("settled toggle") }
        XCTAssertNotEqual(thinking[0].version, answering[0].version)
    }

    func testAReplyThatEndedThinkingKeepsItsTimestampOnTheToggle() {
        let built = buildRows([
            entry(
                [.text(id: "t0", text: "Partial"), .reasoning(id: "r1", text: "Then…")],
                status: .aborted)
        ])
        XCTAssertEqual(built.last?.timestamp, 7)
        let folded = TranscriptRowBuilder.foldClosedToggles(built, pins: [:])
        XCTAssertEqual(folded.map(\.id), ["m1#t0.0", "m1#r1.thought"])
        XCTAssertEqual(folded.last?.timestamp, 7)
        XCTAssertNotEqual(folded.last?.version, built[1].version)
    }

    func testEmptyThinkingAddsNoRows() {
        XCTAssertTrue(buildRows([entry([.reasoning(id: "r0", text: "  \n")])]).isEmpty)
    }

    // MARK: Work runs — transcript.rs `*work_run*` tests, ported.

    private func exec(_ id: String, _ command: String) -> MessagePart {
        .tool(
            id: id, call: RenderToolCall(tag: "exec", fields: ["command": command]),
            isError: false, resolved: true)
    }

    func testThinkingBetweenToolCallsFoldsIntoOneWorkRun() {
        let built = buildRows([
            entry([
                .reasoning(id: "r0", text: "**Look around**\n\nList the files."),
                exec("x1", "ls"),
                exec("x2", "git status"),
                .reasoning(id: "r3", text: "Now build."),
                exec("x4", "cargo build"),
                .reasoning(id: "r5", text: "It built."),
                .text(id: "t6", text: "Done."),
            ])
        ])
        XCTAssertEqual(
            built.map(\.id),
            [
                "m1#r0.activity", "m1#r0.thought", "m1#r0.0", "m1#g0",
                "m1#r3.thought", "m1#r3.0", "m1#g1",
                "m1#r5.thought", "m1#r5.0", "m1#t6.0",
            ])
        guard case .activity(let covered, let summary, let autoOpen) = built[0].kind else {
            return XCTFail("the run opens on its toggle")
        }
        XCTAssertEqual(covered, 8)
        XCTAssertEqual(summary, "Ran 3 commands · 3 thoughts")
        XCTAssertFalse(autoOpen)
        XCTAssertTrue(built[0].turnStart)
        XCTAssertFalse(built[1].turnStart)
        XCTAssertEqual(built.map(\.nested), [false] + Array(repeating: true, count: 8) + [false])
        guard case .thought(_, _, let preview) = built[1].kind else { return XCTFail("a thought") }
        XCTAssertEqual(preview, "Look around")

        // Closed, the whole run is one row above the answer.
        let folded = TranscriptRowBuilder.foldClosedToggles(built, pins: [:])
        XCTAssertEqual(folded.map(\.id), ["m1#r0.activity", "m1#t6.0"])
        XCTAssertEqual(folded[1].topGap, TranscriptView.gapBlock)

        // Open, the run reads in order: chips, and thoughts still folded.
        let open = TranscriptRowBuilder.foldClosedToggles(built, pins: ["m1#r0.activity": true])
        XCTAssertEqual(
            open.map(\.id),
            [
                "m1#r0.activity", "m1#r0.thought", "m1#g0", "m1#r3.thought",
                "m1#g1", "m1#r5.thought", "m1#t6.0",
            ])
        XCTAssertEqual(open[1].topGap, 2)
        XCTAssertEqual(open[2].topGap, 0)
    }

    func testAWorkRunIsOpenWhileItStreamsAndClosesWhenTheAnswerStarts() {
        let working = buildRows([
            entry(
                [exec("x0", "ls"), .reasoning(id: "r1", text: "Hmm")],
                status: .streaming)
        ])
        guard case .activity(_, _, true) = working[0].kind else { return XCTFail("open while live") }
        guard case .thought(_, true, _) = working[2].kind else { return XCTFail("live thought") }
        XCTAssertEqual(TranscriptRowBuilder.foldClosedToggles(working, pins: [:]).count, 3)
        // A tap pins it closed.
        XCTAssertEqual(
            TranscriptRowBuilder.foldClosedToggles(working, pins: ["m1#x0.activity": false])
                .map(\.id), ["m1#x0.activity"])

        let answering = buildRows([
            entry(
                [
                    exec("x0", "ls"), .reasoning(id: "r1", text: "Hmm"),
                    .text(id: "t2", text: "So"),
                ], status: .streaming)
        ])
        XCTAssertEqual(
            TranscriptRowBuilder.foldClosedToggles(answering, pins: [:]).map(\.id),
            ["m1#x0.activity", "m1#t2.0"])
    }

    func testOnlyARunThatMixesToolsAndThinkingFolds() {
        // Tools alone: one group, and an empty text part splits nothing.
        let tools = buildRows([
            entry([
                exec("x0", "ls"), .text(id: "t1", text: "  "), exec("x2", "pwd"),
                .text(id: "t3", text: "Done."),
            ])
        ])
        XCTAssertEqual(tools.map(\.id), ["m1#g0", "m1#t3.0"])
        guard case .toolGroup(let group, _) = tools[0].kind else { return XCTFail("a tool group") }
        XCTAssertEqual(group.count, 2)
        XCTAssertFalse(tools[0].nested)

        // Answer text between a thought and the tools: separate runs.
        let split = buildRows([
            entry([
                .reasoning(id: "r0", text: "Plan"), .text(id: "t1", text: "Let me look."),
                exec("x2", "ls"), .text(id: "t3", text: "Done."),
            ])
        ])
        XCTAssertEqual(
            TranscriptRowBuilder.foldClosedToggles(split, pins: [:]).map(\.id),
            ["m1#r0.thought", "m1#t1.0", "m1#g0", "m1#t3.0"])
    }

    func testThoughtPreviewsDropTitleMarkup() {
        XCTAssertEqual(TranscriptRowBuilder.thoughtPreview("**Planning the fix**\n\nFirst…"), "Planning the fix")
        XCTAssertEqual(TranscriptRowBuilder.thoughtPreview("\n## Heading\nbody"), "Heading")
        XCTAssertEqual(TranscriptRowBuilder.thoughtPreview("Check `__init__` first"), "Check `__init__` first")
        XCTAssertEqual(TranscriptRowBuilder.thoughtPreview("_Weighing options_"), "Weighing options")
        XCTAssertEqual(TranscriptRowBuilder.thoughtPreview(""), "")
    }
}

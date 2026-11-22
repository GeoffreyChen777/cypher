import SwiftUI
import XCTest
@testable import Cypher

/// An open tool group shows its last five calls and folds the earlier ones
/// behind one row; a work run caps its calls as a whole. transcript.rs
/// `tool_group_cap_keeps_the_last_calls` and
/// `the_tool_call_cap_folds_the_start_of_an_open_work_run`, ported.
@MainActor
final class ToolCallCapTests: XCTestCase {
    private func exec(_ id: String, _ command: String) -> MessagePart {
        .tool(
            id: id, call: RenderToolCall(tag: "exec", fields: ["command": command]),
            isError: false, resolved: true)
    }

    private func item(_ n: Int) -> ToolItem {
        ToolItem(
            id: "x\(n)", call: RenderToolCall(tag: "exec", fields: ["command": "step \(n)"]),
            isError: false, resolved: true)
    }

    /// Three thoughts, each leading into three calls, then the answer.
    private var workRun: MessageEntry {
        .fixture(
            "m1",
            parts: [
                .reasoning(id: "r0", text: "Look around."),
                exec("x1", "ls"), exec("x2", "git status"), exec("x3", "git log"),
                .reasoning(id: "r4", text: "Build it."),
                exec("x5", "cargo build"), exec("x6", "cargo test"), exec("x7", "cargo clippy"),
                .reasoning(id: "r8", text: "Ship it."),
                exec("x9", "git add ."), exec("x10", "git commit"), exec("x11", "git push"),
                .text(id: "t12", text: "Done."),
            ], createdAt: 7)
    }

    private func openRun() -> [TranscriptRow] {
        TranscriptRowBuilder.foldClosedToggles(buildRows([workRun]), pins: ["m1#r0.activity": true])
    }

    func testTheCapKeepsTheLastCalls() {
        XCTAssertEqual(ToolCallCap.limit, 5)
        // Under the cap nothing folds; over it, the last five stay.
        XCTAssertEqual(ToolCallCap.hidden(total: 5, revealed: false), 0)
        XCTAssertEqual(ToolCallCap.hidden(total: 12, revealed: false), 7)
        // Strict: one call over the cap folds too.
        XCTAssertEqual(ToolCallCap.hidden(total: 6, revealed: false), 1)
        XCTAssertEqual(ToolCallCap.hidden(total: 7, revealed: false), 2)
        // A revealed group and no cap hide nothing.
        XCTAssertEqual(ToolCallCap.hidden(total: 12, revealed: true), 0)
        XCTAssertEqual(ToolCallCap.hidden(total: 12, limit: 0, revealed: false), 0)
        // The row is there while the group is over the cap, revealed or not.
        XCTAssertFalse(ToolCallCap.overflows(total: 5))
        XCTAssertTrue(ToolCallCap.overflows(total: 6))
        XCTAssertFalse(ToolCallCap.overflows(total: 12, limit: 0))

        XCTAssertEqual(ToolCallCap.label(tools: 7), "Show 7 earlier tool calls")
        XCTAssertEqual(ToolCallCap.label(tools: 1), "Show 1 earlier tool call")
        XCTAssertEqual(ToolCallCap.label(tools: 4, thoughts: 2), "Show 4 earlier tool calls and 2 thoughts")
        XCTAssertEqual(ToolCallCap.label(tools: 1, thoughts: 1), "Show 1 earlier tool call and 1 thought")
        XCTAssertEqual(ToolCallCap.label(tools: 0), "Show fewer tool calls")
        XCTAssertEqual(ToolCallCap.label(tools: 0, thoughts: 3), "Show fewer tool calls")
    }

    func testAGroupCapsItselfAndAGroupInARunFollowsItsRun() throws {
        let built = buildRows([.fixture("m1", parts: (0..<12).map { exec("x\($0)", "step \($0)") })])
        XCTAssertEqual(built.map(\.id), ["m1#g0"])
        let group = built[0]
        XCTAssertEqual(group.skip, 0)
        // Twelve calls: seven fold away under the row, which stays once the
        // reader shows them.
        var fold = ToolCallCap.fold(of: group, total: 12, revealed: false)
        XCTAssertEqual(fold.hidden, 7)
        XCTAssertTrue(fold.overflows)
        fold = ToolCallCap.fold(of: group, total: 12, revealed: true)
        XCTAssertEqual(fold.hidden, 0)
        XCTAssertTrue(fold.overflows)
        // Five fit: no row.
        fold = ToolCallCap.fold(of: group, total: 5, revealed: false)
        XCTAssertEqual(fold.hidden, 0)
        XCTAssertFalse(fold.overflows)

        // In a run the run's cap decides, and the run draws the row.
        let capped = TranscriptRowBuilder.capWorkRuns(openRun(), revealed: [])
        let inRun = try XCTUnwrap(capped.first { $0.id == "m1#g1" })
        fold = ToolCallCap.fold(of: inRun, total: 3, revealed: false)
        XCTAssertEqual(fold.hidden, 1)
        XCTAssertFalse(fold.overflows)
    }

    func testTheCapFoldsTheStartOfAnOpenWorkRun() {
        let open = openRun()
        XCTAssertEqual(
            open.map(\.id),
            [
                "m1#r0.activity", "m1#r0.thought", "m1#g0", "m1#r4.thought", "m1#g1",
                "m1#r8.thought", "m1#g2", "m1#t12.0",
            ])

        // Nine calls, five kept: the cut falls inside the second group, which
        // keeps its last two chips.
        var capped = TranscriptRowBuilder.capWorkRuns(open, limit: 5, revealed: [])
        XCTAssertEqual(
            capped.map(\.id),
            ["m1#r0.activity", "m1#r0.activity.overflow", "m1#g1", "m1#r8.thought", "m1#g2", "m1#t12.0"])
        guard case .runOverflow(let run, let tools, let thoughts) = capped[1].kind else {
            return XCTFail("the run's overflow row")
        }
        XCTAssertEqual(run, "m1#r0.activity")
        XCTAssertEqual(tools, 4)
        XCTAssertEqual(thoughts, 2)
        XCTAssertEqual(capped[2].skip, 1)
        XCTAssertNotEqual(capped[2].version, open[4].version)
        XCTAssertEqual(capped[4].skip, 0)
        // With the run's chips: nested, under the toggle, the group flush
        // below it; the answer keeps its gap.
        XCTAssertTrue(capped[1].nested)
        XCTAssertFalse(capped[1].turnStart)
        XCTAssertNil(capped[1].timestamp)
        XCTAssertEqual(capped[1].entryId, "m1")
        XCTAssertEqual(capped[1].topGap, 2)
        XCTAssertEqual(capped[2].topGap, 0)
        XCTAssertEqual(capped[5].topGap, open[7].topGap)
        XCTAssertEqual(capped[5].timestamp, 7)

        // A cut between groups keeps the thinking that led into the first
        // kept call.
        capped = TranscriptRowBuilder.capWorkRuns(open, limit: 6, revealed: [])
        XCTAssertEqual(
            capped.map(\.id),
            [
                "m1#r0.activity", "m1#r0.activity.overflow", "m1#r4.thought", "m1#g1",
                "m1#r8.thought", "m1#g2", "m1#t12.0",
            ])
        XCTAssertEqual(capped[3].skip, 0)
        guard case .runOverflow(_, 3, 1) = capped[1].kind else { return XCTFail("three calls, one thought") }

        // Revealed, the run shows whole under a row that folds it back.
        let revealed = TranscriptRowBuilder.capWorkRuns(open, limit: 5, revealed: ["m1#r0.activity"])
        XCTAssertEqual(revealed.count, open.count + 1)
        guard case .runOverflow(_, 0, _) = revealed[1].kind else { return XCTFail("the row that folds it back") }
        XCTAssertEqual(Array(revealed.map(\.id)[2...]), Array(open.map(\.id)[1...]))
        XCTAssertEqual(revealed[2].topGap, 0)
        XCTAssertTrue(revealed.allSatisfy { $0.skip == 0 })
        XCTAssertNotEqual(revealed[1].version, TranscriptRowBuilder.capWorkRuns(open, revealed: [])[1].version)

        // One call over the cap folds too, with the thinking before it.
        capped = TranscriptRowBuilder.capWorkRuns(open, limit: 8, revealed: [])
        XCTAssertEqual(
            capped.map(\.id),
            [
                "m1#r0.activity", "m1#r0.activity.overflow", "m1#g0", "m1#r4.thought", "m1#g1",
                "m1#r8.thought", "m1#g2", "m1#t12.0",
            ])
        XCTAssertEqual(capped[2].skip, 1)
        guard case .runOverflow(_, 1, 1) = capped[1].kind else { return XCTFail("one call, one thought") }

        // No cap, a cap the run fits, or a closed run: untouched.
        for limit in [0, 9, 10] {
            XCTAssertEqual(TranscriptRowBuilder.capWorkRuns(open, limit: limit, revealed: []).map(\.id), open.map(\.id))
        }
        let closed = TranscriptRowBuilder.foldClosedToggles(buildRows([workRun]), pins: [:])
        XCTAssertEqual(closed.map(\.id), ["m1#r0.activity", "m1#t12.0"])
        XCTAssertEqual(TranscriptRowBuilder.capWorkRuns(closed, revealed: []).map(\.id), closed.map(\.id))
    }

    func testAThoughtTheReaderOpenedFoldsAwayWithItsText() {
        // The first thought is open, so its text follows it in the run: both
        // go behind the row, and the count is of thoughts, not rows.
        let open = TranscriptRowBuilder.foldClosedToggles(
            buildRows([workRun]), pins: ["m1#r0.activity": true, "m1#r0.thought": true])
        XCTAssertEqual(Array(open.map(\.id).prefix(4)), ["m1#r0.activity", "m1#r0.thought", "m1#r0.0", "m1#g0"])
        let capped = TranscriptRowBuilder.capWorkRuns(open, revealed: [])
        XCTAssertEqual(
            capped.map(\.id),
            ["m1#r0.activity", "m1#r0.activity.overflow", "m1#g1", "m1#r8.thought", "m1#g2", "m1#t12.0"])
        guard case .runOverflow(_, 4, 2) = capped[1].kind else { return XCTFail("four calls, two thoughts") }
    }

    func testEachRunOfAReplyCapsOnItsOwn() {
        // Two runs with the answer's text between them: only the long one
        // folds, and the rows after it keep their places.
        let entry = MessageEntry.fixture(
            "m1",
            parts: [
                .reasoning(id: "r0", text: "Start."),
                exec("a1", "one"), exec("a2", "two"),
                .text(id: "t3", text: "Halfway."),
                .reasoning(id: "r4", text: "Go on."),
            ] + (0..<7).map { exec("b\($0)", "step \($0)") } + [.text(id: "t9", text: "Done.")])
        let open = TranscriptRowBuilder.foldClosedToggles(
            buildRows([entry]), pins: ["m1#r0.activity": true, "m1#r4.activity": true])
        XCTAssertEqual(
            open.map(\.id),
            [
                "m1#r0.activity", "m1#r0.thought", "m1#g0", "m1#t3.0",
                "m1#r4.activity", "m1#r4.thought", "m1#g1", "m1#t9.0",
            ])
        let capped = TranscriptRowBuilder.capWorkRuns(open, revealed: [])
        XCTAssertEqual(
            capped.map(\.id),
            [
                "m1#r0.activity", "m1#r0.thought", "m1#g0", "m1#t3.0",
                "m1#r4.activity", "m1#r4.activity.overflow", "m1#g1", "m1#t9.0",
            ])
        // Two of the seven fold, and with them the thought that led into
        // them.
        guard case .runOverflow("m1#r4.activity", 2, 1) = capped[5].kind else {
            return XCTFail("two calls and a thought")
        }
        XCTAssertEqual(capped[6].skip, 2)
        XCTAssertEqual(capped[7].topGap, open[7].topGap)
        // Revealing a run that fits its cap changes nothing.
        let revealed = TranscriptRowBuilder.capWorkRuns(open, revealed: ["m1#r0.activity"])
        XCTAssertEqual(revealed.map(\.id), capped.map(\.id))
    }

    func testTheCachedRowsFollowTheRunsTheReaderRevealed() {
        let cache = TranscriptBuilderCache()
        let pins = ["m1#r0.activity": true]
        func rows(_ revealed: Set<String>) -> [String] {
            cache.rows(
                revision: 1, entries: [workRun], pendingSends: [], togglePins: pins, revealedRuns: revealed
            ).map(\.id)
        }
        let capped = rows([])
        XCTAssertEqual(capped.count, 6)
        XCTAssertEqual(capped[1], "m1#r0.activity.overflow")
        // Revealed: the same build, refolded.
        XCTAssertEqual(rows(["m1#r0.activity"]).count, 9)
        // A revealed plain group is not a run: nothing changes.
        XCTAssertEqual(rows(["m1#g7"]), capped)
        XCTAssertEqual(rows([]), capped)
    }

    // MARK: The group's view

    /// The height a tool group takes at a phone's width.
    private func height(_ group: ToolGroupView) -> CGFloat {
        let host = UIHostingController(rootView: group)
        return host.sizeThatFits(in: CGSize(width: 390, height: CGFloat.greatestFiniteMagnitude)).height
    }

    private func group(_ count: Int, hidden: Int = 0, overflows: Bool = false, nested: Bool = false) -> ToolGroupView {
        ToolGroupView(
            tools: (0..<count).map(item), open: true, nested: nested, hidden: hidden, overflows: overflows,
            userToggled: false, openChips: [], toggleChip: { _ in }, toggle: {})
    }

    func testACappedGroupDrawsItsLastFiveCallsUnderOneRow() {
        // 38pt a chip, 30pt the row, whatever the header takes.
        let chip = height(group(2)) - height(group(1))
        XCTAssertEqual(chip, 38, accuracy: 0.5)
        let header = height(group(1)) - chip
        let five = height(group(5))
        XCTAssertEqual(five, header + 5 * chip, accuracy: 0.5)

        // Twelve calls take the room of five and the row, however long the
        // run was.
        let capped = height(group(12, hidden: 7, overflows: true))
        XCTAssertEqual(capped, five + 30, accuracy: 0.5)
        XCTAssertEqual(height(group(40, hidden: 35, overflows: true)), capped, accuracy: 0.5)
        // Revealed: every call, and the row stays to fold them back.
        XCTAssertEqual(height(group(12, overflows: true)), header + 12 * chip + 30, accuracy: 0.5)
        // In a run the group draws its chips only; the run draws the row.
        XCTAssertEqual(height(group(3, hidden: 1, nested: true)), 2 * chip, accuracy: 0.5)
    }
}

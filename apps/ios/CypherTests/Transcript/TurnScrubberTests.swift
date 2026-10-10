import XCTest
@testable import Cypher

final class TurnScrubberTests: XCTestCase {
    private func row(_ id: String, _ kind: RowKind) -> TranscriptRow {
        TranscriptRow(id: id, version: 0, turnStart: true, kind: kind, entryId: id,
                      timestamp: nil, partKey: nil)
    }

    func testRoundsStartAtPromptsAndSteersStayInside() {
        let rows = [
            row("u0", .user(text: "\n  First   prompt\nsecond line")),
            row("a0", .errorChip(message: "x")),
            row("s0", .user(text: "steer", isSteer: true)),
            row("u1", .user(text: "Next")),
        ]
        let rounds = TranscriptRound.rounds(in: rows)
        XCTAssertEqual(rounds.map(\.rowId), ["u0", "u1"])
        XCTAssertEqual(rounds.map(\.rowIndex), [0, 3])
        XCTAssertEqual(rounds[0].preview, "First prompt")
    }

    func testShortSessionsGetATickPerRound() {
        let layout = TickLayout(rounds: 8, maxHeight: 300)
        XCTAssertEqual(layout.ticks, 8)
        XCTAssertEqual(layout.pitch, TurnScrubber.pitchMax)
        for round in 0..<8 {
            XCTAssertEqual(layout.tick(forRound: round), round)
            XCTAssertEqual(layout.round(atY: layout.pitch * (CGFloat(round) + 0.5)), round)
        }
    }

    func testLongSessionsShareTicksButResolveEveryRound() {
        let layout = TickLayout(rounds: 400, maxHeight: 300)
        XCTAssertEqual(layout.ticks, 75)
        XCTAssertEqual(layout.pitch, 4)
        XCTAssertEqual(layout.tick(forRound: 0), 0)
        XCTAssertEqual(layout.tick(forRound: 399), 74)
        XCTAssertEqual(layout.round(atY: -50), 0)
        XCTAssertEqual(layout.round(atY: 10_000), 399)
        let rounds = Set(stride(from: CGFloat(2), through: 298, by: 0.25).map { layout.round(atY: $0) })
        XCTAssertEqual(rounds.count, 400, "A slow drag must pass through every round")
    }

    func testTrackerCountsOnlyPromptsOnScreen() {
        let tracker = TurnTracker()
        tracker.appeared(round: 5)
        tracker.report(round: 5, top: 100, line: 300)
        XCTAssertEqual(tracker.current, 5)
        // Torn down by a jump; its last report arrives late.
        tracker.disappeared(round: 5)
        tracker.report(round: 5, top: 120, line: 300)
        tracker.report(round: 0, top: 36, line: 300)
        tracker.appeared(round: 0)
        XCTAssertEqual(tracker.current, 0)
        // Only a prompt below the line in view: the round before it.
        tracker.report(round: 0, top: -900, line: 300)
        tracker.disappeared(round: 0)
        tracker.appeared(round: 3)
        tracker.report(round: 3, top: 500, line: 300)
        XCTAssertEqual(tracker.current, 2)
    }
}

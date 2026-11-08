import XCTest

@testable import Cypher

/// The working strip's progress bar — kit/progress_bar.rs and
/// transcript/progress.rs tests, ported, and the session row's additive
/// `progress` field.
final class RunProgressTests: XCTestCase {
    private typealias G = ProgressBarGeometry

    func testTheSessionRowsProgressDecodesLeniently() throws {
        let value: JSONValue = .object([
            "percent": .int(42), "current": .string("Port the bar"),
            "next": .array([.string("Test it"), .string("Ship it")]),
            "stepsLeft": .int(4),
            "stages": .array([
                .object(["label": .string("Read"), "percent": .int(10)]),
                .object(["label": .string("Port the bar"), "percent": .int(30)]),
            ]),
            "updatedAt": .string("2026-10-25T09:00:00.123456789Z"),
        ])
        let progress = try XCTUnwrap(RunProgress(value))
        XCTAssertEqual(progress.percent, 42)
        XCTAssertEqual(progress.next, ["Test it", "Ship it"])
        XCTAssertEqual(progress.stepsLeftCount, 4)
        XCTAssertEqual(progress.stages.map(\.percent), [10, 30])
        XCTAssertEqual(progress.note, "42% \u{b7} 4 steps left")
        // Optional parts may be absent; a malformed reading drops only itself.
        let bare = try XCTUnwrap(RunProgress(.object(["percent": .int(5), "current": .string("Start")])))
        XCTAssertEqual(bare.shownStages, [ProgressStage(label: "Start", percent: 5)])
        XCTAssertNil(RunProgress(.object(["current": .string("x")])))
        XCTAssertNil(RunProgress(.object(["percent": .int(300), "current": .string("x")])))
        XCTAssertNil(
            RunProgress(.object(["percent": .int(5), "current": .string("x"), "stages": .array([.int(1)])])))
        XCTAssertNil(RunProgress(nil))
    }

    func testTheFillNeverFallsBehindAStage() {
        var progress = RunProgress(
            percent: 40, current: "s",
            stages: [ProgressStage(label: "s", percent: 10), ProgressStage(label: "s", percent: 30)])
        XCTAssertEqual(progress.shownPercent, 40)
        progress.stages = [ProgressStage(label: "s", percent: 35), ProgressStage(label: "s", percent: 62)]
        progress.percent = 22
        XCTAssertEqual(progress.shownPercent, 62)
    }

    func testUpcomingStepsShareWhatIsLeft() {
        XCTAssertEqual(RunProgress.upcomingPositions(percent: 40, count: 2), [0.6, 0.8])
        XCTAssertTrue(RunProgress.upcomingPositions(percent: 40, count: 0).isEmpty)
        XCTAssertEqual(RunProgress.upcomingPositions(percent: 10, count: 40).count, RunProgress.maxUpcoming)
        XCTAssertEqual(RunProgress.leftNote(0), "last step")
        XCTAssertEqual(RunProgress.leftNote(1), "1 step left")
        XCTAssertEqual(RunProgress.leftNote(4), "4 steps left")
    }

    func testDotsSitInsideTheEnds() {
        XCTAssertEqual(G.dotX(0, width: 200), G.inset)
        XCTAssertEqual(G.dotX(1, width: 200), 200 - G.inset)
        XCTAssertEqual(G.dotX(0.5, width: 200), 100)
        XCTAssertEqual(G.dotX(2, width: 200), 200 - G.inset)
    }

    func testSegmentsBreakAroundHoles() {
        XCTAssertEqual(G.segments(from: 0, to: 100, holes: []), [0..<100])
        XCTAssertEqual(
            G.segments(from: 0, to: 100, holes: [(50, 5), (20, 5)]), [0..<15, 25..<45, 55..<100])
        // Holes past either end, or overlapping, cut only what they cover.
        XCTAssertEqual(G.segments(from: 10, to: 60, holes: [(5, 8), (58, 4), (61, 4)]), [13..<54])
        XCTAssertTrue(G.segments(from: 0, to: 10, holes: [(5, 20)]).isEmpty)
    }

    func testOverlappingDotsAreLeftOut() {
        // On a 214pt bar a stage is 2pt per percent.
        let (passed, current) = G.shownStages([0.10, 0.11, 0.30, 0.31], width: 214)
        XCTAssertEqual(current, G.dotX(0.31, width: 214))
        // 0.30 crowds the current ring, 0.10 crowds 0.11.
        XCTAssertEqual(passed, [G.dotX(0.11, width: 214)])
        let ahead = G.shownUpcoming([0.32, 0.40, 0.41, 0.60], current: current, width: 214)
        XCTAssertEqual(ahead, [G.dotX(0.40, width: 214), G.dotX(0.60, width: 214)])
        XCTAssertNil(G.shownStages([], width: 214).current)
    }

    func testABarKeepsItsNewestDotsAndTheRingFollowsTheFill() {
        let stages = (0..<20).map { Double($0) / 20 }
        let ahead = (0..<20).map { 0.95 + Double($0) / 1000 }
        let bar = ProgressBarFrame(fill: 1.4, stages: stages, upcoming: ahead, appear: 1)
        XCTAssertEqual(bar.dots.count, G.maxDots)
        XCTAssertEqual(bar.upcoming.count, RunProgress.maxUpcoming)
        XCTAssertEqual(Array(bar.dots.dropLast()), Array(stages[4..<19]))
        XCTAssertEqual(bar.dots.last, 1)
        XCTAssertEqual(bar.fill, 1)
        XCTAssertTrue(ProgressBarFrame(fill: 0.5, stages: [], upcoming: [], appear: 1).dots.isEmpty)
    }

    func testTheTweenFollowsReadings() throws {
        let reading = { (percent: Int, stages: [Int], left: Int) in
            ProgressReading(
                RunProgress(
                    percent: percent, current: "s", stepsLeft: left,
                    stages: stages.map { ProgressStage(label: "s", percent: $0) }))
        }
        var tween = ProgressTween()
        // First sight fills in from empty, the steps spreading from the finish.
        tween.observe(reading(25, [10], 2), now: 0, animated: true)
        var start = try XCTUnwrap(tween.frame(at: 0))
        XCTAssertEqual(start.fill, 0)
        XCTAssertEqual(start.upcoming, [1, 1])
        XCTAssertEqual(start.appear, 1)
        var end = try XCTUnwrap(tween.frame(at: ProgressTween.duration))
        XCTAssertEqual(end.fill, 0.25, accuracy: 1e-9)
        XCTAssertEqual(end.upcoming, [0.5, 0.75])
        // A new stage glides from where the bar was, its ring growing in.
        tween.observe(reading(40, [10, 40], 1), now: 10, animated: true)
        start = try XCTUnwrap(tween.frame(at: 10))
        XCTAssertEqual(start.fill, 0.25, accuracy: 1e-9)
        XCTAssertEqual(start.upcoming, [0.5])
        XCTAssertEqual(start.appear, 0)
        end = try XCTUnwrap(tween.frame(at: 10 + ProgressTween.duration))
        XCTAssertEqual(end.fill, 0.4, accuracy: 1e-9)
        XCTAssertEqual(end.appear, 1)
        // The same reading again changes nothing.
        let settled = tween
        tween.observe(reading(40, [10, 40], 1), now: 11, animated: true)
        XCTAssertEqual(tween, settled)
        // Reduce Motion jumps.
        tween.observe(reading(60, [10, 40], 1), now: 12, animated: false)
        XCTAssertEqual(try XCTUnwrap(tween.frame(at: 12)).fill, 0.6, accuracy: 1e-9)
    }

    func testPassedStagesGlideWhereAGrownPlanMovesThem() throws {
        let reading = { (percent: Int, stages: [(String, Int)]) in
            ProgressReading(
                RunProgress(
                    percent: percent, current: stages.last?.0 ?? "",
                    stages: stages.map { ProgressStage(label: $0.0, percent: $0.1) }))
        }
        var tween = ProgressTween()
        tween.observe(reading(42, [("A", 20), ("B", 42)]), now: 0, animated: true)
        // The plan grew: the host moved the stages back to make room. The
        // passed one glides from where it was; nothing new grows in.
        tween.observe(reading(22, [("A", 10), ("B", 21)]), now: 10, animated: true)
        var start = try XCTUnwrap(tween.frame(at: 10))
        XCTAssertEqual(start.dots[0], 0.2, accuracy: 1e-9)
        XCTAssertEqual(start.appear, 1)
        var end = try XCTUnwrap(tween.frame(at: 10 + ProgressTween.duration))
        XCTAssertEqual(end.dots[0], 0.1, accuracy: 1e-9)
        XCTAssertEqual(end.fill, 0.22, accuracy: 1e-9)

        // The oldest stage dropping off (the host keeps 16) moves no other,
        // and the stage that entered grows in.
        var many = (0..<16).map { ("s\($0)", $0 * 5) }
        tween.observe(reading(80, many), now: 20, animated: true)
        many.removeFirst()
        many.append(("s16", 82))
        tween.observe(reading(82, many), now: 20 + ProgressTween.duration, animated: true)
        start = try XCTUnwrap(tween.frame(at: 20 + ProgressTween.duration))
        XCTAssertEqual(start.dots[0], 0.05, accuracy: 1e-9)
        XCTAssertEqual(start.dots[14], 0.75, accuracy: 1e-9)
        XCTAssertEqual(start.appear, 0)
        end = try XCTUnwrap(tween.frame(at: 20 + 2 * ProgressTween.duration))
        XCTAssertEqual(end.dots[0], 0.05, accuracy: 1e-9)
        XCTAssertEqual(end.appear, 1)
    }
}

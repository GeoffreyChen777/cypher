import XCTest
import SwiftUI
@testable import Cypher

final class TranscriptScrollMotionTests: XCTestCase {
    func testFirstTouchProtectsTheBounceBeforeTheNextDragStarts() {
        var motion = TranscriptScrollMotion()
        motion.changed(to: .tracking, now: 0)
        XCTAssertTrue(motion.userScrolling, "Tracking was missing from the old drag guard")
        XCTAssertTrue(motion.blocksPositioning(now: 0))
        motion.changed(to: .interacting, now: 0.05)
        XCTAssertTrue(motion.blocksPositioning(now: 0.1))
        motion.changed(to: .decelerating, now: 0.2)
        XCTAssertTrue(motion.blocksPositioning(now: 1))
        // Grab the rubber-band before deceleration has finished.
        motion.changed(to: .tracking, now: 1.01)
        XCTAssertTrue(motion.blocksPositioning(now: 1.02))
        motion.changed(to: .interacting, now: 1.03)
        XCTAssertTrue(motion.blocksPositioning(now: 1.04))
    }

    func testIdleMustWaitForLateBounceGeometryToBecomeQuiet() {
        var motion = TranscriptScrollMotion()
        motion.changed(to: .decelerating, now: 0)
        motion.changed(to: .idle, now: 1)
        XCTAssertTrue(motion.blocksPositioning(now: 1.1))
        motion.geometryChanged(now: 1.12)
        XCTAssertTrue(motion.blocksPositioning(now: 1.2), "Late frames extend the quiet window")
        XCTAssertFalse(motion.blocksPositioning(now: 1.28))
        motion.didSettle()
        motion.geometryChanged(now: 2)
        XCTAssertFalse(motion.blocksPositioning(now: 2), "Normal idle reflows must not perpetually defer")
    }

    func testRapidRetouchesCannotUseThePreviousIdleDeadline() {
        var motion = TranscriptScrollMotion()
        for i in 0..<8 {
            let time = Double(i)
            motion.changed(to: .tracking, now: time)
            motion.changed(to: .interacting, now: time + 0.1)
            motion.changed(to: .decelerating, now: time + 0.2)
            motion.changed(to: .idle, now: time + 0.8)
            motion.changed(to: .tracking, now: time + 0.85)
            XCTAssertTrue(motion.blocksPositioning(now: time + 0.99))
        }
    }

    func testProgrammaticSpringsAlsoOwnTheirEntirePhase() {
        var motion = TranscriptScrollMotion()
        motion.changed(to: .animating, now: 0)
        XCTAssertFalse(motion.userScrolling, "Automatic motion must not break the user's pin")
        XCTAssertTrue(motion.blocksPositioning(now: 10), "Do not rely on a guessed spring duration")
        motion.changed(to: .idle, now: 10)
        XCTAssertTrue(motion.blocksPositioning(now: 10.1))
        XCTAssertFalse(motion.blocksPositioning(now: 10.2))
    }

    func testInitialLayoutAndSettledReflowsCanStillCorrect() {
        var motion = TranscriptScrollMotion()
        XCTAssertFalse(motion.blocksPositioning(now: 0))
        motion.geometryChanged(now: 0)
        XCTAssertFalse(motion.blocksPositioning(now: 0))
        motion.changed(to: .tracking, now: 1)
        motion.changed(to: .idle, now: 2)
        XCTAssertFalse(motion.blocksPositioning(now: 2.2))
        motion.didSettle()
        XCTAssertFalse(motion.blocksPositioning(now: 3))
    }

    func testStreamingCanRetargetItsOwnSpringButNeverNativeMotion() {
        var motion = TranscriptScrollMotion()
        for phase in [ScrollPhase.tracking, .interacting, .decelerating] {
            motion.changed(to: phase, now: 0)
            XCTAssertTrue(motion.blocksContentFollowing(now: 0.1))
        }
        motion.changed(to: .idle, now: 1)
        XCTAssertTrue(motion.blocksContentFollowing(now: 1.1))
        XCTAssertFalse(motion.blocksContentFollowing(now: 1.2))
        motion.didSettle()
        motion.changed(to: .animating, now: 2)
        XCTAssertFalse(motion.blocksContentFollowing(now: 2.1))
        XCTAssertTrue(motion.blocksPositioning(now: 2.1), "A measured clamp must not cancel the stream's spring")
    }
}

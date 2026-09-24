import XCTest

/// The transcript's trailing tick column: a drag jumps round to round.
final class TurnScrubberUITests: XCTestCase {
    private func launch(turns: Int) -> (XCUIApplication, XCUIElement) {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs", "-turns", "\(turns)"]
        app.launch()
        let scrubber = app.descendants(matching: .any)["turn-scrubber"]
        XCTAssertTrue(scrubber.waitForHittable(timeout: 10))
        return (app, scrubber)
    }

    private func turn(_ app: XCUIApplication, _ prefix: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@ OR value BEGINSWITH %@", prefix, prefix))
            .firstMatch
    }

    private func drag(_ scrubber: XCUIElement, from: CGFloat, to: CGFloat, hold: TimeInterval = 0) {
        scrubber.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: from))
            .press(forDuration: 0.05,
                   thenDragTo: scrubber.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: to)),
                   withVelocity: .slow, thenHoldForDuration: hold)
    }

    func testOpensOnTheLatestRound() {
        let (_, scrubber) = launch(turns: 12)
        XCTAssertEqual(scrubber.value as? String, "Round 12 of 12")
    }

    /// The column is invisible at rest, so a touch on it only reveals it —
    /// a stray tap on the edge must not move the transcript.
    func testTapOnlyReveals() {
        let (app, scrubber) = launch(turns: 12)
        scrubber.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.1)).tap()
        sleep(1)
        XCTAssertEqual(scrubber.value as? String, "Round 12 of 12")
        XCTAssertFalse(turn(app, "Turn 0:").isHittable, "A tap must not jump")
    }

    func testDragToTopLandsOnTheFirstPrompt() {
        let (app, scrubber) = launch(turns: 12)
        drag(scrubber, from: 0.85, to: 0.02, hold: Double(ProcessInfo.processInfo.environment["SCRUB_HOLD"] ?? "0") ?? 0)
        XCTAssertTrue(turn(app, "Turn 0:").waitForHittable(timeout: 3), "The first prompt must be in view")
        XCTAssertEqual(scrubber.value as? String, "Round 1 of 12")
    }

    func testDragBackDownReturnsToALaterRound() {
        let (app, scrubber) = launch(turns: 12)
        drag(scrubber, from: 0.85, to: 0.02)
        XCTAssertTrue(turn(app, "Turn 0:").waitForHittable(timeout: 3))
        drag(scrubber, from: 0.02, to: 0.5)
        sleep(1)
        let value = scrubber.value as? String ?? ""
        XCTAssertTrue(["Round 6 of 12", "Round 7 of 12"].contains(value), value)
        let round = Int(value.split(separator: " ")[1])! - 1
        XCTAssertTrue(turn(app, "Turn \(round):").isHittable, "Turn \(round) must be in view")
    }

    /// Above the 200-row render window: the jump must open the window.
    func testLongSessionReachesItsFirstRound() {
        let (app, scrubber) = launch(turns: 120)
        drag(scrubber, from: 0.9, to: 0.0)
        XCTAssertTrue(turn(app, "Turn 0:").waitForHittable(timeout: 5), "Round 1 must be reachable past the window")
        XCTAssertEqual(scrubber.value as? String, "Round 1 of 120")
    }
}

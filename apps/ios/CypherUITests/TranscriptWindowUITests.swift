import XCTest

/// Long transcripts: `-big` is 120 synthetic turns (~1,200 rows), so only the
/// last 200-row window renders on open.
final class TranscriptWindowUITests: XCTestCase {
    private func launchBig() -> (XCUIApplication, XCUIElement) {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-veil", "-big"]
        app.launch()
        let transcript = app.scrollViews.matching(identifier: "chat-transcript").firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        return (app, transcript)
    }

    private func turn(_ app: XCUIApplication, _ prefix: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH %@ OR value BEGINSWITH %@", prefix, prefix))
            .firstMatch
    }

    func testLongTranscriptOpensAtItsTail() {
        let (app, _) = launchBig()
        // The reveal used to give up with the view parked near the top. The
        // last turn's closing paragraph is the transcript's final row.
        let last = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@",
                                  "Landed the pass-119", "Landed the pass-119"))
            .firstMatch
        XCTAssertTrue(last.waitForHittable(timeout: 10), "A long transcript must open at its latest reply")
        XCTAssertFalse(app.buttons["transcript-show-earlier"].isHittable)
    }

    func testShowEarlierKeepsTheReadersPlace() {
        let (app, transcript) = launchBig()
        XCTAssertTrue(turn(app, "Turn 119:").waitForExistence(timeout: 10))
        XCTAssertTrue(transcript.waitForHittable(timeout: 10))
        let button = app.buttons["transcript-show-earlier"]
        var swipes = 0
        while !(button.exists && button.isHittable), swipes < 60 {
            transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25))
                .press(forDuration: 0.01,
                       thenDragTo: transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.85)),
                       withVelocity: .fast, thenHoldForDuration: 0)
            swipes += 1
        }
        XCTAssertTrue(button.isHittable, "Scrolling up must reach the window's top")
        sleep(1)  // any fling has settled; the tap happens at rest
        // The first turn visible under the button is the reader's place.
        let visible = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label BEGINSWITH 'Turn ' OR value BEGINSWITH 'Turn '"))
            .allElementsBoundByIndex.first { $0.isHittable }
        let place = try? XCTUnwrap(visible)
        let placeText = (place?.label.isEmpty == false ? place?.label : place?.value as? String) ?? ""
        let prefix = String(placeText.prefix { $0 != ":" }) + ":"
        XCTAssertTrue(prefix.hasPrefix("Turn "), "Found no turn under the button")
        button.tap()
        sleep(1)
        XCTAssertTrue(turn(app, prefix).isHittable, "\(prefix) must stay in view after the page lands")
        XCTAssertFalse(button.isHittable, "The view must not jump to the new page's top")
    }
}

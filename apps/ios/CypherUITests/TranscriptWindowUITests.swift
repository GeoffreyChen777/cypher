import XCTest

/// Long transcripts: `-big` is 120 synthetic turns (240 messages, ~840 rows).
/// A session opens on its last few messages (turn 117's reply through turn
/// 119) and reads earlier pages as the reader scrolls back.
@MainActor
final class TranscriptWindowUITests: XCTestCase {
    private func launchBig(_ extra: [String] = []) -> (XCUIApplication, XCUIElement) {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-veil", "-big"] + extra)
        let transcript = app.scrollViews.matching(identifier: "chat-transcript").firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        return (app, transcript)
    }

    private func element(_ app: XCUIApplication, _ format: String, _ text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label \(format) %@ OR value \(format) %@", text, text))
            .firstMatch
    }

    private func turn(_ app: XCUIApplication, _ prefix: String) -> XCUIElement {
        element(app, "BEGINSWITH", prefix)
    }

    /// One drag toward the top, flung or let go at rest.
    private func scrollBack(_ transcript: XCUIElement) {
        transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25))
            .press(
                forDuration: 0.01,
                thenDragTo: transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.85)),
                withVelocity: .fast, thenHoldForDuration: 0)
    }

    func testLongTranscriptOpensAtItsTail() {
        let (app, _) = launchBig()
        // The reveal used to give up with the view parked near the top. The
        // last turn's closing paragraph is the transcript's final row.
        let last = element(app, "CONTAINS", "Landed the pass-119")
        XCTAssertTrue(last.waitForHittable(timeout: 10), "A long transcript must open at its latest reply")
        XCTAssertFalse(app.buttons["transcript-loading-earlier"].isHittable)
    }

    func testScrollingBackLoadsEarlierPages() {
        let (app, transcript) = launchBig()
        XCTAssertTrue(turn(app, "Turn 119:").waitForExistence(timeout: 10))
        XCTAssertTrue(transcript.waitForHittable(timeout: 10))
        // Turns 60–79 are pages above the messages the session opened on.
        let older = element(app, "MATCHES", "Turn [67][0-9]:.*")
        var swipes = 0
        while !older.exists, swipes < 80 {
            scrollBack(transcript)
            swipes += 1
        }
        XCTAssertTrue(older.exists, "Scrolling back must keep loading earlier messages")
    }

    func testAPageLandsAboveTheReaderWithoutMovingThem() {
        // Pages take 45s to land, so this one lands with the reader at rest
        // on the top of the messages the session opened on.
        let (app, transcript) = launchBig(["-slow-pages"])
        XCTAssertTrue(turn(app, "Turn 119:").waitForExistence(timeout: 10))
        XCTAssertTrue(transcript.waitForHittable(timeout: 10))
        let loader = app.buttons["transcript-loading-earlier"]
        var swipes = 0
        while !(loader.exists && loader.isHittable), swipes < 60 {
            scrollBack(transcript)
            swipes += 1
        }
        XCTAssertTrue(loader.isHittable, "Scrolling back must reach the top of the loaded messages")
        let landed = element(app, "CONTAINS", "Landed the pass-116")
        XCTAssertFalse(landed.exists, "The page above is still on its way")
        let first = element(app, "CONTAINS", "Pass 117:")
        XCTAssertTrue(first.isHittable)
        let before = first.frame.minY
        XCTAssertTrue(landed.waitForExistence(timeout: 60), "The page lands above the reader")
        sleep(1)
        XCTAssertTrue(first.isHittable, "The reader's place must stay in view")
        XCTAssertEqual(first.frame.minY, before, accuracy: 2, "Nothing on screen moves when the page lands")
    }
}

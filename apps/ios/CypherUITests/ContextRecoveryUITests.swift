import XCTest

/// Context recovery: the caption over a recovered prompt opens its card on
/// tap (the desktop's hover), and a recovery under way shows in the strip.
@MainActor
final class ContextRecoveryUITests: XCTestCase {
    func testTheCaptionOpensTheCardAndTheStripShowsTheRecovery() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-recovery"])
        let caption = app.buttons["recovery-caption"]
        XCTAssertTrue(caption.waitForHittable(timeout: 10))
        XCTAssertEqual(caption.label, "Restarted with context")
        XCTAssertTrue(
            app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Writing context to retry'")).firstMatch
                .waitForExistence(timeout: 3))

        caption.tap()
        XCTAssertTrue(app.staticTexts["Added context"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.staticTexts["\u{201c}I can't access\u{201d} in the agent\u{2019}s answer"].exists)
        XCTAssertTrue(app.staticTexts["Written by anthropic/claude-haiku-4-5"].exists)
        capture(app, "recovery-card")
    }
}

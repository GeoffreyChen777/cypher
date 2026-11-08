import XCTest

/// Run progress: a long turn's narrated progress shows as a bar in the
/// working strip, and a tap opens its stages (the desktop's hover tips).
@MainActor
final class RunProgressUITests: XCTestCase {
    func testTheStripShowsTheBarAndATapOpensTheSteps() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-veil"])
        let strip = app.buttons["run-progress"]
        XCTAssertTrue(strip.waitForHittable(timeout: 10))
        XCTAssertTrue((strip.value as? String)?.contains("%") ?? false)
        capture(app, "run-progress-strip")

        // Once the replay is past its first stage, the card has every state.
        XCTAssertTrue(strip.wait(until: "NOT (value CONTAINS 'Read the desktop veil')", timeout: 15))
        strip.tap()
        let card = app.otherElements["run-progress-card"]
        XCTAssertTrue(card.waitForExistence(timeout: 3))
        // Each step says where it stands.
        let rows = { (format: String) in
            card.descendants(matching: .any).matching(NSPredicate(format: format)).firstMatch
        }
        XCTAssertTrue(rows("label == 'Done, Read the desktop veil, 8%'").exists)
        XCTAssertTrue(rows("label BEGINSWITH 'In progress, '").exists)
        XCTAssertTrue(rows("label == 'Not started, Check reduced motion'").exists)
        capture(app, "run-progress-card")
    }
}

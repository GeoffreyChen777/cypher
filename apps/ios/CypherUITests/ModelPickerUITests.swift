import XCTest

/// The composer's one model chip and the card it opens, by real taps on the
/// offline demo: providers, models and the thinking level in one place.
final class ModelPickerUITests: XCTestCase {
    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    func testOneChipPicksTheModelAndItsThinkingLevel() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs", "-appAppearance", "dark"]
        app.launch()
        let editor = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        XCTAssertTrue(editor.waitForHittable(timeout: 10))
        editor.tap()

        let chip = element(app, "model-chip")
        XCTAssertTrue(chip.waitForExistence(timeout: 5))
        XCTAssertTrue(chip.label.contains("Pi demo model") && chip.label.contains("High"),
                      "one chip carries the model and its level: \(chip.label)")
        chip.tap()

        // Providers, the viewed provider's models, the pinned thinking track.
        XCTAssertTrue(element(app, "model-provider-rail").waitForExistence(timeout: 10))
        Thread.sleep(forTimeInterval: 1)
        capture(app, "dark-model-card")
        XCTAssertTrue(element(app, "thinking-levels").exists)
        XCTAssertTrue(element(app, "thinking-high").isSelected, "the card opens on the chat's level")

        element(app, "model-provider-anthropic").tap()
        let opus = element(app, "model-row-anthropic/claude-opus-5-5")
        XCTAssertTrue(opus.waitForHittable(timeout: 5))
        opus.tap()
        // Opus takes High too, so the level carries over.
        XCTAssertTrue(element(app, "thinking-high").waitForExistence(timeout: 5))
        let xhigh = element(app, "thinking-xhigh")
        XCTAssertTrue(xhigh.waitForHittable(timeout: 5), "the track follows the picked model's levels")
        xhigh.tap()
        XCTAssertTrue(xhigh.waitForSelected(timeout: 5))
        capture(app, "dark-model-card-opus")

        // A model without levels says so instead of offering a track.
        element(app, "model-row-anthropic/claude-haiku-4-5").tap()
        XCTAssertTrue(app.staticTexts["Claude Haiku 4.5 doesn't take a thinking level."].waitForExistence(timeout: 5))
        element(app, "model-row-anthropic/claude-opus-5-5").tap()
        XCTAssertTrue(element(app, "thinking-xhigh").waitForExistence(timeout: 5))

        app.buttons["Close"].tap()
        XCTAssertTrue(chip.waitForHittable(timeout: 5))
        XCTAssertTrue(chip.label.contains("Claude Opus 5.5") && chip.label.contains("X-High"),
                      "the chip shows the new pick: \(chip.label)")
    }
}

private extension XCUIElement {
    func waitForSelected(timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: "selected == true"), object: self)
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }
}

import XCTest

final class QuestionPanelUITests: XCTestCase {
    func testContextIsNotDuplicatedAndSelectionRequiresConfirmation() {
        let app = XCUIApplication()
        for appearance in ["light", "dark"] {
            app.launchArguments = ["-demo", "-route", "chat:chat-picker", "-appAppearance", appearance]
            app.launch()
            let prompt = app.staticTexts["question-prompt"]
            XCTAssertTrue(prompt.waitForExistence(timeout: 10))
            XCTAssertEqual(prompt.label, "Which device should serve harness/model catalogs for the picker?")
            XCTAssertEqual(app.staticTexts.matching(identifier: "question-prompt").count, 1)
            XCTAssertFalse(app.staticTexts["question-context"].exists)
            let submit = app.buttons["question-submit"]
            XCTAssertTrue(submit.isHittable)
            XCTAssertFalse(submit.isEnabled)

            let contextToggle = app.buttons["question-context-toggle"]
            contextToggle.tap()
            let context = app.staticTexts["question-context"]
            XCTAssertTrue(context.waitForExistence(timeout: 3))
            XCTAssertEqual(app.staticTexts.matching(identifier: "question-context").count, 1)
            XCTAssertTrue(context.label.contains("Each device has its own installed harnesses"))
            XCTAssertTrue(submit.isHittable, "Context must not push confirmation out of the card")
            contextToggle.tap()
            XCTAssertFalse(context.exists)

            let first = app.buttons["question-option-0"]
            first.tap()
            Thread.sleep(forTimeInterval: 0.4)
            XCTAssertTrue(first.isSelected)
            XCTAssertTrue(submit.isEnabled)
            XCTAssertTrue(prompt.exists, "Selecting must not auto-submit after the old 220ms delay")
            let second = app.buttons["question-option-1"]
            second.tap()
            XCTAssertTrue(second.isSelected)
            XCTAssertFalse(first.isSelected)
            XCTAssertFalse(app.keyboards.firstMatch.exists)
            XCTAssertTrue(app.buttons["Stop task"].isHittable)

            let custom = app.buttons["question-custom-answer"]
            if !custom.isHittable { app.scrollViews["question-content"].swipeUp() }
            custom.tap()
            // Vertical SwiftUI TextField exposes TextField/TextView depending
            // on the iOS accessibility snapshot API.
            let answer = app.descendants(matching: .any).matching(identifier: "question-answer").firstMatch
            XCTAssertTrue(answer.waitForExistence(timeout: 3))
            answer.typeText("Use the owning device")
            XCTAssertTrue(submit.isHittable, "The keyboard must not cover confirmation")
            XCTAssertTrue(submit.isEnabled)
            // Don't enqueue a demo response: this test exercises the native
            // presentation; exact reply labels are covered by unit tests.
            app.terminate()
        }
    }
}

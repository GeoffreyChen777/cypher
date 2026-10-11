import XCTest

/// The strip under each settled message: copy, the blocked-action reason,
/// and a confirmed restart that truncates and hands the prompt back.
@MainActor
final class MessageStripUITests: XCTestCase {
    private let restartPrompt = "Restart from here — deletes this message and everything after it"
    private let restartReply = "Restart from here — deletes everything after this response"

    private func strip(_ app: XCUIApplication, _ entryId: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: "message-strip-\(entryId)").firstMatch
    }

    func testCopyConfirmsAndABlockedRestartSaysWhy() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-tabs"])
        let last = strip(app, "m8")
        XCTAssertTrue(last.waitForHittable(timeout: 10))
        XCTAssertTrue(last.staticTexts.matching(NSPredicate(format: "label CONTAINS ', '")).firstMatch.exists)

        last.buttons["Copy message"].tap()
        XCTAssertTrue(last.buttons["Copied"].waitForExistence(timeout: 2))
        XCTAssertTrue(last.buttons["Copy message"].waitForExistence(timeout: 3), "The check reverts")

        // The newest reply has nothing after it to remove.
        last.buttons[restartReply].tap()
        XCTAssertTrue(app.staticTexts["Nothing to remove after the last message."].waitForExistence(timeout: 3))
        capture(app, "blocked-restart")
    }

    func testARestartAsksFirstThenTruncatesAndRefillsTheComposer() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-tabs"])
        let prompt = strip(app, "m5")
        XCTAssertTrue(prompt.waitForHittable(timeout: 10))
        prompt.buttons[restartPrompt].tap()
        XCTAssertTrue(
            app.staticTexts["Deletes this message and 3 messages after it. Its text comes back to the composer."]
                .waitForExistence(timeout: 3))
        capture(app, "restart-confirmation")
        app.buttons["Restart from Here"].tap()

        XCTAssertTrue(app.staticTexts["Conversation restarted — 4 messages removed"].waitForExistence(timeout: 5))
        XCTAssertTrue(strip(app, "m5").wait(until: "exists == false", timeout: 3))
        XCTAssertTrue(strip(app, "m4").exists)
        let editor = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        XCTAssertTrue(editor.wait(until: "value CONTAINS '再确认一下'", timeout: 3))
        capture(app, "restarted")
    }
}

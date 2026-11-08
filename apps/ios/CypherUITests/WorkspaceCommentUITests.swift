import XCTest

/// A selection in the Changes diff or a Files source comments like the
/// transcript's: the editor opens over the sheet, and the comment waits for
/// the session's next message. Real touches on the offline demo.
@MainActor
final class WorkspaceCommentUITests: XCTestCase {
    private func commentOnLongPress(_ app: XCUIApplication, _ text: XCUIElement, note: String) {
        XCTAssertTrue(text.waitForExistence(timeout: 15))
        text.press(forDuration: 1.0)
        let comment = app.menuItems["Comment"].exists ? app.menuItems["Comment"] : app.buttons["Comment"]
        XCTAssertTrue(comment.waitForExistence(timeout: 5), "the selection's menu offers Comment")
        comment.tap()
        let input =
            app.textFields["comment-input"].exists ? app.textFields["comment-input"] : app.textViews["comment-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 5), "the comment editor opens over the sheet")
        input.tap()
        input.typeText(note)
        app.buttons["save-comment"].tap()
        XCTAssertTrue(app.buttons["pending-comments"].waitForExistence(timeout: 5), "the sheet shows it pending")
    }

    func testADiffSelectionTakesAComment() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-tabs", "-sheet", "changes"])
        let diff = app.webViews["workspace-diff-web"]
        let line = diff.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Cypher")).firstMatch
        commentOnLongPress(app, line, note: "Keep the old greeting")
        XCTAssertTrue(diff.exists, "the diff stays open")
        // Back in the session, the comment rides the composer.
        app.buttons["workspace-close"].tap()
        XCTAssertTrue(app.buttons["pending-comments"].waitForExistence(timeout: 5))
    }

    func testASourceSelectionTakesAComment() {
        let app = XCUIApplication.launchDemo([
            "-route", "chat:chat-references", "-sheet", "reference:Example.swift:44-45",
        ])
        let code = app.webViews["workspace-source-web"]
        let line = code.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "enabled")).firstMatch
        commentOnLongPress(app, line, note: "Why flip this?")
    }
}

import XCTest

/// Real typing and taps through the `@` menu, on the offline demo.
@MainActor
final class MentionMenuUITests: XCTestCase {
    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    func testAtOpensSessionsAndFilesAndPicksBecomeChipsThatSend() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        let input = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        XCTAssertTrue(input.waitForHittable(timeout: 10))
        input.tap()

        input.typeText("Compare @")
        XCTAssertTrue(element(app, "mention-menu").waitForExistence(timeout: 5), "@ opens the menu")
        XCTAssertTrue(element(app, "mention-session-chat-picker").waitForExistence(timeout: 5))
        XCTAssertFalse(element(app, "mention-session-chat-tabs").exists, "never the current chat")
        capture(app, "mention-menu-open")

        input.typeText("comp")
        let file = element(app, "mention-file-crates/ui/src/composer.rs")
        XCTAssertTrue(file.waitForHittable(timeout: 5), "files narrow to the query")
        capture(app, "mention-menu-files")
        file.tap()
        XCTAssertEqual(input.value as? String, "Compare \u{a0}@composer.rs\u{a0} ")
        XCTAssertTrue(element(app, "mention-menu").waitForNonExistence(timeout: 3), "a pick closes the menu")

        input.typeText("with @catalog")
        let session = element(app, "mention-session-chat-picker")
        XCTAssertTrue(session.waitForHittable(timeout: 5))
        session.tap()
        XCTAssertEqual(
            input.value as? String,
            "Compare \u{a0}@composer.rs\u{a0} with \u{a0}@Model\u{a0}picker\u{a0}catalog\u{a0}sync\u{a0} ")
        capture(app, "mention-chips")

        // Backspace past the trailing space takes the whole chip.
        input.typeText(XCUIKeyboardKey.delete.rawValue + XCUIKeyboardKey.delete.rawValue)
        XCTAssertEqual(input.value as? String, "Compare \u{a0}@composer.rs\u{a0} with ")
        input.typeText("@catalog")
        XCTAssertTrue(session.waitForHittable(timeout: 5))
        session.tap()

        let send = app.buttons.matching(NSPredicate(format: "label == 'Up Arrow' OR identifier == 'arrow.up'"))
            .firstMatch
        XCTAssertTrue(send.waitForHittable(timeout: 5))
        send.tap()
        let bubble = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", "@composer.rs", "@composer.rs"))
            .firstMatch
        XCTAssertTrue(bubble.waitForExistence(timeout: 10), "the sent bubble shows chips, not markup")
        capture(app, "mention-sent")
    }
}

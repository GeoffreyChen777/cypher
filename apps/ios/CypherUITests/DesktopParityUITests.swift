import XCTest

/// Real taps through the desktop-parity features, on the offline demo.
final class DesktopParityUITests: XCTestCase {
    private func launch(_ args: [String]) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-demo"] + args
        app.launch()
        return app
    }

    /// The composer in front: a sheet's, not the session's behind it.
    private func editor(_ app: XCUIApplication) -> XCUIElement {
        let editors = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'"))
        _ = editors.firstMatch.waitForExistence(timeout: 10)
        return editors.allElementsBoundByIndex.last { $0.isHittable } ?? editors.firstMatch
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    private func text(_ app: XCUIApplication, containing fragment: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", fragment, fragment))
            .firstMatch
    }

    /// Long-press a word of transcript text and pick an edit-menu action.
    private func select(_ target: XCUIElement, thenPick action: String, in app: XCUIApplication) {
        XCTAssertTrue(target.waitForHittable(timeout: 10))
        target.press(forDuration: 1.0)
        let item = app.menuItems[action].exists ? app.menuItems[action] : app.buttons[action]
        XCTAssertTrue(item.waitForExistence(timeout: 5), "\(action) must be in the selection menu")
        item.tap()
    }

    func testSlashMenuFillsTheCommand() {
        let app = launch(["-route", "chat:chat-tabs"])
        let input = editor(app)
        XCTAssertTrue(input.waitForHittable(timeout: 10))
        input.tap()
        input.typeText("/com")
        let compact = element(app, "slash-command-compact")
        XCTAssertTrue(compact.waitForHittable(timeout: 5), "typing / opens the command menu")
        XCTAssertFalse(element(app, "slash-command-goal").exists, "filtered to the query")
        XCTAssertFalse(element(app, "slash-command-skill:frontend-design").exists)
        compact.tap()
        XCTAssertEqual(input.value as? String, "/compact ")
        XCTAssertTrue(element(app, "slash-menu").waitForNonExistence(timeout: 3), "arguments close the menu")
    }

    func testContextRingShowsUsageAndOffersCompact() {
        let app = launch(["-route", "chat:chat-tabs"])
        let input = editor(app)
        XCTAssertTrue(input.waitForHittable(timeout: 10))
        input.tap()
        let ring = element(app, "context-ring")
        XCTAssertTrue(ring.waitForHittable(timeout: 5))
        XCTAssertEqual(ring.value as? String, "81% context used · 162k / 200k")
        ring.tap()
        let compact = app.buttons["Compact context"]
        XCTAssertTrue(compact.waitForExistence(timeout: 3))
        XCTAssertTrue(compact.isEnabled)
    }

    func testRenameAndDeleteFromTheProjectList() {
        let app = launch(["-route", "space:space-cypher"])
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS 'Tool group header colors'")).firstMatch
        XCTAssertTrue(row.waitForHittable(timeout: 10))
        row.press(forDuration: 1.0)
        app.buttons["Rename…"].tap()
        let field = app.alerts.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 3))
        field.tap()  // a real tap into the field, caret wherever it lands
        field.typeText(" v2")
        app.alerts.buttons["Rename"].tap()
        let renamed = app.buttons.matching(NSPredicate(format: "label CONTAINS 'v2'")).firstMatch
        XCTAssertTrue(renamed.waitForHittable(timeout: 5))

        renamed.swipeLeft()
        app.buttons["Delete"].firstMatch.tap()
        XCTAssertTrue(app.alerts["Delete session?"].waitForExistence(timeout: 3))
        app.alerts.buttons["Delete"].tap()
        XCTAssertTrue(renamed.waitForNonExistence(timeout: 5))
    }

    func testRenameFromTheSessionMenu() {
        let app = launch(["-route", "chat:chat-tabs"])
        let menu = app.buttons["workspace-browser"]
        XCTAssertTrue(menu.waitForHittable(timeout: 10))
        menu.tap()
        app.buttons["Rename…"].tap()
        let field = app.alerts.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 3))
        field.typeText(" v2")
        app.alerts.buttons["Rename"].tap()
        XCTAssertTrue(app.staticTexts["Tool group header colors v2"].waitForExistence(timeout: 5))
    }

    func testQuickChatStartsOnADeviceAndListsOnHome() {
        let app = launch([])
        let menu = element(app, "quick-chat")
        XCTAssertTrue(menu.waitForHittable(timeout: 10))
        menu.tap()
        app.buttons["MacBook Pro"].tap()
        XCTAssertTrue(app.staticTexts["Quick chat"].waitForExistence(timeout: 5))
        let input = editor(app)
        XCTAssertTrue(input.waitForHittable(timeout: 5))
        input.tap()
        input.typeText("What's using port 8787?")
        app.buttons.matching(NSPredicate(format: "label == 'Up Arrow' OR identifier == 'arrow.up'")).firstMatch.tap()
        XCTAssertTrue(app.staticTexts["Quick chat @ MacBook Pro"].waitForExistence(timeout: 10),
                      "sending opens the new session")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.staticTexts["Quick chats"].waitForExistence(timeout: 5))
    }

    func testForkAfterAReplyOpensTheFork() {
        let app = launch(["-route", "chat:chat-tabs"])
        select(text(app, containing: "继续调整界面"), thenPick: "Fork from Here", in: app)
        XCTAssertTrue(text(app, containing: "— Fork").waitForExistence(timeout: 10), "the fork opens")
    }

    func testSideChatRunsInASheetAndOpensAsAChat() {
        let app = launch(["-route", "chat:chat-tabs"])
        select(text(app, containing: "继续调整界面"), thenPick: "Side Chat", in: app)
        XCTAssertTrue(app.navigationBars["Side Chat"].waitForExistence(timeout: 5))
        let promote = element(app, "side-chat-promote")
        XCTAssertFalse(promote.isEnabled, "nothing to keep yet")
        let input = editor(app)
        XCTAssertTrue(input.waitForHittable(timeout: 5))
        input.tap()
        input.typeText("Why this order?")
        app.buttons.matching(NSPredicate(format: "label == 'Up Arrow' OR identifier == 'arrow.up'")).firstMatch.tap()
        XCTAssertTrue(text(app, containing: "Why this order?").waitForExistence(timeout: 5))
        XCTAssertTrue(promote.waitForEnabled(timeout: 10))
        promote.tap()
        XCTAssertTrue(app.navigationBars["Side Chat"].waitForNonExistence(timeout: 5))
        XCTAssertTrue(text(app, containing: "Why this order?").waitForHittable(timeout: 10),
                      "the promoted chat opens, carrying the side chat's conversation")
    }
}

private extension XCUIElement {
    func waitForEnabled(timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "enabled == true"), object: self)
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }
}



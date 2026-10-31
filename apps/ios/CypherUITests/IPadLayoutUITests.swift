import XCTest

/// The iPad layout: the sidebar selecting into the detail column, the
/// session inspector, the hardware-keyboard commands and session windows.
/// Skipped on iPhone, whose stack these don't apply to.
@MainActor
final class IPadLayoutUITests: XCTestCase {
    override func setUp() async throws {
        try XCTSkipUnless(UIDevice.current.userInterfaceIdiom == .pad, "iPad layout")
        continueAfterFailure = false
        XCUIDevice.shared.orientation = .landscapeLeft
    }

    /// Launches the demo once Home is up. A relaunch first closes any
    /// session window an earlier run left (and reopens Home): let that hand-off
    /// settle before querying, or a query can land in the closing window.
    private func launch(_ arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication.launchDemo(arguments)
        XCTAssertTrue(app.textFields["home-search"].waitForExistence(timeout: 20))
        Thread.sleep(forTimeInterval: 1.5)
        return app
    }

    private func row(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        // A row's label leads with its status ("Working, …"), then the title.
        app.buttons.matching(NSPredicate(format: "label CONTAINS %@", title)).firstMatch
    }

    private func transcript(_ app: XCUIApplication) -> XCUIElement {
        app.scrollViews.matching(identifier: "chat-transcript").firstMatch
    }

    private func editor(_ app: XCUIApplication) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
    }

    func testSidebarSelectsSessionsIntoTheDetailColumn() {
        let app = launch()
        let search = app.textFields["home-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["No Session Selected"].waitForExistence(timeout: 5))

        let veil = row(app, "Streaming veil on transcript rows")
        XCTAssertTrue(veil.waitForHittable(timeout: 5))
        veil.tap()
        XCTAssertTrue(transcript(app).waitForExistence(timeout: 10))
        XCTAssertTrue(veil.waitForSelected(timeout: 5), "The open session is the sidebar's selection")
        XCTAssertTrue(search.isHittable, "The sidebar stays beside the session")

        let picker = row(app, "Model picker catalog sync")
        picker.tap()
        XCTAssertTrue(picker.waitForSelected(timeout: 5))
        XCTAssertFalse(veil.isSelected)
        XCTAssertTrue(app.staticTexts["Model picker catalog sync"].waitForExistence(timeout: 10))
        capture(app, "sidebar-selection")
    }

    func testInspectorDocksFilesAndChangesBesideTheTranscript() {
        let app = launch(["-route", "chat:chat-tabs"])
        // The toolbar has no frame until the session has laid out.
        XCTAssertTrue(transcript(app).waitForExistence(timeout: 20))
        let toggle = app.buttons["inspector-toggle"]
        XCTAssertTrue(toggle.waitForHittable(timeout: 10))
        toggle.tap()

        let tabs = app.segmentedControls["inspector-tabs"]
        XCTAssertTrue(tabs.waitForExistence(timeout: 5))
        XCTAssertTrue(tabs.buttons["Changes"].isSelected, "The inspector opens on Changes")
        XCTAssertTrue(app.webViews["workspace-diff-web"].waitForExistence(timeout: 15))
        XCTAssertTrue(editor(app).isHittable, "The session stays usable beside the inspector")
        XCTAssertTrue(toggle.isHittable, "The session keeps its toolbar")
        capture(app, "inspector-changes")

        // Files drills into folders in the inspector's own stack.
        tabs.buttons["Files"].tap()
        let folder = app.buttons["workspace-item-Sources"]
        XCTAssertTrue(folder.waitForHittable(timeout: 10))
        folder.tap()
        XCTAssertTrue(app.buttons["workspace-item-Sources/Example.swift"].waitForHittable(timeout: 10))
        XCTAssertTrue(editor(app).isHittable, "Drilling in stays in the inspector")
        capture(app, "inspector-files")

        // Close lives on the browser's root (as in the phone's sheet).
        app.buttons["BackButton"].firstMatch.tap()
        app.buttons["workspace-close"].tap()
        XCTAssertTrue(tabs.wait(until: "exists == false", timeout: 5))
        XCTAssertTrue(toggle.isHittable)
    }

    func testSideChatDocksInTheInspectorAndOpensAsAChat() {
        let app = launch(["-route", "chat:chat-tabs"])
        XCTAssertTrue(transcript(app).waitForExistence(timeout: 20))
        let quote = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", "继续调整界面", "继续调整界面"))
            .firstMatch
        XCTAssertTrue(quote.waitForHittable(timeout: 10))
        // The edit menu's item, as a menu item or a button. A long press
        // before the transcript settles may not select: press again.
        let item = app.descendants(matching: .any)
            .matching(
                NSPredicate(
                    format: "(elementType == %d OR elementType == %d) AND label == 'Side Chat'",
                    XCUIElement.ElementType.menuItem.rawValue, XCUIElement.ElementType.button.rawValue)
            )
            .firstMatch
        for _ in 0..<3 where !item.exists {
            quote.press(forDuration: 1.0)
            _ = item.waitForExistence(timeout: 3)
        }
        XCTAssertTrue(item.exists, "Side Chat is in the selection menu")
        item.tap()

        // Beside the transcript, not over it.
        let tabs = app.segmentedControls["inspector-tabs"]
        XCTAssertTrue(tabs.waitForExistence(timeout: 5))
        XCTAssertTrue(tabs.buttons["Side Chat"].isSelected)
        XCTAssertTrue(app.navigationBars["Side Chat"].exists)
        XCTAssertTrue(transcript(app).isHittable, "The session stays in view")
        capture(app, "inspector-side-chat")

        // The side chat's composer is the inspector's: the rightmost.
        let editors = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).allElementsBoundByIndex
        XCTAssertEqual(editors.count, 2, "The session's composer and the side chat's")
        let input = try? XCTUnwrap(editors.max { $0.frame.minX < $1.frame.minX })
        input?.tap()
        input?.typeText("Why this order?")
        let sends = app.buttons.matching(NSPredicate(format: "label == 'Up Arrow' OR identifier == 'arrow.up'"))
            .allElementsBoundByIndex
        sends.max { $0.frame.minX < $1.frame.minX }?.tap()
        let question = "Why this order?"
        let asked = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", question, question))
            .firstMatch
        XCTAssertTrue(asked.waitForExistence(timeout: 5))

        let promote = app.buttons["side-chat-promote"]
        XCTAssertTrue(promote.waitForEnabled(timeout: 10))
        promote.tap()
        // The side chat was all the inspector showed: it goes, and the
        // promoted chat opens in the detail column.
        XCTAssertTrue(tabs.wait(until: "exists == false", timeout: 5))
        XCTAssertTrue(asked.waitForHittable(timeout: 10), "The promoted chat carries the side chat's conversation")
    }

    func testKeyboardTogglesTheInspector() {
        let app = launch(["-route", "chat:chat-tabs"])
        XCTAssertTrue(transcript(app).waitForExistence(timeout: 20))
        XCTAssertTrue(app.buttons["inspector-toggle"].waitForHittable(timeout: 10))
        let tabs = app.segmentedControls["inspector-tabs"]
        app.typeKey("i", modifierFlags: [.command, .option])
        XCTAssertTrue(tabs.waitForExistence(timeout: 5), "⌘⌥I opens the inspector")
        app.typeKey("i", modifierFlags: [.command, .option])
        XCTAssertTrue(tabs.wait(until: "exists == false", timeout: 5), "⌘⌥I closes it again")
    }

    func testKeyboardCommandsStepSessionsSearchAndStartNew() {
        let app = launch()
        let search = app.textFields["home-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 10))

        // ⌘⌥↓ from nothing opens the list's first session, then the next.
        app.typeKey(XCUIKeyboardKey.downArrow.rawValue, modifierFlags: [.command, .option])
        let veil = row(app, "Streaming veil on transcript rows")
        XCTAssertTrue(veil.waitForSelected(timeout: 10))
        app.typeKey(XCUIKeyboardKey.downArrow.rawValue, modifierFlags: [.command, .option])
        let picker = row(app, "Model picker catalog sync")
        XCTAssertTrue(picker.waitForSelected(timeout: 10))
        app.typeKey(XCUIKeyboardKey.upArrow.rawValue, modifierFlags: [.command, .option])
        XCTAssertTrue(veil.waitForSelected(timeout: 10))

        // ⌘F focuses the search: typed keys land in it.
        app.typeKey("f", modifierFlags: .command)
        XCTAssertTrue(search.wait(until: "exists == true", timeout: 5))
        Thread.sleep(forTimeInterval: 1)
        app.typeText("veil")
        XCTAssertTrue(search.wait(until: "value == 'veil'", timeout: 5), "⌘F focuses the session search")
        XCTAssertTrue(picker.wait(until: "exists == false", timeout: 5), "The search narrows the list")
        XCTAssertTrue(veil.exists)

        app.typeKey("n", modifierFlags: .command)
        XCTAssertTrue(
            app.staticTexts["What are we building?"].waitForExistence(timeout: 10), "⌘N starts a new session")
        capture(app, "keyboard-new-session")
    }

    /// Last (`testW…`): its window outlives the app (a demo relaunch closes
    /// it and reopens Home), so the others don't start beside it.
    func testWindowOpensForASession() {
        let app = launch()
        let veil = row(app, "Streaming veil on transcript rows")
        XCTAssertTrue(veil.waitForHittable(timeout: 10))
        veil.press(forDuration: 1.2)
        let open = app.buttons["Open in New Window"]
        XCTAssertTrue(open.waitForExistence(timeout: 5), "Rows offer a window of their own")
        // Nothing is selected in Home: a transcript on screen is the new
        // window's.
        XCTAssertFalse(transcript(app).exists)
        open.tap()
        XCTAssertTrue(transcript(app).waitForExistence(timeout: 15), "The session window shows the session")
        capture(app, "session-window")
    }
}

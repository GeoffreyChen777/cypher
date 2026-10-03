import XCTest

/// The composer's `/` menu by real taps on the offline demo: the stateful
/// commands under their headings with what each controls, a command's
/// choices, and a typed name finding the rest.
final class SlashMenuUITests: XCTestCase {
    private func launch(_ appearance: String = "dark") -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs", "-appAppearance", appearance]
        app.launch()
        return app
    }

    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func editor(_ app: XCUIApplication) -> XCUIElement {
        let editor = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        XCTAssertTrue(editor.waitForHittable(timeout: 10))
        return editor
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    private func waitForLabel(_ element: XCUIElement, containing fragment: String) -> Bool {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label CONTAINS %@", fragment), object: element)
        return XCTWaiter.wait(for: [expectation], timeout: 5) == .completed
    }

    func testMenuGroupsTheStatefulCommandsWithWhatTheyControl() {
        for appearance in ["light", "dark"] {
            let app = launch(appearance)
            let input = editor(app)
            input.tap()
            input.typeText("/")
            XCTAssertTrue(element(app, "slash-command-compact").waitForHittable(timeout: 5))
            for name in ["fast", "scripts", "goal", "orchestrate", "subagent-status"] {
                XCTAssertTrue(element(app, "slash-command-\(name)").exists, name)
            }
            XCTAssertFalse(element(app, "slash-command-export-html").exists, "found by name only")
            XCTAssertFalse(element(app, "slash-command-review").exists, "found by name only")
            for heading in ["CONVERSATION", "AGENT MODES", "SUBAGENTS"] {
                XCTAssertTrue(app.staticTexts[heading].exists, heading)
            }
            XCTAssertTrue(waitForLabel(element(app, "slash-command-compact"), containing: "81% used"))
            XCTAssertTrue(waitForLabel(element(app, "slash-command-scripts"), containing: "On"))
            XCTAssertTrue(waitForLabel(element(app, "slash-command-fast"), containing: "Off"))
            capture(app, "\(appearance)-slash-menu")
        }
    }

    func testAChoiceCommandOpensItsChoicesWithTheOneInEffectChecked() {
        let app = launch()
        let input = editor(app)
        input.tap()
        input.typeText("/")
        // With the keyboard up the list scrolls; the tap brings the row in.
        let orchestrate = element(app, "slash-command-orchestrate")
        XCTAssertTrue(orchestrate.waitForExistence(timeout: 5))
        orchestrate.tap()
        XCTAssertEqual(input.value as? String, "/orchestrate ")
        let on = element(app, "slash-choice-on")
        XCTAssertTrue(on.waitForHittable(timeout: 5), "the choices open")
        XCTAssertTrue(app.staticTexts["Adaptive orchestration is off"].waitForExistence(timeout: 5),
                      "what is in effect heads the choices")
        XCTAssertEqual(element(app, "slash-choice-off").value as? String, "In effect")
        XCTAssertNotEqual(element(app, "slash-choice-on").value as? String, "In effect")
        // Every choice's name starts at one edge, checked or not.
        XCTAssertEqual(app.staticTexts["on"].frame.minX, app.staticTexts["off"].frame.minX, accuracy: 1)
        capture(app, "dark-slash-choices")
        on.tap()
        XCTAssertEqual(input.value as? String, "/orchestrate on ")
        XCTAssertTrue(element(app, "slash-menu").waitForNonExistence(timeout: 3), "a picked choice closes the menu")
    }

    func testATypedNameFindsTheHostsOtherCommands() {
        let app = launch()
        let input = editor(app)
        input.tap()
        input.typeText("/exp")
        XCTAssertTrue(element(app, "slash-command-export-html").waitForHittable(timeout: 5))
        XCTAssertFalse(element(app, "slash-command-compact").exists)
    }
}

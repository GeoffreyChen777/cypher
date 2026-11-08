import XCTest

/// A reply's file chips, file links and changed-files card open the session's
/// Files / Changes sheet (`-demo -route chat:chat-references`).
@MainActor
final class FileReferenceUITests: XCTestCase {
    private func launch() -> XCUIApplication {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-references"])
        XCTAssertTrue(app.scrollViews.matching(identifier: "chat-transcript").firstMatch.waitForExistence(timeout: 10))
        return app
    }

    func testAChipOpensItsFileAtTheCitedLines() {
        let app = launch()
        let chip = app.links.matching(NSPredicate(format: "label CONTAINS %@", "Example.swift:45")).firstMatch
        XCTAssertTrue(chip.waitForHittable(timeout: 10))
        chip.tap()
        let code = app.webViews["workspace-source-web"]
        XCTAssertTrue(code.waitForExistence(timeout: 10))
        XCTAssertTrue(app.navigationBars["Example.swift"].exists)
        XCTAssertTrue(
            code.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "enabled")).firstMatch
                .waitForExistence(timeout: 10))
        // The folder the bare name was found in.
        app.buttons["workspace-show-folder"].tap()
        XCTAssertTrue(app.buttons["workspace-item-Sources/Example.swift"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.keyboards.firstMatch.exists)
    }

    func testAFileLinkOpensItsFile() {
        let app = launch()
        let link = app.links["README"]
        XCTAssertTrue(link.waitForHittable(timeout: 10))
        link.tap()
        XCTAssertTrue(app.navigationBars["README.md"].waitForExistence(timeout: 10))
    }

    func testTheRoundsChangedFilesOpenItsDiff() {
        let app = launch()
        let card = app.descendants(matching: .any)["turn-changes"]
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        let file = app.buttons.matching(identifier: "turn-changes-file").firstMatch
        XCTAssertTrue(file.waitForHittable(timeout: 10))
        file.tap()
        let diff = app.webViews["workspace-diff-web"]
        XCTAssertTrue(diff.waitForExistence(timeout: 10))
        XCTAssertTrue(app.navigationBars["Turn changes"].exists)
        XCTAssertTrue(diff.buttons["Sources/Example.swift"].waitForExistence(timeout: 15))
        app.buttons["workspace-close"].tap()
        // The header folds the list.
        app.buttons["turn-changes-toggle"].tap()
        XCTAssertFalse(app.buttons.matching(identifier: "turn-changes-file").firstMatch.waitForExistence(timeout: 2))
    }
}

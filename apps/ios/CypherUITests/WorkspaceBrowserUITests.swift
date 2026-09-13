import XCTest

final class WorkspaceBrowserUITests: XCTestCase {
    private func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testChangesPresentationStates() {
        let app = XCUIApplication()
        for appearance in ["light", "dark"] {
            app.launchArguments = ["-demo", "-route", "chat:chat-tabs", "-sheet", "changes",
                                   "-appAppearance", appearance]
            app.launch()
            let diff = app.webViews["workspace-diff-web"]
            let file = diff.buttons["Sources/Example.swift"]
            XCTAssertTrue(file.waitForExistence(timeout: 15))
            let message = diff.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Cypher")).firstMatch
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            // Allow asynchronous syntax paint, not just the initial plain rows.
            Thread.sleep(forTimeInterval: 1)
            capture(app, "\(appearance)-unified")
            let height = file.frame.height
            file.tap()
            let readme = diff.buttons["README.md"]
            XCTAssertTrue(readme.waitForExistence(timeout: 5))
            readme.tap()
            XCTAssertEqual(file.frame.height, height, accuracy: 1, "Disclosure must not resize the file header")
            capture(app, "\(appearance)-collapsed")
            file.tap()
            let expand = diff.buttons["Expand unchanged lines"].firstMatch
            XCTAssertTrue(expand.waitForExistence(timeout: 5))
            expand.tap()
            let unchanged = diff.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Foundation")).firstMatch
            XCTAssertTrue(unchanged.waitForExistence(timeout: 10))
            capture(app, "\(appearance)-context")
            app.buttons["workspace-diff-options"].tap()
            capture(app, "\(appearance)-options")
            app.buttons["Side by side"].tap()
            XCTAssertTrue(message.waitForExistence(timeout: 10))
            Thread.sleep(forTimeInterval: 1)
            capture(app, "\(appearance)-split")
            XCTAssertFalse(app.keyboards.firstMatch.exists)
            app.terminate()
        }
    }

    func testBrowseDemoFileAndPerFileDiffWithoutEditing() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        let entry = app.buttons["workspace-browser"]
        XCTAssertTrue(entry.waitForExistence(timeout: 10))
        entry.tap()
        app.buttons["Files"].tap()
        XCTAssertFalse(app.segmentedControls.firstMatch.exists)
        let folder = app.buttons["workspace-item-Sources"]
        XCTAssertTrue(folder.waitForExistence(timeout: 5))
        folder.tap()
        let source = app.buttons["workspace-item-Sources/Example.swift"]
        XCTAssertTrue(source.waitForExistence(timeout: 5))
        source.tap()
        let code = app.webViews["workspace-source-web"]
        XCTAssertTrue(code.waitForExistence(timeout: 5))
        XCTAssertTrue(code.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Foundation")).firstMatch.waitForExistence(timeout: 10))
        code.tap()
        XCTAssertFalse(app.keyboards.firstMatch.exists, "The file reader must never become an editor")
        app.navigationBars["Example.swift"].buttons.element(boundBy: 0).tap()
        app.navigationBars["Sources"].buttons.element(boundBy: 0).tap()
        let readme = app.buttons["workspace-item-README.md"]
        XCTAssertTrue(readme.waitForExistence(timeout: 5))
        readme.tap()
        XCTAssertTrue(code.waitForExistence(timeout: 5))
        XCTAssertTrue(code.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Read-only file browsing")).firstMatch.waitForExistence(timeout: 10))
        XCTAssertFalse(app.keyboards.firstMatch.exists)
        app.navigationBars["README.md"].buttons.element(boundBy: 0).tap()
        app.buttons["workspace-close"].tap()
        entry.tap()
        app.buttons["Changes"].tap()
        XCTAssertFalse(app.segmentedControls.firstMatch.exists)
        let diff = app.webViews["workspace-diff-web"]
        XCTAssertTrue(diff.waitForExistence(timeout: 10))
        let file = diff.buttons["Sources/Example.swift"]
        XCTAssertTrue(file.waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["workspace-diff-options"].waitForExistence(timeout: 5))
        let message = diff.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Cypher")).firstMatch
        XCTAssertTrue(message.waitForExistence(timeout: 15))
        let expand = diff.buttons["Expand unchanged lines"].firstMatch
        XCTAssertTrue(expand.waitForExistence(timeout: 5))
        expand.tap()
        let unchanged = diff.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Foundation")).firstMatch
        XCTAssertTrue(unchanged.waitForExistence(timeout: 15))
        app.buttons["workspace-diff-options"].tap()
        app.buttons["Side by side"].tap()
        XCTAssertTrue(message.waitForExistence(timeout: 15))
        XCTAssertFalse(app.keyboards.firstMatch.exists)
    }

    func testSourceReaderPresentationAndSelection() {
        let app = XCUIApplication()
        for appearance in ["light", "dark"] {
            app.launchArguments = ["-demo", "-route", "chat:chat-tabs", "-sheet", "files",
                                   "-appAppearance", appearance]
            app.launch()
            let folder = app.buttons["workspace-item-Sources"]
            XCTAssertTrue(folder.waitForExistence(timeout: 10))
            folder.tap()
            app.buttons["workspace-item-Sources/ReaderExample.swift"].tap()
            let web = app.webViews["workspace-source-web"]
            let word = web.staticTexts.matching(NSPredicate(format: "label CONTAINS %@", "Foundation")).firstMatch
            XCTAssertTrue(word.waitForExistence(timeout: 15))
            Thread.sleep(forTimeInterval: 1)
            capture(app, "\(appearance)-source")
            word.press(forDuration: 1.2)
            let copy = app.menuItems["Copy"].exists ? app.menuItems["Copy"] : app.buttons["Copy"]
            XCTAssertTrue(copy.waitForExistence(timeout: 5))
            capture(app, "\(appearance)-source-selection")
            if copy.exists { copy.tap() }
            XCTAssertFalse(app.keyboards.firstMatch.exists)
            if appearance == "light" {
                // Paste back into this app's directory filter, not a test-runner
                // pasteboard read (which races Simulator/host clipboard sync).
                app.navigationBars["ReaderExample.swift"].buttons.element(boundBy: 0).tap()
                let filter = app.searchFields.firstMatch
                XCTAssertTrue(filter.waitForExistence(timeout: 5))
                filter.tap()
                filter.press(forDuration: 1)
                let paste = app.menuItems["Paste"].exists ? app.menuItems["Paste"] : app.buttons["Paste"]
                XCTAssertTrue(paste.waitForExistence(timeout: 5))
                if paste.exists { paste.tap() }
                if app.buttons["Allow Paste"].exists { app.buttons["Allow Paste"].tap() }
                XCTAssertEqual(filter.value as? String, "Foundation", "Copy must exclude line numbers and unselected code")
                filter.buttons["Clear text"].tap()
                if app.buttons["Cancel"].exists { app.buttons["Cancel"].tap() }
                app.buttons["workspace-item-Sources/ReaderExample.swift"].tap()
                XCTAssertTrue(word.waitForExistence(timeout: 15))
            }
            app.buttons["workspace-code-options"].tap()
            capture(app, "\(appearance)-source-options")
            app.buttons["Wrap lines"].tap()
            Thread.sleep(forTimeInterval: 1)
            capture(app, "\(appearance)-source-wrap")
            app.buttons["workspace-code-options"].tap()
            capture(app, "\(appearance)-source-options-wrapped")
            app.buttons["Plain text preview"].tap()
            let plain = app.textViews["workspace-code"]
            XCTAssertTrue(plain.waitForExistence(timeout: 5))
            XCTAssertTrue((plain.value as? String)?.contains("struct WorkspaceSummary") == true)
            capture(app, "\(appearance)-source-plain")
            app.buttons["workspace-code-options"].tap()
            app.buttons["Syntax highlighting"].tap()
            XCTAssertTrue(word.waitForExistence(timeout: 15))
            XCTAssertFalse(app.keyboards.firstMatch.exists)
            app.terminate()
        }
    }
}

import XCTest

final class ComposerFocusUITests: XCTestCase {
    func testRepeatedBottomDragsLeaveTheTailVisibleAndComposerUsable() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        let transcript = app.scrollViews.matching(identifier: "chat-transcript").firstMatch
        let tail = app.descendants(matching: .any).matching(identifier: "steer-label").firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        XCTAssertTrue(tail.waitForExistence(timeout: 10))
        let start = transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.8))
        let end = transcript.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.25))
        for _ in 0..<5 {
            start.press(forDuration: 0.02, thenDragTo: end, withVelocity: .fast, thenHoldForDuration: 0)
        }
        // This checks the endpoint, not frame-by-frame spring smoothness:
        // XCUITest may itself wait for quiescence between gestures.
        XCTAssertTrue(tail.isHittable, "Bottom overscroll must not leave a blank/stranded viewport")
        let editor = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        editor.tap()
        let controls = app.descendants(matching: .any).matching(identifier: "composer-model-controls").firstMatch
        XCTAssertTrue(controls.waitForExistence(timeout: 5))
        editor.typeText("hi")
        XCTAssertTrue(controls.exists)
    }

    func testDemoSteerHasAQuietLabel() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        let label = app.descendants(matching: .any).matching(identifier: "steer-label").firstMatch
        XCTAssertTrue(label.waitForExistence(timeout: 10))
        XCTAssertTrue(label.isHittable, "The example steer should be visible at the transcript tail")
    }

    func testRealTapExpandsEmptyComposerAndShortDraftCollapsesOnBlur() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        let editor = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'composer-editor-'")).firstMatch
        let controls = app.descendants(matching: .any)
            .matching(identifier: "composer-model-controls").firstMatch
        XCTAssertTrue(editor.waitForExistence(timeout: 10))
        XCTAssertFalse(controls.exists)
        // Do not replace this with becomeFirstResponder: the hosted unit-test
        // fixture passed before this real-tap regression was fixed.
        editor.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(controls.waitForExistence(timeout: 5))
        editor.typeText("a")
        XCTAssertTrue(controls.exists, "One character must show the toolbar, not only long drafts")
        editor.typeText(XCUIKeyboardKey.delete.rawValue)
        XCTAssertTrue(controls.exists, "Deleting back to empty while focused must keep the toolbar")
        editor.typeText("hi")
        XCTAssertTrue(controls.exists, "Short input must not hide the toolbar")
        // Tap the transcript, not the composer or a sheet search field.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.15, dy: 0.3)).tap()
        XCTAssertTrue(controls.waitForNonExistence(timeout: 5))
        editor.tap()
        XCTAssertTrue(controls.waitForExistence(timeout: 5))
    }
}

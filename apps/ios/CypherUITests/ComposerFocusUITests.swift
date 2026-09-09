import XCTest

final class ComposerFocusUITests: XCTestCase {
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
        editor.typeText("hi")
        XCTAssertTrue(controls.exists, "Short input must not hide the toolbar")
        // Tap the transcript, not the composer or a sheet search field.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.15, dy: 0.3)).tap()
        XCTAssertTrue(controls.waitForNonExistence(timeout: 5))
        editor.tap()
        XCTAssertTrue(controls.waitForExistence(timeout: 5))
    }
}

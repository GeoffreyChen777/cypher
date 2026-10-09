import XCTest

/// A drag selection in a reply crosses its paragraphs and list items: they
/// are one selectable text. Real touches on the offline demo.
final class ProseSelectionUITests: XCTestCase {
    func testADragSelectionCrossesAParagraphIntoAList() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-veil"]
        app.launch()
        // The reply's heading, paragraph and list are one text view.
        let prose = app.textViews.matching(NSPredicate(format: "value CONTAINS %@", "The desktop veil")).firstMatch
        let transcript = app.descendants(matching: .any).matching(identifier: "chat-transcript").firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        for _ in 0..<6 where !prose.isHittable { transcript.swipeDown(velocity: .slow) }
        XCTAssertTrue(prose.waitForHittable(timeout: 5))
        let value = prose.value as? String ?? ""
        XCTAssertTrue(value.contains("Veil port plan") && value.contains("Chunk spans keep"),
                      "the heading, paragraph and list share one text: \(value.prefix(120))")

        // From "desktop" in the paragraph's first line down into the first
        // list item, past "Chunk spans": heading 27pt, the 12pt gap, four
        // 22pt lines, the gap, then the item at 150. iOS reads a dragged
        // selection's end about a line above the finger, so the finger goes
        // a line lower. Slowly, holding at the end, as a finger does.
        let origin = prose.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
        let start = origin.withOffset(CGVector(dx: 40, dy: 50))
        let end = origin.withOffset(CGVector(dx: 150, dy: 172))
        start.press(forDuration: 1.0, thenDragTo: end, withVelocity: 150, thenHoldForDuration: 0.6)
        capture(app, "prose-selection")

        let comment = app.menuItems["Comment"].exists ? app.menuItems["Comment"] : app.buttons["Comment"]
        XCTAssertTrue(comment.waitForExistence(timeout: 5), "the selection's menu offers Comment")
        comment.tap()
        let quote = app.staticTexts.matching(NSPredicate(format: "label CONTAINS %@ AND label CONTAINS %@",
                                                         "desktop", "Chunk spans")).firstMatch
        XCTAssertTrue(quote.waitForExistence(timeout: 5), "the quote runs from the paragraph into the list")
        capture(app, "prose-selection-quote")
    }
}

import XCTest

/// The tool call cap by real taps on the offline demo (`chat-tools`): an open
/// group shows its last five calls under a row that shows the earlier ones,
/// and a work run folds its start the same way.
@MainActor
final class ToolCallCapUITests: XCTestCase {
    private func text(_ app: XCUIApplication, containing fragment: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", fragment, fragment))
            .firstMatch
    }

    private func count(_ app: XCUIApplication, _ identifier: String) -> Int {
        app.descendants(matching: .any).matching(identifier: identifier).count
    }

    /// The row over the shown calls, once it reads `label`. Showing earlier
    /// calls grows the reply upward from the transcript's pinned bottom,
    /// which can carry the row off the top: scroll back to it.
    private func overflow(_ app: XCUIApplication, reading label: String) -> XCUIElement {
        let row = app.buttons["tool-overflow"]
        let transcript = app.descendants(matching: .any).matching(identifier: "chat-transcript").firstMatch
        for _ in 0..<3 where !row.waitForHittable(timeout: 2) { transcript.swipeDown() }
        XCTAssertTrue(row.waitForHittable(timeout: 5), "the row over the chips")
        XCTAssertTrue(row.wait(until: "label == '\(label)'", timeout: 5), "reads \(row.label), not \(label)")
        return row
    }

    func testAGroupShowsItsLastFiveCallsAndTheRowShowsTheRest() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-tools"])
        let group = app.buttons["tool-group-toggle"]
        XCTAssertTrue(group.waitForHittable(timeout: 10))
        XCTAssertTrue(group.label.contains("Ran 5 commands · edited 4 files · read 2 files"), group.label)
        XCTAssertEqual(group.value as? String, "Collapsed")
        XCTAssertEqual(count(app, "tool-overflow"), 0, "a closed group draws no row")
        group.tap()

        // Twelve calls: the last five, the ones the agent just ran.
        var row = overflow(app, reading: "Show 7 earlier tool calls")
        XCTAssertEqual(row.value as? String, "Collapsed")
        XCTAssertEqual(count(app, "tool-status"), 5)
        XCTAssertTrue(text(app, containing: "cargo test --locked").exists)
        XCTAssertTrue(text(app, containing: "cargo fmt --all --check").exists)
        XCTAssertFalse(text(app, containing: "edge/package.json").exists, "the sixth call is folded")
        capture(app, "group-capped")

        row.tap()
        row = overflow(app, reading: "Show fewer tool calls")
        XCTAssertEqual(row.value as? String, "Expanded")
        XCTAssertTrue(text(app, containing: "git log --oneline").exists, "the first call shows")
        XCTAssertTrue(text(app, containing: "edge/package.json").exists)
        XCTAssertGreaterThan(count(app, "tool-status"), 5)
        capture(app, "group-revealed")

        // The same row folds them back.
        row.tap()
        _ = overflow(app, reading: "Show 7 earlier tool calls")
        XCTAssertEqual(count(app, "tool-status"), 5)
        XCTAssertFalse(text(app, containing: "git log --oneline").exists)

        // Closed, the group draws neither.
        group.tap()
        XCTAssertTrue(app.buttons["tool-overflow"].waitForNonExistence(timeout: 5))
        XCTAssertEqual(count(app, "tool-status"), 0)
    }

    func testAWorkRunFoldsItsStartBehindOneRow() {
        let app = XCUIApplication.launchDemo(["-route", "chat:chat-tools"])
        let run = app.buttons["activity-toggle"]
        XCTAssertTrue(run.waitForHittable(timeout: 10))
        XCTAssertTrue(run.label.contains("Ran 9 commands · 3 thoughts"), run.label)
        run.tap()

        // Nine calls between three thoughts: the last five calls and the
        // thought among them.
        var row = overflow(app, reading: "Show 4 earlier tool calls and 2 thoughts")
        XCTAssertEqual(count(app, "tool-status"), 5)
        XCTAssertEqual(count(app, "thought-chip"), 1)
        XCTAssertTrue(text(app, containing: "Tag the platforms").exists)
        XCTAssertTrue(text(app, containing: "git push origin main").exists, "the first shown call")
        XCTAssertFalse(text(app, containing: "git commit").exists, "the call before it is folded")
        capture(app, "run-capped")

        row.tap()
        row = overflow(app, reading: "Show fewer tool calls")
        XCTAssertTrue(text(app, containing: "Check the tree first").exists, "the first thought shows")
        XCTAssertTrue(text(app, containing: "git status --short").exists)
        XCTAssertTrue(text(app, containing: "git commit").exists)
        capture(app, "run-revealed")

        row.tap()
        _ = overflow(app, reading: "Show 4 earlier tool calls and 2 thoughts")
        XCTAssertEqual(count(app, "tool-status"), 5)
        XCTAssertEqual(count(app, "thought-chip"), 1)
    }
}

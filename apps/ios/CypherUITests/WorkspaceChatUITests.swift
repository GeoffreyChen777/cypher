import XCTest

/// The workspace chat end to end on the demo: Home's Workspaces row opens
/// Atlas's chat, its plan awaiting approval shows as a card, and Approve
/// turns it into running work (the card's Stop replaces its decision).
@MainActor
final class WorkspaceChatUITests: XCTestCase {
    func testApprovingThePlanCardStartsTheWork() {
        let app = XCUIApplication.launchDemo()
        let row = app.descendants(matching: .any).matching(identifier: "workspace-row-bot-atlas").firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), "Home lists the workspace")
        row.tap()

        XCTAssertTrue(app.buttons["workspace-board"].waitForExistence(timeout: 10), "the workspace's toolbar")
        let approve = app.buttons["card-approve"]
        XCTAssertTrue(approve.waitForExistence(timeout: 10), "the plan awaiting approval")
        XCTAssertEqual(app.buttons.matching(identifier: "card-approve").count, 1)
        // The card's head reads its title and status as one.
        func head(_ status: String) -> XCUIElementQuery {
            app.descendants(matching: .any).matching(
                NSPredicate(format: "label CONTAINS 'Fix invoice rounding' AND label CONTAINS %@", status))
        }
        XCTAssertTrue(head("Awaiting approval").firstMatch.waitForExistence(timeout: 5))
        // The awaiting plan sits above the running one: scroll it into reach.
        let window = app.windows.firstMatch
        for _ in 0..<8 where !approve.isHittable {
            window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
                .press(
                    forDuration: 0.05, thenDragTo: window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.6)))
        }
        XCTAssertTrue(approve.isHittable)
        capture(app, "plan-awaiting")
        approve.tap()

        let gone = NSPredicate(format: "count == 0")
        expectation(for: gone, evaluatedWith: app.buttons.matching(identifier: "card-approve"))
        waitForExpectations(timeout: 10)
        XCTAssertTrue(head("Running").firstMatch.waitForExistence(timeout: 10), "the host started it")
        XCTAssertFalse(head("Awaiting approval").firstMatch.exists)
        capture(app, "plan-approved")
    }

    /// The board opens at half height on its overview and grows to full
    /// height for a page that holds a decision.
    func testTheBoardGrowsForADecision() {
        let app = XCUIApplication.launchDemo(["-route", "bot:bot-atlas"])
        let board = app.buttons["workspace-board"]
        XCTAssertTrue(board.waitForExistence(timeout: 10))
        board.tap()
        let close = app.buttons["board-close"]
        XCTAssertTrue(close.waitForExistence(timeout: 5))
        let height = app.windows.firstMatch.frame.height
        XCTAssertGreaterThan(close.frame.minY, height * 0.3, "half height")

        let approval = app.staticTexts.matching(
            NSPredicate(format: "label BEGINSWITH 'Start a session in webapp'")
        ).firstMatch
        XCTAssertTrue(approval.waitForExistence(timeout: 5))
        approval.tap()
        let bar = app.navigationBars["Approval"]
        XCTAssertTrue(bar.waitForExistence(timeout: 5))
        // The detent animates: give it a moment.
        let deadline = Date().addingTimeInterval(3)
        while bar.frame.minY >= height * 0.2, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.2)
        }
        XCTAssertLessThan(bar.frame.minY, height * 0.2, "full height")
        capture(app, "board-approval")
    }
}

import XCTest

/// The workspace board's decisions on the demo dataset: each page opens
/// from the launch rig, takes its action and shows the host's answer.
@MainActor
final class WorkspaceBoardUITests: XCTestCase {
    /// Launch to a settled sheet; generous for a busy simulator.
    static let launch: TimeInterval = 30

    override func setUp() {
        continueAfterFailure = false
    }

    private func board(_ page: String) -> XCUIApplication {
        XCUIApplication.launchDemo(["-route", "bot:bot-atlas", "-sheet", "board:\(page)"])
    }

    func testApprovingAnActionSettlesIt() {
        let app = board("approval:approval-staging")
        let approve = app.buttons["decision-approve"]
        XCTAssertTrue(approve.waitForHittable(timeout: Self.launch))
        let status = app.descendants(matching: .any)["approval-status"]
        XCTAssertTrue(status.waitForExistence(timeout: 5))
        XCTAssertTrue(status.label.contains("Awaiting your approval"), status.label)
        approve.tap()
        XCTAssertTrue(status.wait(until: "label CONTAINS 'Approved'", timeout: 15), status.label)
        capture(app, "approval-approved")
    }

    func testARowsIconButtonsDecideInPlaceWithoutOpeningTheRow() {
        let app = board("overview")
        let approve = app.buttons["row-approve-approval:approval-staging"]
        XCTAssertTrue(approve.waitForHittable(timeout: Self.launch), "Approve is a button on the row")
        XCTAssertTrue(app.buttons["row-decline-approval:approval-staging"].isHittable, "so is Decline")
        approve.tap()
        // Decided where it stands: the row leaves Needs you and the board
        // stays on the overview (the tap didn't open the approval).
        XCTAssertTrue(approve.wait(until: "exists == false", timeout: 15), "the approved row leaves Needs you")
        XCTAssertFalse(
            app.descendants(matching: .any)["approval-status"].waitForExistence(timeout: 3),
            "the icon's tap is the button's alone, not the row's")
        XCTAssertTrue(app.descendants(matching: .any)["board-needs-header"].exists)
        capture(app, "row-approved")
    }

    func testAPermissionCardShowsEveryOptionAsAButton() {
        let app = XCUIApplication.launchDemo([
            "-demo-needs", "-route", "bot:bot-atlas", "-sheet", "board:asking:work-deps:chat-atlas-w6",
        ])
        let primary = app.buttons["permission-option-0"]
        XCTAssertTrue(primary.waitForHittable(timeout: Self.launch))
        XCTAssertEqual(primary.label, "Run once")
        // Allow for this chat, Always allow, Deny, Deny and stop: each on
        // screen, none behind a menu.
        for index in 1...4 {
            XCTAssertTrue(app.buttons["permission-option-\(index)"].isHittable, "option \(index) is on screen")
        }
        XCTAssertEqual(app.buttons["permission-option-4"].label, "Deny and stop")
        XCTAssertFalse(app.buttons["permission-more"].exists, "no More menu")
        capture(app, "permission-options")
    }

    func testReviewingAnAnswerEditsAndSendsIt() {
        let app = board("exchange:ex-review")
        let answer = app.textViews["exchange-answer"]
        XCTAssertTrue(answer.waitForHittable(timeout: Self.launch), "The drafted answer is on screen")
        answer.tap()
        // The software keyboard depends on the simulator's hardware keyboard
        // setting; focus doesn't.
        XCTAssertTrue(answer.wait(until: "hasKeyboardFocus == true", timeout: 5), "Tapping the answer edits it")
        answer.typeText(" Thanks!")
        let send = app.buttons["decision-send"]
        XCTAssertTrue(send.isHittable, "The keyboard must not cover Send")
        capture(app, "review-editing")
        send.tap()
        let status = app.descendants(matching: .any)["exchange-status"]
        XCTAssertTrue(status.waitForExistence(timeout: 5))
        XCTAssertTrue(status.wait(until: "label CONTAINS 'Sent'", timeout: 5), status.label)
        let sent = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'Thanks!'")).firstMatch
        XCTAssertTrue(sent.waitForExistence(timeout: 3), "The sent answer is the edited one")
    }

    func testRunNowAndANewMemoryFile() {
        let app = board("routine:routine-triage")
        let run = app.buttons["decision-run now"]
        XCTAssertTrue(run.waitForHittable(timeout: Self.launch))
        run.tap()
        let status = app.descendants(matching: .any)["routine-status"]
        XCTAssertTrue(status.wait(until: "label CONTAINS 'Running'", timeout: 6), status.label)
        app.terminate()

        let memory = board("memory")
        let add = memory.buttons["memory-new-file"]
        XCTAssertTrue(add.waitForHittable(timeout: Self.launch))
        add.tap()
        let name = memory.textFields["memory-editor-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        name.tap()
        name.typeText("ui-test-notes")
        let text = memory.textViews["memory-editor-text"]
        text.tap()
        text.typeText("Written by the board UI test.")
        memory.buttons["memory-editor-save"].tap()
        // The new file opens on top of the store.
        XCTAssertTrue(memory.staticTexts["ui-test-notes.md"].waitForExistence(timeout: 5))
        // Markdown renders as selectable text views.
        let body = memory.descendants(matching: .any).matching(
            NSPredicate(format: "label == 'Written by the board UI test.'")
        ).firstMatch
        XCTAssertTrue(body.waitForExistence(timeout: 3))
    }
}

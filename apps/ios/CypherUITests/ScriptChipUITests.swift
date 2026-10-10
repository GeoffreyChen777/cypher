import XCTest

/// A Pi codemode script in the transcript, by real taps on the offline demo:
/// its calls nest under it and its chip opens onto the code. Every chip shows
/// its call's status.
@MainActor
final class ScriptChipUITests: XCTestCase {
    private func text(_ app: XCUIApplication, containing fragment: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", fragment, fragment))
            .firstMatch
    }

    private func status(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: "tool-status")
            .matching(NSPredicate(format: "label == %@", label)).firstMatch
    }

    func testChipsShowTheirStatus() {
        let app = XCUIApplication()
        app.launchArguments = ["-demo", "-route", "chat:chat-tabs"]
        app.launch()
        // The group sits above the session's later exchanges.
        let group = text(app, containing: "1 failed")
        let transcript = app.descendants(matching: .any).matching(identifier: "chat-transcript").firstMatch
        XCTAssertTrue(transcript.waitForExistence(timeout: 10))
        for _ in 0..<4 where !group.waitForHittable(timeout: 2) { transcript.swipeDown() }
        XCTAssertTrue(group.waitForHittable(timeout: 5))
        group.tap()
        XCTAssertTrue(status(app, "Failed").waitForExistence(timeout: 5), "the failed command shows a cross")
        XCTAssertTrue(status(app, "Completed").exists, "the others show a check")
        XCTAssertFalse(status(app, "Running").exists)
    }

    func testLiveCallsSpinUntilTheyResolve() {
        let app = XCUIApplication()
        // `-stream` starts the demo's scripted reply, which opens with a
        // script and the call it makes.
        app.launchArguments = ["-demo", "-route", "chat:chat-deploy", "-stream", "-appAppearance", "dark"]
        app.launch()
        let running = text(app, containing: "Running")
        XCTAssertTrue(running.waitForExistence(timeout: 10), "a live call shows the spinner")
        capture(app, "dark-running")
        XCTAssertTrue(running.waitForNonExistence(timeout: 10), "and stops once the calls resolve")
    }

    func testScriptChipOpensOntoItsCode() {
        let app = XCUIApplication()
        for appearance in ["light", "dark"] {
            app.launchArguments = ["-demo", "-route", "chat:chat-deploy", "-appAppearance", appearance]
            app.launch()
            let group = text(app, containing: "Ran 1 script")
            XCTAssertTrue(group.waitForHittable(timeout: 10), "the group counts the script with the commands")
            group.tap()

            let script = app.buttons["script-chip"]
            XCTAssertTrue(script.waitForHittable(timeout: 5))
            XCTAssertTrue(text(app, containing: "read, search_cloudflare_documentation").exists,
                          "the chip names the tools the script calls")
            XCTAssertTrue(text(app, containing: "edge/wrangler.jsonc").exists, "the script's calls are listed")
            XCTAssertFalse(text(app, containing: "const config").exists, "the code starts folded")

            script.tap()
            XCTAssertTrue(text(app, containing: "const config").waitForExistence(timeout: 5),
                          "the chip opens onto the script")
            Thread.sleep(forTimeInterval: 0.5)
            capture(app, "\(appearance)-script-open")

            script.tap()
            XCTAssertTrue(text(app, containing: "const config").waitForNonExistence(timeout: 5))
        }
    }
}

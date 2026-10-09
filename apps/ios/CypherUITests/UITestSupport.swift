import XCTest

extension XCUIElement {
    /// Waits until `predicate` (an NSPredicate format over this element)
    /// holds, or `timeout` passes.
    func wait(until predicate: String, timeout: TimeInterval) -> Bool {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate(format: predicate), object: self)
        return XCTWaiter.wait(for: [expectation], timeout: timeout) == .completed
    }

    /// The transcript exists (hidden behind its skeleton) from the first frame
    /// and is revealed only once it has settled at the bottom — so "visible"
    /// assertions must wait for hittability, not just existence.
    func waitForHittable(timeout: TimeInterval) -> Bool {
        wait(until: "exists == true AND hittable == true", timeout: timeout)
    }

    func waitForSelected(timeout: TimeInterval) -> Bool {
        wait(until: "selected == true", timeout: timeout)
    }

    func waitForEnabled(timeout: TimeInterval) -> Bool {
        wait(until: "enabled == true", timeout: timeout)
    }
}

extension XCTestCase {
    /// Attaches a screenshot of `app`, kept even when the test passes.
    func capture(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

extension XCUIApplication {
    /// Launches the app on the in-memory demo dataset with `arguments`.
    static func launchDemo(_ arguments: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-demo"] + arguments
        app.launch()
        return app
    }
}

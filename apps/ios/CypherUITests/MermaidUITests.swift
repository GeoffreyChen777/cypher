import XCTest

/// ```mermaid fences in the offline demo's [mock:mermaid] showcase: drawn as
/// diagrams, switchable to their source and back by real taps.
@MainActor
final class MermaidUITests: XCTestCase {
    private func elements(_ app: XCUIApplication, _ id: String) -> XCUIElementQuery {
        app.descendants(matching: .any).matching(identifier: id)
    }

    private func hittable(_ query: XCUIElementQuery) -> XCUIElement? {
        query.allElementsBoundByIndex.last { $0.exists && $0.isHittable }
    }

    func testDiagramsDrawAndSwitchToTheirSource() throws {
        for appearance in ["light", "dark"] {
            let app = XCUIApplication.launchDemo(["-route", "chat:chat-mermaid", "-appAppearance", appearance])
            // The first diagram on screen: the class diagram in the list.
            let diagram = elements(app, "mermaid-diagram").firstMatch
            XCTAssertTrue(diagram.waitForHittable(timeout: 60), "the diagram draws")
            capture(app, "\(appearance)-diagram")

            // (Diagram counts are no measure: the lazy transcript loads the
            // rows a shorter block brings on screen.)
            XCTAssertFalse(elements(app, "mermaid-source").firstMatch.exists)
            try XCTUnwrap(hittable(elements(app, "mermaid-show-source"))).tap()
            XCTAssertTrue(elements(app, "mermaid-source").firstMatch.waitForExistence(timeout: 5))
            capture(app, "\(appearance)-source")

            try XCTUnwrap(hittable(elements(app, "mermaid-show-diagram"))).tap()
            XCTAssertTrue(
                elements(app, "mermaid-source").firstMatch.wait(until: "exists == false", timeout: 5))

            // The diagrams above it drew too.
            app.swipeDown(velocity: .fast)
            app.swipeDown(velocity: .fast)
            Thread.sleep(forTimeInterval: 2)
            capture(app, "\(appearance)-top")
            app.terminate()
        }
    }
}

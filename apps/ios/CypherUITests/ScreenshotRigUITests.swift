import XCTest

/// Development screenshot rig, skipped unless asked for: writes a PNG of each
/// demo screen named in `CYPHER_SHOTS` to `CYPHER_SHOT_DIR` (both passed to
/// the test runner as `TEST_RUNNER_CYPHER_SHOTS` / `TEST_RUNNER_CYPHER_SHOT_DIR`).
///
/// `CYPHER_SHOTS` is `name=arguments` entries separated by `;`, the arguments
/// space-separated launch arguments after `-demo`, e.g.
/// `home=;dark-chat=-route chat:chat-veil -appAppearance dark`. Words
/// starting with `@` are gestures run in order once the screen has settled,
/// not launch arguments: `@swipeUp`, `@swipeDown`, `@tap:<identifier>`,
/// `@press:<identifier>` (a long press: opens the element's context menu,
/// which a held `@drag` in place does not), `@wait`, and
/// `@drag:<x1>,<y1>,<x2>,<y2>,<hold>` — a touch held `hold` seconds at the
/// first point, then dragged to the second, in points from the screen's top
/// left: a hold of 1 selects text from there, 0.1 scrolls.
final class ScreenshotRigUITests: XCTestCase {
    @MainActor
    func testWritesTheRequestedScreens() throws {
        let env = ProcessInfo.processInfo.environment
        guard let shots = env["CYPHER_SHOTS"], let dir = env["CYPHER_SHOT_DIR"] else {
            throw XCTSkip("Screenshot rig: set CYPHER_SHOTS and CYPHER_SHOT_DIR to use it.")
        }
        let settle = Double(env["CYPHER_SHOT_SETTLE"] ?? "") ?? 2.5
        for entry in shots.split(separator: ";") {
            let parts = entry.split(separator: "=", maxSplits: 1).map(String.init)
            guard let name = parts.first, !name.isEmpty else { continue }
            let words = (parts.count > 1 ? parts[1] : "").split(separator: " ").map(String.init)
            let app = XCUIApplication()
            app.launchArguments = ["-demo"] + words.filter { !$0.hasPrefix("@") }
            app.launch()
            Thread.sleep(forTimeInterval: settle)
            for gesture in words where gesture.hasPrefix("@") {
                switch gesture {
                case "@swipeUp": app.swipeUp()
                case "@swipeDown": app.swipeDown()
                case "@wait": Thread.sleep(forTimeInterval: settle)
                default:
                    if gesture.hasPrefix("@tap:") {
                        let element = app.descendants(matching: .any)[String(gesture.dropFirst(5))].firstMatch
                        if element.waitForExistence(timeout: 5) { element.tap() }
                    } else if gesture.hasPrefix("@press:") {
                        let element = app.descendants(matching: .any)[String(gesture.dropFirst(7))].firstMatch
                        if element.waitForExistence(timeout: 5) { element.press(forDuration: 1.2) }
                    } else if gesture.hasPrefix("@drag:") {
                        let v = gesture.dropFirst(6).split(separator: ",").compactMap { Double($0) }
                        guard v.count == 5 else { continue }
                        let origin = app.coordinate(withNormalizedOffset: .zero)
                        origin.withOffset(CGVector(dx: v[0], dy: v[1])).press(
                            forDuration: v[4], thenDragTo: origin.withOffset(CGVector(dx: v[2], dy: v[3])),
                            withVelocity: v[4] < 0.5 ? .fast : 150, thenHoldForDuration: 0.6)
                    }
                }
                Thread.sleep(forTimeInterval: 1)
            }
            let url = URL(fileURLWithPath: dir).appendingPathComponent("\(name).png")
            try XCUIScreen.main.screenshot().pngRepresentation.write(to: url)
            app.terminate()
        }
    }
}

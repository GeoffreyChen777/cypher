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
/// `@wait`.
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

import XCTest
@testable import Cypher

@MainActor
final class BootSplashTests: XCTestCase {
    func testWordmarkMatchesDesktopAsset() {
        XCTAssertEqual(BootSplash.wordmark.count, 6)
        XCTAssertEqual(BootSplash.wordmark[0], String(repeating: " ", count: 24) + "_")
        XCTAssertEqual(BootSplash.wordmark[2], #" / __| | | | | | '_ \  | '_ \   / _ \ | '__|"#)
        XCTAssertEqual(BootSplash.wordmark[5], "        |___/  |_|")
    }

    func testDecodeSweepsFromBlankThroughNoiseToInk() {
        // First glyph of row 1 (col 2) and last glyph of row 2.
        let lastCol = BootSplash.wordmark[2].count - 1
        XCTAssertEqual(BootSplash.glyph(row: 1, col: 2, char: "_", at: 0), .blank)
        XCTAssertEqual(BootSplash.glyph(row: 2, col: lastCol, char: "|", at: 0.2), .blank)
        guard case .scrambled = BootSplash.glyph(row: 1, col: 2, char: "_", at: 0.1) else {
            return XCTFail("left edge should be scrambling while the right edge is still blank")
        }
        for row in BootSplash.wordmark.indices {
            for (col, char) in BootSplash.wordmark[row].enumerated() where char != " " {
                XCTAssertEqual(BootSplash.glyph(row: row, col: col, char: char, at: BootSplash.decodeDuration),
                               .locked(flash: 0), "row \(row) col \(col) not settled")
            }
        }
    }

    func testLaunchStallPausesTheDecodeInsteadOfSkippingIt() {
        // A 2s main-thread stall counts as one frame, not as the whole decode.
        let afterStall = SplashClock.advance(0, by: 2.0)
        XCTAssertEqual(afterStall, SplashClock.maxStep, accuracy: 1e-9)
        XCTAssertLessThan(afterStall, BootSplash.decodeDuration)
        XCTAssertEqual(SplashClock.advance(0.1, by: 1.0 / 120), 0.1 + 1.0 / 120, accuracy: 1e-9)
        XCTAssertEqual(SplashClock.advance(0.1, by: -1), 0.1, "timestamps never run backwards")
    }

    func testSpacesNeverScramble() {
        for t in stride(from: 0.0, through: 1.0, by: 0.05) {
            XCTAssertEqual(BootSplash.glyph(row: 3, col: 5, char: " ", at: t), .blank)
        }
    }

    func testRigsSkipTheSplash() {
        XCTAssertTrue(BootSplash.enabled(arguments: ["Cypher"], environment: [:]))
        XCTAssertFalse(BootSplash.enabled(arguments: ["Cypher", "-demo"], environment: [:]))
        XCTAssertFalse(BootSplash.enabled(arguments: ["Cypher", "-e2e"], environment: [:]))
        XCTAssertFalse(BootSplash.enabled(arguments: ["Cypher"], environment: ["XCTestConfigurationFilePath": "x"]))
        XCTAssertTrue(BootSplash.enabled(arguments: ["Cypher", "-demo", "-splash"], environment: [:]))
    }

    func testWaitsForRealContent() {
        func ready(restored: Bool = true, _ phase: AppModel.Phase,
                   demo: Bool = false, connected: Bool = false, hasRows: Bool = false) -> Bool {
            BootSplash.contentReady(restored: restored, phase: phase, demo: demo,
                                    connected: connected, hasRows: hasRows)
        }
        XCTAssertFalse(ready(restored: false, .signedOut), "initial phase isn't a decision")
        XCTAssertTrue(ready(.signedOut))
        XCTAssertFalse(ready(.ready), "first launch after sign-in: no cache, no room yet")
        XCTAssertTrue(ready(.ready, hasRows: true), "local-first cache is enough")
        XCTAssertTrue(ready(.ready, connected: true))
        XCTAssertTrue(ready(.ready, demo: true))
    }
}

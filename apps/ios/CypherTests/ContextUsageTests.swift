import XCTest
@testable import Cypher

/// The composer's context ring: token counts and lenient decoding
/// (context_ring.rs test vectors).
@MainActor
final class ContextUsageTests: XCTestCase {
    func testTokenCountsMatchTheDesktop() {
        // context_ring.rs test vectors.
        XCTAssertEqual(ContextUsage.formatTokens(950), "950")
        XCTAssertEqual(ContextUsage.formatTokens(1_000), "1k")
        XCTAssertEqual(ContextUsage.formatTokens(9_540), "9.5k")
        XCTAssertEqual(ContextUsage.formatTokens(124_400), "124k")
        XCTAssertEqual(ContextUsage.formatTokens(200_000), "200k")
        XCTAssertEqual(ContextUsage.formatTokens(1_000_000), "1M")
        XCTAssertEqual(ContextUsage.formatTokens(1_250_000), "1.2M")
        XCTAssertEqual(ContextUsage(used: 124_000, size: 200_000).summary, "62% context used · 124k / 200k")
        XCTAssertEqual(ContextUsage(used: 250_000, size: 200_000).fraction, 1)
        XCTAssertEqual(ContextUsage(used: 5, size: 0).fraction, 0)
    }

    func testContextUsageDecodesLeniently() {
        XCTAssertEqual(ContextUsage(.object(["used": .int(10), "size": .double(20)])),
                       ContextUsage(used: 10, size: 20))
        XCTAssertNil(ContextUsage(.object(["used": .string("x"), "size": .int(20)])))
        XCTAssertNil(ContextUsage(nil))
        XCTAssertNil(ContextUsage(.null))
    }
}

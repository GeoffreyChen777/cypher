import XCTest
@testable import Cypher

final class NotificationBadgeTests: XCTestCase {
    func testBadgeOnlyAndAlertPayloadsCarryTheSameSnapshot() {
        for kind in ["badge", "completed"] {
            let fields: [String: Any] = ["version": 1, "scope": String(repeating: "a", count: 64),
                "kind": kind, "badgeCount": 3, "badgeRevision": 12]
            XCTAssertEqual(NotificationBadge.parse(["cypher": fields]),
                           NotificationBadge(scope: String(repeating: "a", count: 64), badgeCount: 3, badgeRevision: 12))
        }
    }

    func testZeroClearsAndMalformedOrLegacyPayloadsAreIgnored() {
        let fields: [String: Any] = ["version": 1, "scope": String(repeating: "a", count: 64),
                                   "badgeCount": 0, "badgeRevision": 1]
        XCTAssertEqual(NotificationBadge.parse(["cypher": fields])?.badgeCount, 0)
        for (key, value): (String, Any) in [
            ("badgeCount", -1), ("badgeCount", 1.5), ("badgeCount", true),
            ("badgeRevision", -1), ("scope", "bad"), ("version", 2)
        ] {
            var bad = fields
            bad[key] = value
            XCTAssertNil(NotificationBadge.parse(["cypher": bad]))
        }
        XCTAssertNil(NotificationBadge.parse(["cypher": ["version": 1]]))
    }
}

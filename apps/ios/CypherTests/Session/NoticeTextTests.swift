// Extension notices (proto notice.rs): the strip's compact reading and the
// old-transcript filter on the shared vectors in
// protocol/vectors/notice-text-v1.json (crates/proto/tests/notice_text_vectors.rs
// runs the same file), the session row's `notice`, and the rows.

import XCTest

@testable import Cypher

@MainActor
final class NoticeTextTests: XCTestCase {
    private func vectors() throws -> [String: Any] {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/notice-text-v1.json")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    func testCompactVectors() throws {
        let cases = try XCTUnwrap(try vectors()["compact"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 8)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let text = try XCTUnwrap(vector["text"] as? String)
            XCTAssertEqual(NoticeText.compact(text), vector["expect"] as? String, name)
        }
    }

    func testStripVectors() throws {
        let cases = try XCTUnwrap(try vectors()["strip"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 8)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let text = try XCTUnwrap(vector["text"] as? String)
            let expect = vector["expect"] as? String  // NSNull: untouched
            XCTAssertEqual(NoticeText.strippingReplyNotices(text), expect, name)
            XCTAssertEqual(NoticeText.isReplyNotice(text), expect == "", name)
        }
    }

    func testTheSessionRowsNoticeDecodesLenientlyAndExpires() throws {
        let value: JSONValue = .object([
            "level": .string("warning"),
            "text": .string("Claude rate limit warning: 58% used (seven_day)"),
            "at": .string("2026-10-14T21:39:44.123456789Z"),
        ])
        let notice = try XCTUnwrap(SessionNotice(value))
        XCTAssertEqual(notice.level, .warning)
        XCTAssertEqual(notice.at, 1_792_013_984_123)
        XCTAssertEqual(notice.displayText, "Claude: 58% of weekly limit")
        XCTAssertTrue(notice.isLive(now: notice.at + SessionNotice.ttlMs - 1))
        XCTAssertFalse(notice.isLive(now: notice.at + SessionNotice.ttlMs))
        XCTAssertNil(SessionNotice(.object(["level": .string("shout"), "text": .string("x")])))
        XCTAssertNil(SessionNotice(nil))
    }

    func testOldReplyNoticesRenderNothing() {
        let notice = "Claude rate limit warning: 57% used (seven_day)"
        let only = MessageEntry.fixture("m1", parts: [.text(id: "t0", text: notice)], createdAt: 7)
        XCTAssertTrue(buildRows([only]).isEmpty)
        let glued = MessageEntry.fixture(
            "m2", parts: [.text(id: "t0", text: "\(notice)Let me look."), .text(id: "t1", text: "Done.\n\(notice)")],
            createdAt: 7)
        let cleaned = NoticeText.withoutReplyNotices(glued)
        XCTAssertEqual(cleaned.parts, [.text(id: "t0", text: "Let me look."), .text(id: "t1", text: "Done.")])
        XCTAssertEqual(MessageCopy.text(glued), "Let me look.\n\nDone.")
        let quoted = MessageEntry.fixture(
            "m3", parts: [.text(id: "t0", text: "The bridge prints `\(notice)` near the cap.")], createdAt: 7)
        XCTAssertEqual(NoticeText.withoutReplyNotices(quoted), quoted)
    }
}

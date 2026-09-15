import XCTest
@testable import Cypher

final class PreviewProjectionTests: XCTestCase {
    func testSharedReducerVectors() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: root.appendingPathComponent("edge/src/fixtures/preview-reducer-v1.json"))
        let cases = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        var p = PreviewProjection()
        for c in cases {
            let bytes = try XCTUnwrap(StreamPreviewWire.encode((c["kind"] as! NSNumber).uint8Value,
                                                               header: c["header"] as! [String: Any], text: c["text"] as! String))
            let replies = p.receive(bytes, chatId: "c")
            XCTAssertEqual(p.displayed == nil ? "" : p.text, c["display"] as? String)
            XCTAssertEqual(p.displayed != nil && p.interrupted, c["interrupted"] as? Bool)
            XCTAssertEqual(replies.map { Int($0[0]) }, c["replies"] as? [Int])
        }
    }

    func testCoverageDoesNotUseCursorOrFinishHintAsProof() throws {
        var p = PreviewProjection()
        _ = p.receive(StreamPreviewWire.encode(0x25, header: ["chatId": "c", "mode": "preview", "runId": "r", "segmentId": "s", "epoch": "e"])!, chatId: "c")
        let h: [String: Any] = ["chatId": "c", "runId": "r", "segmentId": "s", "epoch": "e", "revision": 2, "baseSeq": 999]
        _ = p.receive(StreamPreviewWire.encode(0x21, header: h, text: "new text")!, chatId: "c")
        var final = h; final["batchId"] = "b"
        _ = p.receive(StreamPreviewWire.encode(0x23, header: final)!, chatId: "c")
        let durable = [MessageEntry(id: "s", role: .assistant, parts: [.text(id: "t", text: "old")], createdAt: 1, deviceId: "host", status: .streaming, continuationOf: nil)]
        XCTAssertEqual(p.overlay(durable, coverage: nil).count, 1)
        XCTAssertNotEqual(p.overlay(durable, coverage: nil), durable)
        let covered = PreviewCoverage(runId: "r", segmentId: "s", epoch: "e", revision: 2, complete: true)
        XCTAssertEqual(p.overlay(durable, coverage: covered), durable)
        var wrong = covered; wrong.epoch = "old"
        XCTAssertEqual(p.overlay(durable, coverage: wrong), durable, "another epoch wins over a stale cached preview")
        p.disconnect(); XCTAssertTrue(p.interrupted)
        let interrupted = p.overlay(durable, coverage: nil)[0]
        XCTAssertEqual(interrupted.parts.first, durable[0].parts.first)
        if case .text(_, let text) = interrupted.parts.last! { XCTAssertTrue(text.contains("暂存预览")) }
        XCTAssertNil(p.retry(chatId: "c", cursor: 0))
        XCTAssertTrue(p.expire(now: Date().addingTimeInterval(301)))
        XCTAssertEqual(p.overlay(durable, coverage: nil), durable)
    }

    func testNeverReplacesUserOrToolEntries() {
        var p = PreviewProjection()
        _ = p.receive(StreamPreviewWire.encode(0x25, header: ["chatId": "c", "mode": "preview", "runId": "r", "segmentId": "s", "epoch": "e"])!, chatId: "c")
        _ = p.receive(StreamPreviewWire.encode(0x21, header: ["chatId": "c", "runId": "r", "segmentId": "s", "epoch": "e", "revision": 0, "baseSeq": 0], text: "untrusted")!, chatId: "c")
        let user = MessageEntry(id: "s", role: .user, parts: [.text(id: "t", text: "user")], createdAt: 1, deviceId: "me", status: .complete, continuationOf: nil)
        XCTAssertEqual(p.overlay([user], coverage: nil), [user])
    }
}

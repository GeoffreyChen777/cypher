import XCTest
@testable import Cypher

final class StreamPreviewTests: XCTestCase {
    func testSharedVectors() throws {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try Data(contentsOf: root.appendingPathComponent("edge/src/fixtures/stream-preview-v1.json"))
        let vectors = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        for v in vectors {
            let name = try XCTUnwrap(v["name"] as? String)
            let bytes: Data
            if let hex = v["hex"] as? String {
                let chars = Array(hex)
                bytes = Data(stride(from: 0, to: chars.count, by: 2).map {
                    UInt8(String(chars[$0...($0 + 1)]), radix: 16)!
                })
            } else {
                bytes = ChatWire.encode(try XCTUnwrap(v["kind"] as? NSNumber).uint8Value,
                                        header: try XCTUnwrap(v["header"] as? [String: Any]),
                                        payload: Data(try XCTUnwrap(v["text"] as? String).utf8))
            }
            let decoded = StreamPreviewWire.decode(bytes)
            XCTAssertEqual(decoded != nil, v["valid"] as? Bool, name)
            if let decoded {
                let encoded = try XCTUnwrap(StreamPreviewWire.encode(decoded.kind, header: decoded.header,
                                                                    text: String(data: decoded.payload, encoding: .utf8)!))
                XCTAssertEqual(StreamPreviewWire.decode(encoded)?.payload, decoded.payload, name)
            }
        }
    }

    func testLimitsAndStrictUTF8() {
        let h: [String: Any] = ["chatId": "c", "runId": "r", "segmentId": "s", "epoch": "e", "revision": 0, "baseSeq": 0]
        XCTAssertNotNil(StreamPreviewWire.encode(0x21, header: h, text: String(repeating: "x", count: 61_440)))
        XCTAssertNil(StreamPreviewWire.encode(0x21, header: h, text: String(repeating: "界", count: 20_481)))
        XCTAssertNil(StreamPreviewWire.decode(ChatWire.encode(0x21, header: h, payload: Data([0xff]))))
        XCTAssertNil(StreamPreviewWire.decode(Data(repeating: 0, count: 65_537)))
        var long = h
        long["epoch"] = String(repeating: "x", count: 128)
        XCTAssertNotNil(StreamPreviewWire.encode(0x21, header: long))
        long["epoch"] = String(repeating: "x", count: 129)
        XCTAssertNil(StreamPreviewWire.encode(0x21, header: long))
        long["epoch"] = String(repeating: "x", count: 4096)
        XCTAssertNil(StreamPreviewWire.encode(0x21, header: long))
    }
}

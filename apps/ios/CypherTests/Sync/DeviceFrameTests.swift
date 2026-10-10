// Device relay frame codec: the shared vectors in protocol/vectors/device-frames-v1.json,
// run by the Rust and TypeScript codecs too (protocol/README.md).

import XCTest
@testable import Cypher

private func cases(_ key: String) throws -> [[String: Any]] {
    let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/device-frames-v1.json")
    let vectors = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    return try XCTUnwrap(vectors[key] as? [[String: Any]])
}

private func hex(_ value: Any?) -> Data {
    let chars = Array(value as? String ?? "")
    return Data(
        stride(from: 0, to: chars.count, by: 2).map {
            UInt8(String(chars[$0...($0 + 1)]), radix: 16)!
        })
}

final class DeviceFrameTests: XCTestCase {
    func testSharedFrameVectors() throws {
        for c in try cases("frames") {
            let name = c["name"] as! String
            let json = try XCTUnwrap(c["json"] as? String)
            let frame = DeviceRelayClient.encodeFrame(header: json, payload: hex(c["payload"]))
            XCTAssertEqual(frame, hex(c["hex"]), "\(name): encode")
            let (header, payload) = try XCTUnwrap(DeviceRelayClient.decodeFrame(frame), name)
            let expected = try XCTUnwrap(c["header"] as? [String: String])
            XCTAssertEqual(header.s, expected["s"], name)
            XCTAssertEqual(header.k, expected["k"], name)
            XCTAssertEqual(header.to, expected["to"], name)
            XCTAssertEqual(header.from, expected["from"], name)
            XCTAssertEqual(payload, hex(c["payload"]), name)
        }
    }

    func testSharedMalformedVectors() throws {
        for c in try cases("malformed") {
            XCTAssertNil(DeviceRelayClient.decodeFrame(hex(c["hex"])), c["name"] as! String)
        }
    }
}

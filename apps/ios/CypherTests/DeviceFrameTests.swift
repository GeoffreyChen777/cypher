// Device relay frame codec vectors, ported from edge/src/device-frame.test.ts
// and crates/rpc/src/device_room.rs (uleb128 header length ‖ JSON header ‖ payload).

import XCTest
@testable import Cypher

final class DeviceFrameTests: XCTestCase {
    private func decode(_ bytes: [UInt8]) -> (DeviceRelayClient.FrameHeader, Data)? {
        DeviceRelayClient.decodeFrame(Data(bytes))
    }

    func testRoundTripsHeaderAndPayload() throws {
        let payload = Data([1, 2, 3, 250, 255])
        let frame = DeviceRelayClient.encodeFrame(header: #"{"s":"term-42","k":"term","to":"conn-9"}"#,
                                                  payload: payload)
        let (header, out) = try XCTUnwrap(DeviceRelayClient.decodeFrame(frame))
        XCTAssertEqual(header.s, "term-42")
        XCTAssertEqual(header.k, "term")
        XCTAssertEqual(header.to, "conn-9")
        XCTAssertNil(header.from)
        XCTAssertEqual(out, payload)
    }

    func testMatchesTheEdgeEncoderBytes() {
        let json = #"{"s":"a","k":"rpc"}"#
        let frame = [UInt8](DeviceRelayClient.encodeFrame(header: json, payload: Data([1, 2])))
        XCTAssertEqual(frame, [UInt8(json.utf8.count)] + Array(json.utf8) + [1, 2])
    }

    func testLongHeaderUsesTwoByteLengthAndEmptyPayload() throws {
        let json = #"{"s":"\#(String(repeating: "x", count: 200))","k":"rpc","from":"conn-1"}"#
        let length = json.utf8.count
        XCTAssertGreaterThan(length, 0x7f, "the vector must exercise a multi-byte length")
        let frame = DeviceRelayClient.encodeFrame(header: json, payload: Data())
        XCTAssertEqual([frame[0], frame[1]], [UInt8(length & 0x7f) | 0x80, UInt8(length >> 7)])
        let (header, payload) = try XCTUnwrap(DeviceRelayClient.decodeFrame(frame))
        XCTAssertEqual(header.s, String(repeating: "x", count: 200))
        XCTAssertEqual(header.from, "conn-1")
        XCTAssertTrue(payload.isEmpty)
    }

    func testRejectsTruncatedFrames() {
        let json = #"{"s":"\#(String(repeating: "x", count: 200))","k":"rpc"}"#
        let frame = [UInt8](DeviceRelayClient.encodeFrame(header: json, payload: Data([7])))
        XCTAssertNil(decode(Array(frame.prefix(1))), "length prefix cut mid-varint")
        XCTAssertNil(decode(Array(frame.prefix(10))), "header cut short")
        XCTAssertNil(decode([]), "empty frame")
        XCTAssertNil(decode([0x85]), "continuation bit with no next byte")
        XCTAssertNil(decode([10, UInt8(ascii: "{")]), "header shorter than its length")
        let minimal = Array(#"{"s":"a","k":"b"}"#.utf8)
        XCTAssertNil(decode([15] + minimal.prefix(15)), "length that cuts the JSON")
    }

    func testRejectsASixthLengthByteInsteadOfWrapping() {
        XCTAssertNil(decode([0x80, 0x80, 0x80, 0x80, 0x80, 0x01, 0x7b, 0x7d]))
        XCTAssertNil(decode([0xff, 0xff, 0xff, 0xff, 0xff, 0x01]))
    }

    func testMinimalFrameKeepsTrailingPayload() throws {
        let (header, payload) = try XCTUnwrap(decode([17] + Array(#"{"s":"a","k":"b"}"#.utf8) + [9]))
        XCTAssertEqual(header.s, "a")
        XCTAssertEqual(header.k, "b")
        XCTAssertEqual(payload, Data([9]))
    }
}

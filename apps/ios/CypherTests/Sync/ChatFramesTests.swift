// chat2 wire frames: the shared vectors in protocol/vectors/chat-frames-v1.json,
// run by the Rust client and the TypeScript server codec too
// (protocol/README.md), plus the typed state header and catch-up plan.

import XCTest
@testable import Cypher

private func vectors() throws -> [String: Any] {
    let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/chat-frames-v1.json")
    return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
}

private func cases(_ key: String) throws -> [[String: Any]] {
    try XCTUnwrap(vectors()[key] as? [[String: Any]])
}

private func hex(_ value: Any?) -> Data {
    let chars = Array(value as? String ?? "")
    return Data(
        stride(from: 0, to: chars.count, by: 2).map {
            UInt8(String(chars[$0...($0 + 1)]), radix: 16)!
        })
}

private func typeByte(_ value: Any?) -> UInt8 {
    (value as! NSNumber).uint8Value
}

final class ChatFramesTests: XCTestCase {
    func testSharedFrameTypes() throws {
        let v = try vectors()
        let types: [String: UInt8] = [
            "hello": ChatFrameType.hello, "state": ChatFrameType.state,
            "rowsReq": ChatFrameType.rowsReq, "row": ChatFrameType.row,
            "rowsDone": ChatFrameType.rowsDone, "push": ChatFrameType.push,
            "ack": ChatFrameType.ack, "presence": ChatFrameType.presence,
            "probe": ChatFrameType.probe, "probeOk": ChatFrameType.probeOk,
            "error": ChatFrameType.error,
        ]
        let shared = try XCTUnwrap(v["types"] as? [String: NSNumber])
        XCTAssertEqual(shared.mapValues(\.uint8Value), types)
        XCTAssertEqual((v["maxHeaderBytes"] as? NSNumber)?.intValue, chatFrameMaxHeaderBytes)
    }

    func testSharedEncodeAndDecodeVectors() throws {
        for c in try cases("encode") {
            let name = c["name"] as! String
            let header = try XCTUnwrap(c["header"] as? [String: Any])
            let frame = ChatWire.encode(typeByte(c["type"]), header: header, payload: hex(c["payload"]))
            XCTAssertEqual(frame, hex(c["hex"]), "\(name): encode")
            let decoded = try XCTUnwrap(ChatWire.decode(frame), name)
            XCTAssertEqual(decoded.kind, typeByte(c["type"]), name)
            XCTAssertEqual(decoded.header as NSDictionary, header as NSDictionary, name)
            XCTAssertEqual(decoded.payload, hex(c["payload"]), name)
        }
    }

    func testSharedMalformedVectors() throws {
        for c in try cases("malformed") {
            XCTAssertNil(ChatWire.decode(hex(c["hex"])), c["name"] as! String)
        }
    }

    /// Unlike the DO, the client decodes future frame types and skips them.
    func testSharedUnknownTypeVectors() throws {
        for c in try cases("unknownType") {
            let name = c["name"] as! String
            let decoded = try XCTUnwrap(ChatWire.decode(hex(c["hex"])), name)
            XCTAssertEqual(decoded.kind, typeByte(c["type"]), name)
            XCTAssertEqual(decoded.header as NSDictionary, c["header"] as! NSDictionary, name)
            XCTAssertEqual(decoded.payload, hex(c["payload"]), name)
        }
    }

    func testSharedHeaderSizeVectors() throws {
        for c in try cases("headerSize") {
            // `{"pad":""}` is 10 bytes; the pad fills the header to `bytes`.
            let pad = String(repeating: "x", count: (c["bytes"] as! NSNumber).intValue - 10)
            let frame = ChatWire.encode(ChatFrameType.hello, header: ["pad": pad])
            XCTAssertEqual(ChatWire.decode(frame) != nil, c["valid"] as! Bool, c["name"] as! String)
        }
    }

    func testStateHeaderParsesServerShape() {
        let state = ChatStateHeader([
            "headSeq": 10, "seqFloor": 3, "checkpointSeq": 3,
            "checkpointSize": 160_000, "rowCount": 7, "rowBytes": 14_000,
        ])
        XCTAssertEqual(state?.headSeq, 10)
        XCTAssertEqual(state?.checkpointSeq, 3)
        XCTAssertEqual(state?.checkpointSize, 160_000)
        XCTAssertNil(ChatStateHeader(["nope": 1]))
    }

    /// The catch-up decision table from chat_client.rs plan_catch_up.
    func testPlanCatchUp() {
        func state(_ head: UInt64, _ ckSeq: UInt64, _ ckSize: UInt64) -> ChatStateHeader {
            ChatStateHeader([
                "headSeq": head, "seqFloor": 0,
                "checkpointSeq": ckSeq, "checkpointSize": ckSize,
            ])!
        }
        // No checkpoint: rows from the cursor.
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 4, state: state(10, 0, 0), frontierContained: false),
            .rowsOnly(after: 4))
        // Contained frontier skips rows the checkpoint covers.
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 2, state: state(10, 6, 100), frontierContained: true),
            .rowsOnly(after: 6))
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 8, state: state(10, 6, 100), frontierContained: true),
            .rowsOnly(after: 8))
        // Missing frontier: fetch the checkpoint, then rows after it.
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 2, state: state(10, 6, 100), frontierContained: false),
            .checkpointThenRows(after: 6))
        // Server behind the cursor (reset/wipe): the cursor is meaningless.
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 20, state: state(10, 0, 0), frontierContained: false),
            .rowsOnly(after: 0))
        // A freshly SEEDED room: checkpoint covers seq 0 but has SIZE — it
        // must not be misread as "no checkpoint".
        XCTAssertEqual(
            chatPlanCatchUp(cursor: 0, state: state(0, 0, 5_000), frontierContained: false),
            .checkpointThenRows(after: 0))
    }
}

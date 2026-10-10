// Registry merge core: the shared vectors in protocol/vectors/registry-core-v1.json,
// run by the Rust and TypeScript mirrors too (protocol/README.md), plus the
// RegistryDoc cursor-integrity cases.

import XCTest
@testable import Cypher

private struct RegistryCoreVectors: Decodable {
    struct EncodeHlc: Decodable {
        var name: String
        var ms: Int64
        var counter: UInt32
        var device: String
        var hlc: Hlc
    }

    struct HlcNewer: Decodable {
        var name: String
        var a: Hlc
        var b: Hlc?
        var newer: Bool
    }

    struct ApplyOp: Decodable {
        struct Step: Decodable {
            var op: RegistryOp
            var changed: Bool
            var row: RegistryRow?
        }

        var name: String
        var row: RegistryRow?
        var steps: [Step]
    }

    struct MaxClock: Decodable {
        var name: String
        var row: RegistryRow
        var hlc: Hlc?
    }

    struct SeedOp: Decodable {
        var name: String
        var row: RegistryRow
        var op: RegistryOp
    }

    struct Convergence: Decodable {
        var name: String
        var prefix: [RegistryOp]
        var permute: [RegistryOp]
        var row: RegistryRow
    }

    var encodeHlc: [EncodeHlc]
    var hlcNewer: [HlcNewer]
    var applyOp: [ApplyOp]
    var maxClock: [MaxClock]
    var rowToSeedOp: [SeedOp]
    var convergence: [Convergence]

    static func load() throws -> RegistryCoreVectors {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/registry-core-v1.json")
        return try JSONDecoder().decode(RegistryCoreVectors.self, from: Data(contentsOf: url))
    }
}

private func permutations<T>(_ items: [T]) -> [[T]] {
    guard !items.isEmpty else { return [[]] }
    return items.indices.flatMap { ix -> [[T]] in
        var rest = items
        let first = rest.remove(at: ix)
        return permutations(rest).map { [first] + $0 }
    }
}

final class RegistryCoreVectorTests: XCTestCase {
    func testEncodeHlcVectors() throws {
        for v in try RegistryCoreVectors.load().encodeHlc {
            XCTAssertEqual(encodeHlc(ms: v.ms, counter: v.counter, device: v.device), v.hlc, v.name)
        }
    }

    func testHlcNewerVectors() throws {
        for v in try RegistryCoreVectors.load().hlcNewer {
            XCTAssertEqual(hlcNewer(v.a, v.b), v.newer, v.name)
        }
    }

    func testApplyOpVectors() throws {
        for v in try RegistryCoreVectors.load().applyOp {
            var row = v.row
            for (ix, step) in v.steps.enumerated() {
                let result = applyOp(row, step.op)
                XCTAssertEqual(result.changed, step.changed, "\(v.name) step \(ix): changed")
                XCTAssertEqual(result.row, step.changed ? step.row : row, "\(v.name) step \(ix): row")
                row = result.row
            }
        }
    }

    func testMaxClockVectors() throws {
        for v in try RegistryCoreVectors.load().maxClock {
            XCTAssertEqual(maxClock(v.row), v.hlc, v.name)
        }
    }

    func testRowToSeedOpVectors() throws {
        for v in try RegistryCoreVectors.load().rowToSeedOp {
            let op = rowToSeedOp(v.row)
            XCTAssertEqual(op, v.op, "\(v.name): seed op")
            let reseeded = applyOp(nil, op)
            XCTAssertTrue(reseeded.changed, "\(v.name): re-seeded")
            XCTAssertEqual(reseeded.row, v.row, "\(v.name): re-seeded row")
        }
    }

    func testConvergenceVectors() throws {
        for v in try RegistryCoreVectors.load().convergence {
            for order in permutations(v.permute) {
                var row: RegistryRow?
                for op in v.prefix + order {
                    row = applyOp(row, op).row ?? row
                }
                XCTAssertEqual(row, v.row, v.name)
            }
        }
    }
}

final class RegistryCursorIntegrityTests: XCTestCase {
    func testBroadcastGapHoldsCursor() {
        let doc = RegistryDoc(deviceId: "dev-a")
        XCTAssertFalse(doc.applyRows(seq: 3, rows: []))
        XCTAssertEqual(doc.cursor, 0)
        XCTAssertTrue(doc.applyRows(seq: 1, rows: []))
        XCTAssertEqual(doc.cursor, 1)
    }

    func testAckDoesNotJumpOverUnreceivedRows() {
        let doc = RegistryDoc(deviceId: "dev-a")
        doc.ackBatch("missing", seq: 7)
        XCTAssertEqual(doc.cursor, 0)
    }

    func testPreIntegritySnapshotResetsCursorOnce() throws {
        let doc = RegistryDoc(deviceId: "dev-a")
        _ = doc.applyState(seq: 7, full: true, gcFloor: 0, rows: [])
        XCTAssertEqual(doc.cursor, 7)

        var object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: doc.toData()) as? [String: Any]
        )
        object["resyncEpoch"] = 0
        let oldData = try JSONSerialization.data(withJSONObject: object)
        let restored = try RegistryDoc.from(data: oldData, deviceId: "dev-a")
        XCTAssertEqual(restored.cursor, 0)
    }
}

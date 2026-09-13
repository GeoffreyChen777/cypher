import XCTest
@testable import Cypher

final class Workspace3MetadataTests: XCTestCase {
    private func op(_ n: UInt32, _ kind: Workspace3OpType, _ set: [String: JSONValue]? = nil) -> Workspace3Op {
        Workspace3Op(kind: "chats", id: "chat", op: kind, set: set,
                     hlc: Workspace3Metadata.clock(ms: 1, counter: n, actor: "host"))
    }
    func testNativeOperationsRejectClockOverridesAndUnknownFields() throws {
        let valid = op(1, .upsert, ["title": .string("first")])
        let data = try JSONEncoder().encode(valid)
        XCTAssertEqual(try JSONDecoder().decode(Workspace3Op.self, from: data), valid)
        let original = try XCTUnwrap(JSONDecoder().decode(JSONValue.self, from: data).objectValue)
        for (key, value) in [("clocks", JSONValue.null), ("clocks", .object([:])), ("reseed", .bool(true)), ("id", .int(1))] {
            var invalid = original; invalid[key] = value
            XCTAssertThrowsError(try JSONDecoder().decode(Workspace3Op.self, from: JSONEncoder().encode(JSONValue.object(invalid))))
        }
    }
    func testNativeDeletesRevivalAndEqualClockImmutability() throws {
        let first = try XCTUnwrap(Workspace3Metadata.apply(nil, op(1, .upsert, ["title":.string("first"),"cwd":.string("/work")])).row)
        let equal = Workspace3Metadata.apply(first, op(1, .update, ["title":.string("changed")]))
        XCTAssertEqual(equal.row, first); XCTAssertFalse(equal.changed)
        let edited = try XCTUnwrap(Workspace3Metadata.apply(first, op(3, .update, ["title":.string("new"),"cwd":.null])).row)
        XCTAssertEqual(edited.fields, ["title":.string("new")])
        XCTAssertFalse(Workspace3Metadata.apply(edited, op(2, .delete)).changed)
        let gone = try XCTUnwrap(Workspace3Metadata.apply(edited, op(4, .delete)).row)
        XCTAssertFalse(Workspace3Metadata.apply(gone, op(5, .update, ["title":.string("wrong")])).changed)
        XCTAssertFalse(Workspace3Metadata.apply(gone, op(4, .upsert, ["title":.string("old")])).changed)
        let revived = try XCTUnwrap(Workspace3Metadata.apply(gone, op(5, .upsert, ["title":.string("revived")])).row)
        XCTAssertFalse(revived.deleted); XCTAssertEqual(revived.delHlc, gone.delHlc)
        XCTAssertEqual(revived.fields, ["title":.string("revived")])
        XCTAssertNil(Workspace3Metadata.apply(nil, op(1, .update, ["title":.string("missing")])).row)
    }
    func testConcurrentFieldsConvergeAndStoredRowsRejectLossyDecode() throws {
        let a = op(1, .upsert, ["title":.string("A")]), b = op(2, .upsert, ["archived":.bool(true)])
        let ab = Workspace3Metadata.apply(Workspace3Metadata.apply(nil, a).row, b).row
        let ba = Workspace3Metadata.apply(Workspace3Metadata.apply(nil, b).row, a).row
        XCTAssertEqual(ab, ba)
        let data = try JSONEncoder().encode(XCTUnwrap(ab))
        var object = try XCTUnwrap(JSONDecoder().decode(JSONValue.self, from: data).objectValue)
        object["future"] = .string("must not disappear")
        XCTAssertThrowsError(try JSONDecoder().decode(Workspace3Row.self, from: JSONEncoder().encode(JSONValue.object(object))))
    }
}

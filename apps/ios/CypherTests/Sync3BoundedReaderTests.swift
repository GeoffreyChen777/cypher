import XCTest
@testable import Cypher

@MainActor
final class Sync3BoundedReaderTests: XCTestCase {
    func testLargeMessageSetIsReadThroughBoundedIndexedWindows() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: url) }
        let journal = try Sync3Journal(url: url, account: "account", room: "room", actor: "actor")
        _ = try journal.acceptState(["type":.string("state"), "version":.int(3), "epoch":.int(1),
            "owner":.string("actor"), "ownerEpoch":.int(1), "head":.int(75)])
        for index in 1...75 {
            let event = try Sync3Operation(id: "message-\(index)", actor: "actor", ownerEpoch: 1,
                event: ["type":.string("messageCreated"), "runId":.null,
                    "messageId":.string("message-\(index)"), "role":.string("assistant"),
                    "deviceId":.string("actor"), "createdAt":.int(Int64(index)), "continuationOf":.null])
            try journal.applyPage(["type":.string("page"), "version":.int(3), "epoch":.int(1),
                "through":.int(Int64(index)), "next":.int(Int64(index)), "done":.bool(true),
                "rows":.array([.object(["seq":.int(Int64(index)), "operation":.init(encodable: event)!])])])
        }
        let rows = try journal.allMessagesBounded()
        XCTAssertEqual(rows.count, 75)
        XCTAssertEqual(rows.first?["createdSeq"], .int(1))
        XCTAssertEqual(rows.last?["createdSeq"], .int(75))
    }
}

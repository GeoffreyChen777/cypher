import XCTest
@testable import Cypher

@MainActor
final class Sync3Tests: XCTestCase {
    private func sharedJSON(_ name: String) throws -> JSONValue {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: root.appendingPathComponent("fixtures/sync3/\(name).json")))
    }
    func testCompleteCommandValidation() throws {
        let queued = try fixture().operations[0]
        let vectors = try sharedJSON("command-validation").objectValue!
        if case .array(let payloads) = vectors["payloads"] {
            for payload in payloads {
                var e = queued.event, c = e["command"]!.objectValue!
                c["payload"] = payload; e["command"] = .object(c)
                XCTAssertNoThrow(try Sync3Operation(id: queued.id, actor: queued.actor, ownerEpoch: 1, event: e))
            }
        } else { XCTFail("missing payloads") }
        func edit(_ node: JSONValue, path: ArraySlice<String>, value: JSONValue?) -> JSONValue {
            let key = path.first!, tail = path.dropFirst()
            if var object = node.objectValue {
                if tail.isEmpty { object[key] = value }
                else { object[key] = edit(object[key]!, path: tail, value: value) }
                return .object(object)
            }
            if case .array(var array) = node, let index = Int(key) {
                array[index] = edit(array[index], path: tail, value: value); return .array(array)
            }
            preconditionFailure("invalid fixture edit path")
        }
        if case .array(let invalid) = vectors["invalid"] {
            for mutation in invalid {
                let m = mutation.objectValue!
                guard case .array(let path) = m["path"] else { return XCTFail("missing path") }
                var e = queued.event
                e["command"] = edit(e["command"]!, path: path.map { $0.stringValue! }[...], value: m["remove"] == .bool(true) ? nil : m["value"])
                XCTAssertThrowsError(try Sync3Operation(id: queued.id, actor: queued.actor, ownerEpoch: 1, event: e), "\(mutation)")
            }
        } else { XCTFail("missing invalid cases") }
        var e = queued.event, c = e["command"]!.objectValue!
        c["payload"] = .object(["kind": .string("interrupt")]); c["basedOn"] = .null; e["command"] = .object(c)
        XCTAssertThrowsError(try Sync3Operation(id: queued.id, actor: queued.actor, ownerEpoch: 1, event: e))
    }
    func testSharedCommandLifecyclePersistsAndRejectsWithoutCursorAdvance() throws {
        let queued = try fixture().operations[0]
        guard case .array(let scenarios) = try sharedJSON("command-lifecycle") else { return XCTFail("missing scenarios") }
        for scenario in scenarios {
            let c = scenario.objectValue!, url = try directory().appendingPathComponent("journal.sqlite")
            var journal: Sync3Journal? = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
            try journal!.acceptState(state(head: 1))
            var committed = [queued]
            var pure = Sync3Projection()
            try pure.apply(queued, owner: "host", ownerEpoch: 1)
            try journal!.applyPage(page(committed))
            guard case .array(let steps) = c["steps"] else { return XCTFail("missing steps") }
            for (index, value) in steps.enumerated() {
                let step = value.objectValue!
                let op = try Sync3Operation(id: "step-\(index)", actor: step["actor"]!.stringValue!,
                                            ownerEpoch: (try? Sync3Wire.integer(step["ownerEpoch"])) ?? 1,
                                            event: step["event"]!.objectValue!)
                let before = try journal!.projection
                if let error = step["error"]?.stringValue {
                    let beforePure = pure
                    XCTAssertThrowsError(try pure.apply(op, owner: "host", ownerEpoch: 1)) {
                        XCTAssertEqual($0 as? Sync3Error, .protocolError(error))
                    }
                    XCTAssertEqual(pure, beforePure)
                    // Historical pages are already fenced by the server at
                    // commit time; do not judge a past owner using today's fence.
                    if ["not_owner", "stale_owner_epoch"].contains(error) { continue }
                    XCTAssertThrowsError(try journal!.applyPage(page(committed + [op]))) {
                        XCTAssertEqual($0 as? Sync3Error, .protocolError(error))
                    }
                    XCTAssertEqual(try journal!.cursor, Int64(committed.count))
                    XCTAssertEqual(try journal!.projection, before)
                } else {
                    try pure.apply(op, owner: "host", ownerEpoch: 1)
                    committed.append(op); try journal!.applyPage(page(committed))
                }
            }
            journal = nil
            journal = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
            XCTAssertEqual(try journal!.projection.commands["command"]?["command"]?.objectValue?["status"], c["status"])
            XCTAssertEqual(try journal!.cursor, Int64(committed.count))
        }
    }
    private func fixture() throws -> (operations: [Sync3Operation], projection: Sync3Projection) {
        struct Fixture: Decodable { let operations: [Sync3Operation]; let projection: Sync3Projection }
        // Simulator tests run on the build host, using the same checked-in
        // fixture as Rust and TS rather than another hand-copied vector.
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let value = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: root.appendingPathComponent("fixtures/sync3/golden.json")))
        return (value.operations, value.projection)
    }
    private func directory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("sync3-\(UUID())")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }
    private func state(head: Int64) -> [String: JSONValue] {
        ["type": .string("state"), "version": .int(3), "epoch": .int(1),
         "owner": .string("host"), "ownerEpoch": .int(1), "head": .int(head)]
    }
    private func page(_ operations: [Sync3Operation]) throws -> [String: JSONValue] {
        let rows = operations.enumerated().map { Sync3Row(seq: Int64($0.offset + 1), operation: $0.element) }
        let value = try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode(rows))
        return ["type": .string("page"), "version": .int(3), "epoch": .int(1), "through": .int(Int64(rows.count)),
                "next": .int(Int64(rows.count)), "rows": value, "done": .bool(true)]
    }
    func testSharedGoldenAndUTF8Offsets() throws {
        let f = try fixture()
        var p = Sync3Projection()
        for op in f.operations { try p.apply(op, owner: "host", ownerEpoch: 1) }
        XCTAssertEqual(p, f.projection)
        XCTAssertThrowsError(try p.apply(f.operations[5], owner: "host", ownerEpoch: 1))
        var wrong = Sync3Projection()
        for op in f.operations.prefix(5) { try wrong.apply(op, owner: "host", ownerEpoch: 1) }
        var event = f.operations[5].event; event["offset"] = .int(2)
        let op = try Sync3Operation(id: "wrong", actor: "host", ownerEpoch: 1, event: event)
        XCTAssertThrowsError(try wrong.apply(op, owner: "host", ownerEpoch: 1))
        XCTAssertEqual(wrong.messages["message"]?["text"], .string("你好"))
        let system = try JSONDecoder().decode(Sync3Operation.self, from: Sync3Wire.encode(sharedJSON("system-message")))
        try wrong.apply(system, owner: "host", ownerEpoch: 1)
        XCTAssertEqual(wrong.messages["system-message#c1"]?["role"], .string("system"))
    }
    func testDurableOutboxRestartAndAckDoesNotSkipCursor() throws {
        let f = try fixture(), url = try directory().appendingPathComponent("journal.sqlite")
        var j: Sync3Journal? = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        try j!.enqueue(f.operations[0]); j = nil
        j = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        XCTAssertEqual(try j!.pending(), [f.operations[0]])
        try j!.acceptState(state(head: 10))
        try j!.acknowledge(["type": .string("ack"), "version": .int(3), "epoch": .int(1),
                           "receipts": .array([.object(["id": .string("op-1"), "seq": .int(1)])])])
        XCTAssertEqual(try j!.cursor, 0)
        XCTAssertTrue(try j!.pending().isEmpty)
        j = nil
        j = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        try j!.applyPage(page(f.operations))
        XCTAssertEqual(try j!.projection, f.projection)
        XCTAssertEqual(try j!.cursor, 10)
        try j!.applyPage(page(f.operations))
        XCTAssertEqual(try j!.cursor, 10)
    }
    func testScopeEpochAndRollbackProtection() throws {
        let f = try fixture(), url = try directory().appendingPathComponent("journal.sqlite")
        let j = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        try j.acceptState(state(head: 10)); try j.enqueue(f.operations[0])
        let bad = try Sync3Operation(id: "bad", actor: "host", ownerEpoch: 1,
                                    event: ["type": .string("runStarted"), "runId": .string("absent")])
        XCTAssertThrowsError(try j.applyPage(page([f.operations[0], bad])))
        XCTAssertEqual(try j.cursor, 0); XCTAssertEqual(try j.projection, Sync3Projection())
        XCTAssertEqual(try j.pending(), [f.operations[0]])
        XCTAssertThrowsError(try Sync3Journal(url: url, account: "different", room: "room", actor: "phone"))
        var changed = state(head: 10); changed["epoch"] = .int(2)
        XCTAssertThrowsError(try j.acceptState(changed))
        changed = state(head: 10); changed["owner"] = .string("different-owner")
        XCTAssertThrowsError(try j.acceptState(changed))
        try j.applyPage(page(f.operations))
        XCTAssertThrowsError(try j.acceptState(state(head: 9)))
    }
    func testStrictWireRejectsUnknownFieldsAndUnsafeCursor() throws {
        let raw = Data(#"{"id":"a","actor":"a","ownerEpoch":1,"event":{"type":"runStarted","runId":"run"},"extra":true}"#.utf8)
        XCTAssertThrowsError(try JSONDecoder().decode(Sync3Operation.self, from: raw))
        XCTAssertThrowsError(try Sync3Wire.integer(.int(9_007_199_254_740_992)))
        XCTAssertThrowsError(try Sync3Operation(id: "x", actor: "a", ownerEpoch: 1,
                                               event: ["type": .string("unknown")]))
    }
    func testRejectsSharedInvalidVectors() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let cases = try JSONDecoder().decode([JSONValue].self, from: Data(contentsOf: root.appendingPathComponent("fixtures/sync3/invalid.json")))
        for value in cases {
            XCTAssertThrowsError(try JSONDecoder().decode(Sync3Operation.self, from: Sync3Wire.encode(value)))
        }
    }
    func testDroppingClientCancelsPendingCredentialsWithoutRetainCycle() async throws {
        let url = try directory().appendingPathComponent("journal.sqlite")
        let journal = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        let started = expectation(description: "credential resolution started")
        let cancelled = expectation(description: "credential resolution cancelled")
        var client: Sync3Client? = Sync3Client(journal: journal, request: {
            started.fulfill()
            do { try await Task.sleep(for: .seconds(300)) }
            catch { cancelled.fulfill(); throw error }
            throw Sync3Error.protocolError("unexpected_timeout")
        })
        weak var observed = client
        await fulfillment(of: [started], timeout: 2)
        XCTAssertNotNil(observed)
        client = nil
        XCTAssertNil(observed)
        await fulfillment(of: [cancelled], timeout: 2)
    }
    func testCancellationFencesEveryHTTPRepairCommitBoundary() async throws {
        let f = try fixture()
        for boundary in ["hello", "pull", "push"] {
            let journal = try Sync3Journal(url: directory().appendingPathComponent("journal.sqlite"),
                                           account: "account", room: "room", actor: "phone")
            try journal.enqueue(f.operations[0])
            let started = expectation(description: "\(boundary) started")
            let cancelled = expectation(description: "\(boundary) cancelled")
            var response: CheckedContinuation<[String: JSONValue], Never>?
            let head: Int64 = boundary == "pull" ? 1 : 0
            let initial = state(head: head)
            let reply: [String: JSONValue]
            if boundary == "pull" { reply = try page([f.operations[0]]) }
            else if boundary == "push" {
                reply = ["type": .string("ack"), "version": .int(3), "epoch": .int(1),
                         "receipts": .array([.object(["id": .string(f.operations[0].id), "seq": .int(1)])])]
            } else { reply = initial }
            let client = Sync3Client(journal: journal, request: {
                throw Sync3Error.protocolError("transport_unavailable")
            }, repair: { frame in
                if frame["type"] == .string(boundary) {
                    return await withTaskCancellationHandler {
                        await withCheckedContinuation { continuation in
                            response = continuation; started.fulfill()
                        }
                    } onCancel: { cancelled.fulfill() }
                }
                return initial
            })
            await fulfillment(of: [started], timeout: 2)
            let stop = Task { await client.stop() }
            await fulfillment(of: [cancelled], timeout: 2)
            response?.resume(returning: reply)
            await stop.value
            XCTAssertEqual(try journal.cursor, 0)
            XCTAssertEqual(try journal.projection, Sync3Projection())
            XCTAssertEqual(try journal.pending(), [f.operations[0]], boundary)
            XCTAssertEqual(try journal.epoch, boundary == "hello" ? 0 : 1)
        }
    }
    func testHTTPRepairHasOneAggregateDeadline() async throws {
        let journal = try Sync3Journal(url: directory().appendingPathComponent("journal.sqlite"),
                                      account: "account", room: "room", actor: "phone")
        let cancelled = expectation(description: "budget cancelled the HTTP exchange")
        let reply = state(head: 0)
        let client = Sync3Client(journal: journal, request: {
            throw Sync3Error.protocolError("transport_unavailable")
        }, repair: { _ in
            do { try await Task.sleep(for: .seconds(300)) }
            catch { cancelled.fulfill() }
            return reply // Even a late callback that ignores cancellation is fenced.
        }, tuning: .init(deadline: .milliseconds(30)))
        await fulfillment(of: [cancelled], timeout: 2)
        await client.stop()
        XCTAssertEqual(try journal.epoch, 0)
        XCTAssertEqual(try journal.cursor, 0)
        XCTAssertEqual(client.status.repairs, 1)
    }
    func testHTTPRepairEpochConflictParksWithoutRetry() async throws {
        let journal = try Sync3Journal(url: directory().appendingPathComponent("journal.sqlite"),
                                      account: "account", room: "room", actor: "phone")
        try journal.acceptState(state(head: 0))
        var conflict = state(head: 0); conflict["epoch"] = .int(2)
        let failed = expectation(description: "recovery condition surfaced")
        var calls = 0
        let client = Sync3Client(journal: journal, request: {
            throw Sync3Error.protocolError("transport_unavailable")
        }, repair: { _ in calls += 1; return conflict })
        client.onChange = {
            if client.status.error == "epoch_mismatch" { failed.fulfill() }
        }
        await fulfillment(of: [failed], timeout: 2)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(calls, 1)
        XCTAssertEqual(try journal.epoch, 1)
        client.onChange = nil
        await client.stop()
    }
    func testHTTPRepairRejectsNonTLSRemoteEndpointBeforeSending() async throws {
        let transport = Sync3HTTPTransport {
            URLRequest(url: URL(string: "http://example.com/exchange")!)
        }
        do {
            _ = try await transport.exchange(["type": .string("probe"), "version": .int(3)])
            XCTFail("insecure endpoint was accepted")
        } catch {
            XCTAssertEqual(error as? Sync3Error, .protocolError("insecure_endpoint"))
        }
    }
    func testJavaScriptNumericNormalizationPreservesOperationIdentity() throws {
        let journal = try Sync3Journal(url: directory().appendingPathComponent("journal.sqlite"),
                                      account: "account", room: "room", actor: "phone")
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let fixture = try JSONDecoder().decode([String: JSONValue].self, from: Data(contentsOf: root.appendingPathComponent("fixtures/sync3/numbers.json")))
        let canonical = try JSONDecoder().decode(Sync3Operation.self, from: Sync3Wire.encode(fixture["canonical"]!))
        var event = canonical.event, entry = event["command"]!.objectValue!
        var payload = entry["payload"]!.objectValue!, request = payload["request"]!.objectValue!
        request["modelOptions"] = .object(["numbers": .array([.double(1), .double(-0.0), .double(1.25), .object(["count": .double(1e3)])])])
        payload["request"] = .object(request); entry["payload"] = .object(payload); event["command"] = .object(entry)
        let source = try Sync3Operation(id: canonical.id, actor: "phone", ownerEpoch: 1, event: event)
        XCTAssertEqual(source, canonical)
        try journal.enqueue(source); try journal.enqueue(canonical)
        XCTAssertEqual(try journal.pending(), [canonical])
        try journal.acceptState(state(head: 1)); try journal.applyPage(page([canonical]))
        try journal.enqueue(source)
        XCTAssertEqual(try journal.cursor, 1)
        XCTAssertTrue(try journal.pending().isEmpty)
    }
}

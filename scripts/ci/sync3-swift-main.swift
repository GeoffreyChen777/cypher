// Host-only cross-language smoke runner. Compiled explicitly by sync3-smoke.py,
// never linked into the iOS application. RegistryCore's clock dependency:
import Foundation
func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

@main struct Sync3SwiftSmoke {
    @MainActor static func main() async throws {
        struct Fixture: Decodable { let operations: [Sync3Operation]; let projection: Sync3Projection }
        let fixture = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])))
        let head = Int64(fixture.operations.count)
        var projection = Sync3Projection()
        for (i, op) in fixture.operations.enumerated() { try projection.apply(op, owner: "host", ownerEpoch: 1, seq: Int64(i + 1)) }
        precondition(projection == fixture.projection)
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("cypher-sync3-swift-\(UUID())")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("journal.sqlite")
        var journal: Sync3Journal? = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        try journal!.enqueue(fixture.operations[0]); journal = nil
        journal = try Sync3Journal(url: url, account: "account", room: "room", actor: "phone")
        let pending = try journal!.pending()
        precondition(pending == [fixture.operations[0]])
        try journal!.acceptState(["type": .string("state"), "version": .int(3), "epoch": .int(1),
                                 "owner": .string("host"), "ownerEpoch": .int(1), "head": .int(head)])
        let rows = fixture.operations.enumerated().map { Sync3Row(seq: Int64($0.offset + 1), operation: $0.element) }
        let encoded = try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode(rows))
        try journal!.applyPage(["type": .string("page"), "version": .int(3), "epoch": .int(1),
                               "through": .int(head), "next": .int(head), "rows": encoded, "done": .bool(true)])
        let actual = try journal!.projection, cursor = try journal!.cursor
        precondition(actual == fixture.projection && cursor == head)
        print("PASS: Swift shared fixture, SQLite restart and transactional projection")
        if CommandLine.arguments.count >= 4, CommandLine.arguments[2] == "--shared-journal" {
            let shared = try Sync3Journal(url: URL(fileURLWithPath: CommandLine.arguments[3]),
                                          account: "account", room: "shared-room", actor: "phone")
            let before = try shared.projection, cursor = try shared.cursor
            precondition(before == fixture.projection && cursor == head)
            let window = try shared.messageWindow()
            precondition(window.through == head && window.messages.count == 1)
            precondition(window.messages[0]["createdSeq"] == .int(4))
            let numbersURL = URL(fileURLWithPath: CommandLine.arguments[1]).deletingLastPathComponent().appendingPathComponent("numbers.json")
            let numbers = try JSONDecoder().decode([String: JSONValue].self, from: Data(contentsOf: numbersURL))
            let canonical = try JSONDecoder().decode(Sync3Operation.self, from: Sync3Wire.encode(numbers["canonical"]!))
            let pending = try shared.pending()
            precondition(pending == [canonical])
            try shared.acknowledge(["type": .string("ack"), "version": .int(3), "epoch": .int(1),
                                    "receipts": .array([.object(["id": .string(canonical.id), "seq": .int(head + 1)])])])
            let afterAck = try shared.cursor
            precondition(afterAck == head)
            let rows = try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode([Sync3Row(seq: head + 1, operation: canonical)]))
            try shared.applyPage(["type": .string("page"), "version": .int(3), "epoch": .int(1),
                                  "through": .int(head + 1), "next": .int(head + 1), "rows": rows, "done": .bool(true)])
            let resolved = try Sync3Operation(id: "resolved", actor: "host", ownerEpoch: 1, event: [
                "type": .string("commandResolved"), "commandId": .string("command"),
                "status": .string("applied"), "resolution": .null,
            ])
            let resolvedRows = try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode([Sync3Row(seq: head + 2, operation: resolved)]))
            try shared.applyPage(["type": .string("page"), "version": .int(3), "epoch": .int(1),
                                  "through": .int(head + 2), "next": .int(head + 2), "rows": resolvedRows, "done": .bool(true)])
            print("PASS: Swift read Rust SQLite state/outbox and applied the canonical receipt")
            return
        }
        if CommandLine.arguments.count >= 4 {
            let base = CommandLine.arguments[2], room = CommandLine.arguments[3]
            guard let parsed = URL(string: base), ["localhost", "127.0.0.1"].contains(parsed.host),
                  parsed.scheme == "http" else { throw Sync3Error.protocolError("local_only_test") }
            let path = "\(base)/sync3/sync3-org/chats/\(room)/"
            let liveJournal = try Sync3Journal(url: directory.appendingPathComponent("live.sqlite"),
                                              account: "account", room: path, actor: "swift-reader")
            let client = Sync3Client(journal: liveJournal, request: {
                var request = URLRequest(url: URL(string: path.replacingOccurrences(of: "http:", with: "ws:") + "ws")!)
                request.setValue("Bearer sync3-user@sync3-org", forHTTPHeaderField: "Authorization")
                return request
            })
            try await wait(client, cursor: head)
            let projected = try liveJournal.projection
            precondition(projected == fixture.projection)
            var entry = fixture.operations[0].event["command"]!.objectValue!
            entry["id"] = .string("swift-command"); entry["issuedBy"] = .string("swift-reader")
            let command = try Sync3Operation(id: "swift-op", actor: "swift-reader", ownerEpoch: 1,
                                            event: ["type": .string("commandQueued"), "commandId": .string("swift-command"),
                                                    "command": .object(entry)])
            try client.enqueue(command)
            try await wait(client, cursor: head + 1)
            precondition(client.status.repairs == 0)
            await client.stop()
            print("PASS: Swift reads Rust events and commits a command over real workerd WS; HTTP repairs=0")
        }
    }
    @MainActor static func wait(_ client: Sync3Client, cursor: Int64) async throws {
        let deadline = ContinuousClock.now.advanced(by: .seconds(15))
        while client.status.cursor < cursor || client.status.phase != "live" {
            if let error = client.status.error { throw Sync3Error.protocolError(error) }
            guard ContinuousClock.now < deadline else { throw Sync3Error.protocolError("convergence_timeout") }
            try await Task.sleep(for: .milliseconds(20))
        }
    }
}

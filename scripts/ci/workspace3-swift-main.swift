import Foundation
func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

@main
struct WorkspaceSmoke {
    @MainActor static func main() async throws {
        let args = CommandLine.arguments
        if args[1] == "--rpc" {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: directory) }
            let base = args[2], room = args[3], cwd = args[4]
            let journal = try Workspace3Journal(url: directory.appendingPathComponent("rpc.sqlite"),
                scope: Workspace3Scope(endpoint: base, org: "sync3-org", user: "sync3-user", actor: "swift-rpc"))
            var rpc: Workspace3RPC?
            let client = Workspace3Client(journal: journal, request: {
                var request = URLRequest(url: URL(string: base.replacingOccurrences(of: "http:", with: "ws:") + "/workspace3/sync3-org/ws")!)
                request.setValue("Bearer sync3-user@sync3-org", forHTTPHeaderField: "Authorization")
                return request
            }, event: { rpc?.event($0) })
            rpc = Workspace3RPC(client: client, active: { true })
            let value = try await rpc!.call(owner: "probe", target: "host", method: "ReadWorkspaceFile",
                params: .object(["chatId": .string(room), "cwd": .string(cwd), "path": .string("rpc-proof.txt")]), timeout: 30)
            let result = try JSONDecoder().decode(JSONValue.self, from: value)
            precondition(result.objectValue?["text"] == .string(String(repeating: "远程完整文件🙂\"\\\n", count: 10_000)))
            do {
                _ = try await rpc!.call(owner: "probe", target: "host", method: "SearchFiles",
                    params: .object(["query": .string(String(repeating: "界", count: 32_000))]), timeout: 30)
                fatalError("invalid query accepted")
            } catch { precondition(error.localizedDescription.contains("must not exceed 256"), "\(error)") }
            do {
                _ = try await rpc!.call(owner: "probe", target: "host", method: "SignOut", params: .object([:]), timeout: 30)
                fatalError("remote logout accepted")
            } catch { precondition(error.localizedDescription.contains("remote_method_forbidden"), "\(error)") }
            let upload = "swift-native-upload"
            let bytes = Data((0..<120_003).map { UInt8($0 % 251) })
            let encoded = Array(bytes.base64EncodedString().utf8)
            for start in stride(from: 0, to: encoded.count, by: 60_000) {
                let chunk = String(decoding: encoded[start..<min(start + 60_000, encoded.count)], as: UTF8.self)
                _ = try await rpc!.call(owner: "upload", target: "host", method: "UploadChunk", params: .object([
                    "uploadId": .string(upload), "seq": .int(Int64(start / 60_000)), "data": .string(chunk)
                ]), timeout: 30)
            }
            let commit: JSONValue = .object(["uploadId": .string(upload), "fileName": .string("proof.png"), "chatId": .string(room)])
            let committed = try await rpc!.call(owner: "upload", target: "host", method: "UploadCommit", params: commit, timeout: 30)
            let path = try JSONDecoder().decode(JSONValue.self, from: committed).objectValue!["path"]!.stringValue!
            let repeated = try await rpc!.call(owner: "upload", target: "host", method: "UploadCommit", params: commit, timeout: 30)
            let repeatedValue = try JSONDecoder().decode(JSONValue.self, from: repeated)
            precondition(repeatedValue.objectValue?["path"] == .string(path))
            var received = Data(), offset: Int64 = 0
            while true {
                let reply = try await rpc!.call(owner: "upload", target: "host", method: "ReadAttachmentChunk",
                    params: .object(["path": .string(path), "offset": .int(offset)]), timeout: 30)
                let value = try JSONDecoder().decode(JSONValue.self, from: reply).objectValue!
                received.append(Data(base64Encoded: value["data"]!.stringValue!)!)
                offset = value["nextOffset"]!.int64Value!
                if value["done"] == .bool(true) { break }
            }
            precondition(received == bytes)
            do {
                _ = try await rpc!.call(owner: "upload", target: "host", method: "UploadChunk", params: .object([
                    "uploadId": .string(upload), "seq": .int(0), "data": .string(Data("changed".utf8).base64EncodedString())
                ]), timeout: 30)
                fatalError("changed committed upload accepted")
            } catch { precondition(error.localizedDescription.contains("Upload chunk conflict"), "\(error)") }
            precondition(rpc!.count == 0)
            rpc!.retire(); await client.stop()
            print("PASS: normal Swift RPC → workerd → Engine; full file, immutable upload/readback + repeated commit, conflict rejection, bounded calls")
            return
        }
        if args[1] == "--create-journal" {
            let journal = try Workspace3Journal(url: URL(fileURLWithPath: args[2]),
                scope: Workspace3Scope(endpoint: "https://workspace.fixture", org: "org", user: "user", actor: "shared"))
            try journal.mutate([Workspace3Op(kind: "chats", id: "swift-created", op: .upsert,
                set: ["id": .string("swift-created"), "title": .string("Swift created")], hlc: "")], now: 100)
            print("PASS: Swift created a fresh portable workspace SQLite journal")
            return
        }
        if args[1] == "--codec" {
            struct Corpus: Codable { let value: JSONValue; var parts: [JSONValue] }
            let url = URL(fileURLWithPath: args[2])
            var corpus = try JSONDecoder().decode(Corpus.self, from: Data(contentsOf: url))
            var decoder = Workspace3RPCCodec.Decoder()
            for (i, part) in corpus.parts.enumerated() {
                let result = try decoder.push(part)
                precondition(i + 1 == corpus.parts.count ? result == corpus.value : result == nil)
            }
            var encoder = try Workspace3RPCCodec.Encoder(corpus.value)
            corpus.parts.removeAll()
            while let part = encoder.next() { corpus.parts.append(part) }
            try Workspace3Wire.data(corpus).write(to: url, options: .atomic)
            print("PASS: Swift decoded Rust RPC Unicode fragments and wrote its own bounded encoding")
            return
        }
        if args[1] == "--journal" {
            let scope = Workspace3Scope(endpoint: "https://workspace.fixture", org: "org", user: "user", actor: "shared")
            let journal = try Workspace3Journal(url: URL(fileURLWithPath: args[2]), scope: scope)
            guard let pending = try journal.pending() else { fatalError("missing Rust outbox") }
            struct Push: Decodable { let ops: [Workspace3Op] }
            let push = try JSONDecoder().decode(Push.self, from: Data(pending.request.utf8))
            var row = Workspace3Metadata.apply(nil, push.ops[0]).row!
            precondition(row.fields["title"] == .string("Rust 工作区🙂")); row.seq = 1
            try journal.acknowledge(id: pending.id, hash: pending.hash, through: 1, rows: [row])
            let cursor = try journal.cursor
            precondition(cursor == 0)
            try journal.applyPage(after: 0, page: Workspace3Page(through: 1, next: 1, done: true, rows: [row]))
            try journal.mutate([Workspace3Op(kind: "chats", id: "shared-chat", op: .update,
                                           set: ["title": .string("Swift 工作区🙂")], hlc: "")], now: 1)
            print("PASS: Swift reopened Rust workspace3 SQLite and preserved scoped HLC/outbox semantics")
            return
        }
        let base = args[1], directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        func scope(_ actor: String, user: String = "sync3-user") -> Workspace3Scope {
            Workspace3Scope(endpoint: base, org: "sync3-org", user: user, actor: actor)
        }
        func request() -> URLRequest {
            var request = URLRequest(url: URL(string: base.replacingOccurrences(of: "http:", with: "ws:") + "/workspace3/sync3-org/ws")!)
            request.setValue("Bearer sync3-user@sync3-org", forHTTPHeaderField: "Authorization")
            return request
        }
        let writer = try Workspace3Journal(url: directory.appendingPathComponent("writer.sqlite"), scope: scope("swift-workspace"))
        let observer = try Workspace3Journal(url: directory.appendingPathComponent("observer.sqlite"), scope: scope("swift-observer"))
        try writer.mutate([Workspace3Op(kind: "chats", id: "swift-workspace-chat", op: .upsert,
                                     set: ["id": .string("swift-workspace-chat"), "deviceId": .string("host"), "title": .string("Swift 工作区🙂")],
                                     hlc: "")], now: 2000)
        var sawPresence = false
        let a = Workspace3Client(journal: writer, request: { request() }, event: { _ in })
        let b = Workspace3Client(journal: observer, request: { request() }, event: {
            if case .frame(_, let frame) = $0, frame["type"] == .string("presence"),
               frame["actor"] == .string("swift-workspace") { sawPresence = true }
        })
        try a.presence(["visible": .bool(true)])
        try b.watch(["swift-workspace-chat"])
        let deadline = ContinuousClock.now.advanced(by: .seconds(20))
        while try (!a.status.caughtUp || !b.status.caughtUp || !sawPresence || writer.pending() != nil ||
                observer.row(kind: "chats", id: "swift-workspace-chat")?.fields["title"] != .string("Swift 工作区🙂")) {
            if let error = a.status.error ?? b.status.error { fatalError(error) }
            precondition(ContinuousClock.now < deadline, "workspace convergence deadline")
            try await Task.sleep(for: .milliseconds(10))
        }
        let wrong = try Workspace3Journal(url: directory.appendingPathComponent("wrong.sqlite"), scope: scope("wrong", user: "other-user"))
        try wrong.mutate([Workspace3Op(kind: "chats", id: "must-not-upload", op: .upsert, set: ["id": .string("must-not-upload")], hlc: "")], now: 1)
        let bad = Workspace3Client(journal: wrong, request: { request() }, event: { _ in })
        while bad.status.error == nil {
            precondition(ContinuousClock.now < deadline)
            try await Task.sleep(for: .milliseconds(10))
        }
        precondition(bad.status.error == "account_mismatch")
        let retained = try wrong.pending(), absent = try observer.row(kind: "chats", id: "must-not-upload")
        precondition(retained != nil)
        precondition(absent == nil)
        await bad.stop(); await a.stop(); await b.stop()
        do {
            try await a.send(generation: a.status.generation, frame: ["type": .string("call")])
            fatalError("retired client sent")
        } catch {}
        precondition(!a.status.connected && !b.status.connected)
        print("PASS: Swift workspace3 → real workerd; offline metadata, presence, read interest, exact ACK, account fence and retirement")
    }
}

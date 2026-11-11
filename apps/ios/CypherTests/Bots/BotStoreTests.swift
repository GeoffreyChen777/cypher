// The live store's workspace side, driven through a RegistryDoc as the
// registry room would (never started: no network, no disk): rows projected
// into typed records, workspace chats kept out of every session list, and
// owner edits written as exactly the registry ops the engine writes.

import XCTest
@testable import Cypher

@MainActor
final class BotStoreTests: XCTestCase {
    private let now = nowMs()
    private var doc: RegistryDoc!

    override func setUp() {
        super.setUp()
        doc = RegistryDoc(deviceId: "ios-test")
    }

    private func store() -> WorkspaceStore {
        let config = AppConfig(
            edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Test")
        return WorkspaceStore(config: config, initialDocument: doc)
    }

    /// A row whose fields are `json` (a record's serde JSON).
    private func put(_ kind: String, _ id: String, _ json: String) throws {
        guard case .object(let fields) = try jsonFixture(json) else { return XCTFail("not an object") }
        doc.write(kind: kind, id: id, op: .upsert, set: fields)
    }

    private func chat(_ id: String, space: String? = "space-1", extra: String = "") throws {
        let spaceField = space.map { #","spaceId":"\#($0)""# } ?? ""
        try put(
            "chats", id,
            #"{"id":"\#(id)","deviceId":"mac","archived":false,"cwd":"/p","createdAt":\#(now)\#(spaceField)\#(extra)}"#)
    }

    private func seedWorkspace() throws {
        try put("spaces", "space-1", #"{"id":"space-1","deviceId":"mac","path":"/p","createdAt":1}"#)
        try put(
            "bots", "b1",
            #"""
            {"id":"b1","name":"Atlas","hostDeviceId":"mac","chatId":"bot-chat",
             "scope":{"projects":{"mode":"selected","spaceIds":["space-1"]}},"createdAt":5}
            """#)
        try put("bots", "b0", #"{"id":"b0","name":"Older","hostDeviceId":"mac","chatId":"c0","createdAt":1}"#)
        try put(
            "botWork", "w1",
            #"""
            {"id":"w1","botId":"b1","title":"Plan","chatId":"bot-chat","createdAt":10,"state":"proposed",
             "steps":[{"id":"1","title":"Do","state":"running","sessions":["worker"]}]}
            """#)
        try put("botWork", "w-bad", #"{"id":"w-bad","botId":"b1","title":"Broken"}"#)
        try put(
            "botWork", "w-done",
            #"""
            {"id":"w-done","botId":"b1","title":"Done","chatId":"bot-chat","createdAt":3,"state":"done",
             "threadChatId":"thread-done"}
            """#)
        try put(
            "botRoutines", "r1",
            #"""
            {"id":"r1","botId":"b1","name":"Brief","prompt":"P","createdAt":1,"updatedAt":1,
             "threadChatId":"routine-thread","manualChatId":"routine-manual"}
            """#)
        try put(
            "botApprovals", "a1",
            #"{"id":"a1","botId":"b1","threadChatId":"t","action":"Start","reason":"Peer","createdAt":2}"#)
        try put(
            "botConnections", "k1",
            #"""
            {"id":"k1","botId":"b1","peer":{"roomId":"r","name":"Ledger","ownerName":"Dana"},"createdAt":1,
             "replyMode":"review","scope":{"projects":{"mode":"all"}}}
            """#)
        try put("botInvites", "i1", #"{"id":"i1","botId":"b1","createdAt":1,"expiresAt":9999999999999,"maxUses":1}"#)
        try put(
            "botExchanges", "e1",
            #"""
            {"id":"e1","botId":"b1","connectionId":"k1","direction":"incoming","question":"Q",
             "state":"awaitingReview","askedAt":4}
            """#)
        for (id, order) in [("s-a", 0), ("s-b", 1), ("s-c", 2)] {
            try put(
                "botShortcuts", id,
                #"{"id":"\#(id)","botId":"b1","label":"L","request":"R","order":\#(order),"createdAt":1,"updatedAt":1}"#
            )
        }
        try put(
            "memoryStores", "mem-w-b1",
            #"{"id":"mem-w-b1","scope":{"kind":"workspace","botId":"b1"},"hostDeviceId":"mac","files":2}"#)
        try put("botActivity", "act-1", #"{"id":"act-1","botId":"b1","at":7,"kind":"futureKind","text":"Did"}"#)
        // Chats: an ordinary session, the workspace chat, its threads, a
        // project routine's session, a memory pass, and the work's session.
        try chat("plain")
        try chat("worker")
        try chat("bot-chat", space: nil, extra: #","bot":{"botId":"b1"}"#)
        try chat(
            "thread-done", space: nil,
            extra: #","bot":{"botId":"b1","origin":"owner","thread":{"kind":"work","workId":"w-done"}}"#)
        try chat(
            "routine-thread", space: "space-1",
            extra: #","bot":{"botId":"b1","origin":"routine","thread":{"kind":"routineSession","routineId":"r1"}}"#)
        try chat(
            "routine-manual", space: nil, extra: #","bot":{"botId":"b1","thread":{"kind":"routine","routineId":"r1"}}"#)
        try chat(
            "pass", space: nil,
            extra: #","memory":{"storeId":"mem-w-b1","scope":{"kind":"workspace","botId":"b1"},"passId":"p"}"#)
        try chat("garbled", space: nil, extra: #","bot":{"origin":"owner"}"#)
        try chat(
            "archived-thread",
            extra: #","archived":true,"bot":{"botId":"b1","thread":{"kind":"peer","connectionId":"k1"}}"#)
        try put(
            "chats", "quick",
            #"{"id":"quick","deviceId":"mac","archived":false,"cwd":"/tmp/cypher-scratch/quick","createdAt":1}"#)
    }

    func testRecordsProjectIntoTypedListsAndSkipMalformedRows() throws {
        try seedWorkspace()
        let store = store()
        XCTAssertEqual(store.bots.map(\.id), ["b0", "b1"], "oldest first")
        XCTAssertEqual(store.botRecords.work.map(\.id), ["w-done", "w1"], "the row without chatId is skipped")
        XCTAssertEqual(store.botRecords.routines.map(\.id), ["r1"])
        XCTAssertEqual(store.botRecords.connections.first?.scope?.projects, .all)
        XCTAssertEqual(store.botRecords.activity.first?.kind, .other)
        XCTAssertEqual(store.botRecords.shortcuts.map(\.id), ["s-a", "s-b", "s-c"])
        XCTAssertEqual(store.memoryStore(scope: .workspace(botId: "b1"))?.files, 2)
        XCTAssertEqual(store.chat(id: "bot-chat")?.bot?.botId, "b1")
        XCTAssertEqual(store.chat(id: "thread-done")?.botThread, .work(workId: "w-done"))
        XCTAssertNotNil(store.chat(id: "pass")?.memory)
        XCTAssertNil(store.chat(id: "garbled"), "an unreadable marker hides the chat")
        XCTAssertEqual(store.startedBy(chatId: "worker")?.work.id, "w1")
        XCTAssertEqual(store.grantedSpaces(of: try XCTUnwrap(store.bot(id: "b1"))).map(\.id), ["space-1"])
        XCTAssertEqual(store.openInvites(of: "b1").map(\.id), ["i1"])
    }

    func testWorkspaceChatsStayOutOfEverySessionList() throws {
        try seedWorkspace()
        let store = store()
        let hidden: Set<String> = [
            "bot-chat", "thread-done", "routine-thread", "routine-manual", "pass", "archived-thread",
        ]
        let lists: [(String, [Chat])] = [
            ("overview", store.overviewChats), ("active", store.activeChats), ("in space", store.chats(in: "space-1")),
            ("no project", store.projectlessChats), ("quick", store.quickChats), ("archived", store.archivedChats()),
            ("archived in space", store.archivedChats(in: "space-1")),
        ]
        for (name, list) in lists {
            XCTAssertTrue(Set(list.map(\.id)).isDisjoint(with: hidden), name)
        }
        XCTAssertEqual(Set(store.activeChats.map(\.id)), ["plain", "worker", "quick"])
        XCTAssertEqual(store.projectlessChats.map(\.id), [], "workspace threads no longer land under No project")
        let candidates = Mentions.sessionCandidates(
            store.chats, query: "", currentChat: nil, project: nil, device: nil)
        XCTAssertTrue(Set(candidates.map(\.chatId)).isDisjoint(with: hidden))
        let typed = Mentions.sessionCandidates(store.chats, query: "bot", currentChat: nil, project: nil, device: nil)
        XCTAssertTrue(typed.isEmpty, "not even by id")
        // Opening by id (a notification) still finds them.
        for id in hidden { XCTAssertNotNil(store.chat(id: id), id) }
    }

    func testResolvingAPlanWritesItsDecisionOnce() async throws {
        try seedWorkspace()
        let store = store()
        try await store.resolvePlan(workId: "w1", approve: true)
        let op = try XCTUnwrap(doc.pending.last?.ops.first)
        XCTAssertEqual(op.kind, "botWork")
        XCTAssertEqual(op.id, "w1")
        XCTAssertEqual(op.op, .update)
        XCTAssertEqual(Set(op.set?.keys ?? [:].keys), ["decision"])
        guard case .object(let decision) = op.set?["decision"] else { return XCTFail("no decision") }
        XCTAssertEqual(decision["approve"], .bool(true))
        XCTAssertNotNil(decision["at"]?.int64Value)
        XCTAssertNotNil(store.botRecords.work.first { $0.id == "w1" }?.decision, "the overlay shows it at once")
        do {
            try await store.resolvePlan(workId: "w1", approve: false)
            XCTFail("decided twice")
        } catch let error as BotActionError {
            XCTAssertEqual(error, .alreadyDecided)
        }
        do {
            try await store.resolvePlan(workId: "gone", approve: true)
            XCTFail("no such plan")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "This plan no longer exists.")
        }
    }

    func testOwnerFieldsAreSingleFieldUpdates() async throws {
        try seedWorkspace()
        let store = store()
        func lastSet() throws -> (kind: String, id: String, set: [String: JSONValue]) {
            let op = try XCTUnwrap(doc.pending.last?.ops.first)
            XCTAssertEqual(op.op, .update, "\(op.kind) \(op.id)")
            return (op.kind, op.id, op.set ?? [:])
        }
        try await store.stopWork(workId: "w1")
        XCTAssertEqual(Set(try lastSet().set.keys), ["stopRequestedAt"])
        try await store.resolveApproval(approvalId: "a1", approve: false)
        XCTAssertEqual(try lastSet().kind, "botApprovals")
        try await store.runRoutine(routineId: "r1")
        XCTAssertEqual(Set(try lastSet().set.keys), ["runRequestedAt"])
        try await store.setRoutineEnabled(routineId: "r1", enabled: false)
        let enabled = try lastSet().set
        XCTAssertEqual(enabled["enabled"], .bool(false))
        XCTAssertNotNil(enabled["updatedAt"]?.int64Value)
        XCTAssertEqual(store.botRecords.routines.first?.enabled, false)
        try await store.updateConnection(connectionId: "k1", replyMode: .off, scope: nil)
        let connection = try lastSet().set
        XCTAssertEqual(connection["replyMode"], .string("off"))
        XCTAssertEqual(connection["scope"], .null, "null deletes the field: the workspace's own scope")
        XCTAssertNil(store.botRecords.connections.first?.scope)
        try await store.updateConnection(
            connectionId: "k1", replyMode: .review, scope: BotScope(projects: .selected(["space-1"])))
        XCTAssertEqual(
            try lastSet().set["scope"],
            try jsonFixture(
                #"{"projects":{"mode":"selected","spaceIds":["space-1"]},"includeArchived":false,"transcripts":true}"#))
        try await store.revokeConnection(connectionId: "k1")
        XCTAssertEqual(Set(try lastSet().set.keys), ["revokeRequestedAt"])
        try await store.revokeInvite(inviteId: "i1")
        XCTAssertEqual(try lastSet().kind, "botInvites")
        try await store.reviewExchange(exchangeId: "e1", send: true, text: "  ")
        guard case .object(let review) = try lastSet().set["review"] else { return XCTFail("no review") }
        XCTAssertEqual(review["send"], .bool(true))
        XCTAssertNil(review["text"], "an empty edit sends the draft as it is")
        try await store.reviewExchange(exchangeId: "e1", send: true, text: String(repeating: "x", count: 9_000))
        guard case .object(let clipped) = try lastSet().set["review"] else { return XCTFail("no review") }
        XCTAssertEqual(clipped["text"]?.stringValue?.count, BotCaps.answer)
        try await store.setBotPaused(botId: "b1", paused: true)
        XCTAssertEqual(try lastSet().set, ["paused": .bool(true)])
        XCTAssertEqual(store.bot(id: "b1")?.paused, true)
        do {
            try await store.setBotPaused(botId: "nope", paused: true)
            XCTFail("no such workspace")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "This workspace no longer exists.")
        }
    }

    func testShortcutOrderIsOneBatchOfTheChangedRows() async throws {
        try seedWorkspace()
        let store = store()
        let before = doc.pending.count
        try await store.orderShortcuts(botId: "b1", ids: ["s-c", "s-b", "s-a", "missing"])
        XCTAssertEqual(doc.pending.count, before + 1)
        let ops = try XCTUnwrap(doc.pending.last?.ops)
        XCTAssertEqual(ops.map(\.id), ["s-c", "s-a"], "s-b keeps its place")
        XCTAssertEqual(ops.map { $0.set?["order"] }, [.int(0), .int(2)])
        XCTAssertEqual(store.shortcuts(of: "b1").map(\.id), ["s-c", "s-b", "s-a"])
        try await store.orderShortcuts(botId: "b1", ids: ["s-c", "s-b", "s-a"])
        XCTAssertEqual(doc.pending.count, before + 1, "nothing changed, nothing written")
        try await store.deleteShortcut(shortcutId: "s-b")
        XCTAssertEqual(doc.pending.last?.ops.first?.op, .delete)
        XCTAssertEqual(store.shortcuts(of: "b1").map(\.id), ["s-c", "s-a"])
    }

    func testDeletingOfflineTombstonesTheRecordWithItsSessions() async throws {
        try seedWorkspace()
        let store = store()
        do {
            try await store.deleteWork(workId: "w1")
            XCTFail("live work")
        } catch let error as BotActionError {
            XCTAssertEqual(error, .stillLive)
        }
        // No device is online (no presence), so the store does it itself.
        try await store.deleteWork(workId: "w-done")
        let ops = try XCTUnwrap(doc.pending.last?.ops)
        XCTAssertTrue(ops.allSatisfy { $0.op == .delete })
        XCTAssertEqual(
            Set(ops.map { "\($0.kind)/\($0.id)" }), ["botWork/w-done", "chats/thread-done", "sessions/thread-done"])
        XCTAssertNil(store.chat(id: "thread-done"))
        try await store.deleteRoutine(routineId: "r1")
        XCTAssertEqual(
            Set(try XCTUnwrap(doc.pending.last?.ops).map { "\($0.kind)/\($0.id)" }),
            [
                "botRoutines/r1", "chats/routine-thread", "sessions/routine-thread", "chats/routine-manual",
                "sessions/routine-manual",
            ])
        XCTAssertTrue(store.botRecords.routines.isEmpty)
    }

    func testHostOnlyCallsSayTheHostIsOffline() async throws {
        try seedWorkspace()
        let store = store()
        do {
            _ = try await store.createInvite(botId: "b1")
            XCTFail("no host")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "Atlas's device is offline.")
        }
        do {
            try await store.saveRoutine(
                BotRoutine(id: "r2", botId: "b1", name: "N", prompt: "P", createdAt: 1, updatedAt: 1))
            XCTFail("no device")
        } catch let error as BotActionError {
            XCTAssertEqual(error, .devicesOffline)
        }
        do {
            try await store.saveRoutine(
                BotRoutine(id: "r2", botId: "b1", name: " ", prompt: "P", createdAt: 1, updatedAt: 1))
            XCTFail("invalid")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "Name the routine.", "validated before any device is asked")
        }
    }

    func testRelayErrorsReadAsSentences() {
        XCTAssertEqual(BotActionError.from(RelayError.hostOffline, device: "MacBook").message, "MacBook is offline.")
        XCTAssertEqual(
            BotActionError.from(RelayError.rpc("bad params: name the routine."), device: "Mac").message,
            "Name the routine.")
        XCTAssertEqual(
            BotActionError.from(RelayError.rpc("unknown method: CreateBotInvite"), device: "Mac").message,
            "This needs a newer Cypher on Mac.")
        XCTAssertEqual(
            BotActionError.from(RelayError.rpc("bad params: unknown variant `saveBotRoutine`"), device: "Mac").message,
            "This needs a newer Cypher on Mac.")
        XCTAssertEqual(BotActionError.from(RelayError.timeout, device: "Mac").message, "Mac didn't respond.")
    }

    func testOnlyChangedKindsAreDecodedAgain() throws {
        try seedWorkspace()
        let store = store()
        let routines = doc.generation(of: "botRoutines")
        let work = doc.generation(of: "botWork")
        try put(
            "botWork", "w2", #"{"id":"w2","botId":"b1","title":"New","chatId":"bot-chat","createdAt":20}"#)
        store.afterLocalWrite()
        XCTAssertEqual(doc.generation(of: "botRoutines"), routines)
        XCTAssertNotEqual(doc.generation(of: "botWork"), work)
        XCTAssertEqual(store.botRecords.work.map(\.id), ["w-done", "w1", "w2"])
        let batch = try XCTUnwrap(doc.pending.last?.batch)
        doc.ackBatch(batch, seq: 1)
        XCTAssertNotEqual(doc.generation(of: "botWork"), work, "an ack can change the overlay")
    }
}

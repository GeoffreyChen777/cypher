import XCTest
import Loro
@testable import Cypher

final class SubagentsTests: XCTestCase {
    private let now: Int64 = 100_000

    private func parent(_ id: String = "parent") -> Chat {
        Chat(id: id, deviceId: "host", title: nil, archived: false, cwd: "/project",
             branch: "main", checkoutId: nil,
             config: ChatConfig(harness: "pi", model: "provider/model", reasoning: nil, sandbox: nil),
             lastMessagePreview: nil, lastMessageAt: nil, createdAt: 1, spaceId: "project", lastSeenAt: nil)
    }

    private func child(_ id: String = "child") -> Chat {
        var chat = parent(id)
        chat.child = ChildChat(parentChatId: "parent", parentRunId: "run", agent: "planner",
                              task: "Plan", mode: .async, toolCallId: "tool")
        return chat
    }

    private func makeRun(status: SubagentRunStatus = .running, mode: SubagentMode = .async,
                     updated: Int64 = 100_000) -> SubagentRun {
        SubagentRun(runId: "run", toolCallId: "tool", agent: "planner", model: "provider/model",
                    task: "Plan", mode: mode, status: status, progress: "Progress",
                    startedAt: 10, updatedAt: updated, childChatId: "child")
    }

    private func message(async: Bool, resolved: Bool, error: Bool = false,
                         status: MessageStatus = .complete) -> MessageEntry {
        var call = RenderToolCall(tag: "unknown", fields: ["name": "subagent"])
        call.subagent = SubagentCallMetadata(agent: "planner", task: "Plan", isAsync: async)
        return MessageEntry(id: "message", role: .assistant,
            parts: [.tool(id: "tool", call: call, isError: error, resolved: resolved)],
            createdAt: 20, deviceId: "host", status: status)
    }

    private func aggregate(_ messages: [MessageEntry] = [], runs: [SubagentRun] = [],
                           children: [Chat] = [], sessions: [String: SessionRow] = [:]) -> [SubagentPanelEntry] {
        SubagentProjection.aggregate(parent: parent(), transcript: messages, snapshot: runs,
            chats: children, sessions: sessions, now: now)
    }

    func testWireSnapshotMatchesRustCamelCaseAndSkipsMalformedRuns() throws {
        let value = try JSONDecoder().decode(JSONValue.self, from: Data("""
        [
          {"runId":"run","toolCallId":"tool","agent":"planner","model":"provider/model","task":"Plan",
           "mode":"async","status":"running","progress":"ok","startedAt":10,"updatedAt":100000,"childChatId":"child"},
          {"runId":"broken","mode":"future"},
          {"runId":"run","agent":"duplicate","task":"","mode":"sync","status":"done","startedAt":1,"updatedAt":1}
        ]
        """.utf8))
        let runs = SubagentProjection.snapshot(value)
        XCTAssertEqual(runs.count, 1)
        XCTAssertEqual(runs.first?.childChatId, "child")
        XCTAssertEqual(runs.first?.status, .running)
        XCTAssertTrue(SubagentProjection.snapshot(nil).isEmpty)
    }

    func testChildWireIgnoresProfileWithoutRequiringIt() throws {
        let value = try JSONDecoder().decode(JSONValue.self, from: Data("""
        {"parentChatId":"parent","parentRunId":"run","agent":"planner","task":"Plan","mode":"async",
         "toolCallId":"tool","profile":{"systemPrompt":"not part of the inspector"}}
        """.utf8))
        let relation = SubagentProjection.decode(value, as: ChildChat.self)
        XCTAssertEqual(relation, child().child)
        XCTAssertTrue(child().isChild)
        XCTAssertFalse(parent().isChild)
    }

    func testAsyncAcknowledgementIsNotDoneOrRunning() {
        XCTAssertTrue(aggregate([message(async: true, resolved: true)]).isEmpty)
        XCTAssertEqual(aggregate([message(async: true, resolved: true, error: true)]).first?.status, .error)
    }

    func testUnresolvedOnlyStartsWhileAssistantStillStreams() {
        XCTAssertEqual(aggregate([message(async: false, resolved: false, status: .streaming)]).first?.status, .starting)
        XCTAssertTrue(aggregate([message(async: false, resolved: false)]).isEmpty)
        XCTAssertTrue(aggregate([message(async: true, resolved: false, status: .aborted)]).isEmpty)
    }

    func testSyncTerminalTruthWinsOverRunningSnapshot() {
        let entries = aggregate([message(async: false, resolved: true)], runs: [makeRun(mode: .sync)])
        XCTAssertEqual(entries.count, 1)
        XCTAssertEqual(entries[0].status, .done)
        XCTAssertEqual(entries[0].model, "provider/model")
        XCTAssertEqual(entries[0].progress, "Progress")
        XCTAssertEqual(entries[0].id, "run")
    }

    func testAsyncSnapshotWinsAndDisappearingSnapshotNeverResurrectsAck() {
        let doc = [message(async: true, resolved: true)]
        XCTAssertEqual(aggregate(doc, runs: [makeRun()]).first?.status, .running)
        XCTAssertEqual(aggregate(doc, runs: [makeRun(status: .error)]).first?.status, .error)
        XCTAssertTrue(aggregate(doc).isEmpty)
    }

    func testStalenessBoundaryAndCountsAreDistinct() {
        XCTAssertEqual(aggregate(runs: [makeRun(updated: now - 45_000)]).first?.status, .running)
        let entries = aggregate(runs: [makeRun(updated: now - 45_001)])
        XCTAssertEqual(entries.first?.status, .stale)
        let counts = SubagentProjection.counts(entries)
        XCTAssertEqual(counts.running, 0)
        XCTAssertEqual(counts.stale, 1)
        XCTAssertEqual(counts.done, 0)
        XCTAssertEqual(counts.total, 1)
        XCTAssertEqual(SubagentProjection.snapshotStatus(makeRun(updated: Int64.min), now: Int64.max), .stale)
    }

    func testDurableChildSurvivesEmptySnapshotAndLinksDocByToolId() {
        let sessions = ["child": SessionRow(chatId: "child", deviceId: "host",
            status: .idle, startedAt: nil, updatedAt: now)]
        let entries = aggregate([message(async: false, resolved: true)], children: [child()], sessions: sessions)
        XCTAssertEqual(entries.count, 1)
        XCTAssertEqual(entries[0].childChatId, "child")
        XCTAssertEqual(entries[0].id, "run")
        XCTAssertEqual(entries[0].status, .done)
        XCTAssertEqual(aggregate(children: [child()], sessions: sessions).first?.status, .done)
    }

    func testChildSessionOwnStatusOverridesParentAndAwaitingInputIsLive() {
        for status in [SessionStatus.working, .awaitingInput, .errored, .idle] {
            let expected: SubagentPanelStatus = status == .errored ? .error : status == .idle ? .done : .running
            let entries = aggregate(runs: [makeRun(status: .done)], children: [child()], sessions: [
                "child": SessionRow(chatId: "child", deviceId: "host", status: status,
                                    startedAt: nil, updatedAt: now)])
            XCTAssertEqual(entries.first?.status, expected)
        }
        XCTAssertEqual(aggregate(children: [child()]).first?.status, .starting)
    }

    func testNavigationRequiresDurableSameParentAndDeviceRelation() {
        let entry = aggregate(runs: [makeRun()])[0]
        XCTAssertNil(SubagentProjection.navigableChild(entry, parent: parent(), chats: []))
        XCTAssertNotNil(SubagentProjection.navigableChild(entry, parent: parent(), chats: [child()]))
        var foreign = child()
        foreign.deviceId = "other-host"
        XCTAssertNil(SubagentProjection.navigableChild(entry, parent: parent(), chats: [foreign]))
        foreign = child()
        foreign.child?.parentChatId = "other-parent"
        XCTAssertNil(SubagentProjection.navigableChild(entry, parent: parent(), chats: [foreign]))
        XCTAssertTrue(aggregate(children: [foreign]).isEmpty)
    }

    func testDuplicateSnapshotsAreNotDoubleCounted() {
        XCTAssertEqual(aggregate(runs: [makeRun(), makeRun()]).count, 1)
    }

    func testMessageModeWithoutToolPartAndOrdering() {
        var messageRun = makeRun()
        messageRun.runId = "message-run"
        messageRun.toolCallId = nil
        messageRun.mode = .message
        messageRun.childChatId = nil
        let entries = aggregate(runs: [makeRun(status: .done), messageRun])
        XCTAssertEqual(entries.count, 2)
        XCTAssertEqual(entries.first?.id, "message-run")
        XCTAssertEqual(SubagentProjection.counts(entries).done, 1)
    }

    func testProgressIsBounded() {
        let progress = (0..<20).map { "line \($0)" }.joined(separator: "\n")
        XCTAssertEqual(SubagentProjection.boundedProgress(progress)?.components(separatedBy: "\n").count, 8)
        XCTAssertEqual(SubagentProjection.boundedProgress(String(repeating: "a", count: 9000))?.count, 4096)
    }

    func testNestedNavigationAndReturningToParentReuseStack() {
        let root: [Route] = [.space("project"), .chat("parent")]
        let child = SessionNavigation.opening("child", in: root)
        let grandchild = SessionNavigation.opening("grandchild", in: child)
        XCTAssertEqual(grandchild, root + [.chat("child"), .chat("grandchild")])
        XCTAssertEqual(SessionNavigation.opening("parent", in: grandchild), root)
        XCTAssertEqual(SessionNavigation.opening("child", in: child), child)
    }

    @MainActor
    func testWorkspaceProjectsChildrenAndSnapshotsWithoutPollutingRootLists() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".sqlite")
        defer { for suffix in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: url.path + suffix) } }
        let journal = try Workspace3Journal(url: url, scope: Workspace3Scope(endpoint: "local", org: "test", user: "test", actor: "ios-test"))
        func write(_ kind: String, _ id: String, _ fields: [String: JSONValue]) throws {
            try journal.mutate([Workspace3Op(kind: kind, id: id, op: .upsert, set: fields, hlc: "")], now: now)
        }
        try write("spaces", "project", [
            "deviceId": .string("host"), "path": .string("/project")])
        try write("chats", "parent", [
            "deviceId": .string("host"), "spaceId": .string("project")])
        let relation: JSONValue = .object([
            "parentChatId": .string("parent"), "parentRunId": .string("run"),
            "agent": .string("planner"), "task": .string("Plan"), "mode": .string("async"),
            "toolCallId": .string("tool"), "profile": .object(["systemPrompt": .string("keep")])])
        try write("chats", "child", [
            "deviceId": .string("host"), "spaceId": .string("project"), "child": relation])
        try write("chats", "broken", [
            "deviceId": .string("host"), "spaceId": .string("project"), "child": .object([:])])
        let runs = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode([makeRun()]))
        let config = AppConfig(edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "Test")
        // Never start the store: no network, no disk, no Runtime/LLM.
        let store = WorkspaceStore(config: config, initialJournal: journal)
        store.consume(.frame(1, ["type": .string("presence"), "role": .string("host"),
            "actor": .string("host"), "connection": .string("connection"), "expiresAt": .int(nowMs() + 45000),
            "state": .object(["sessions": .array([.object([
                "chatId": .string("parent"), "deviceId": .string("host"), "status": .string("working"),
                "updatedAt": .int(now), "subagents": runs
            ])])])]))
        XCTAssertEqual(store.chats(in: "project").map(\.id), ["parent"])
        XCTAssertEqual(store.overviewChats.map(\.id), ["parent"])
        XCTAssertNotNil(store.chats.first { $0.id == "child" })
        XCTAssertNil(store.chats.first { $0.id == "broken" })
        XCTAssertEqual(store.sessions["parent"]?.subagents.first?.runId, "run")
        store.setArchived(chatId: "child", archived: true)
        XCTAssertTrue(store.archivedChats(in: "project").isEmpty)
        store.setChatConfig(chatId: "child", config: ChatConfig(
            harness: "pi", model: "provider/new", reasoning: "high", sandbox: nil))
        XCTAssertEqual(try journal.row(kind: "chats", id: "child")?.fields["child"], relation,
                       "Editing run config must never rewrite the persisted child profile")
    }

    func testRealDocDecoderRetainsOnlySubagentDisplayMetadata() throws {
        let doc = LoroDoc()
        let message = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try message.insert(key: "id", v: "m")
        try message.insert(key: "role", v: "assistant")
        try message.insert(key: "status", v: "streaming")
        try message.insert(key: "parts", v: LoroValue.fromJSON([[
            "id": "tool", "kind": "tool", "progress": "Checking…",
            "call": ["kind": "unknown", "name": "subagent",
                     "input": ["agent": "planner", "task": "Plan", "async": true, "privateField": "not displayed"]]
        ]]))
        doc.commit()
        let entries = try XCTUnwrap(SessionStore.decodeEntries(from: doc))
        guard case .tool(_, let call, _, _) = entries[0].parts[0] else { return XCTFail("tool missing") }
        XCTAssertEqual(call.subagent?.agent, "planner")
        XCTAssertEqual(call.subagent?.isAsync, true)
        XCTAssertNil(call.fields["input"])
        XCTAssertEqual(aggregate(entries).first?.progress, "Checking…")
    }
}

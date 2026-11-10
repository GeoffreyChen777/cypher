// The workspace chat's transcript and words: tool chips read as what the
// workspace did (tool_chips.rs), a turn's summary counts its acts, plan
// cards follow their record's phase, held memory writes and routine posts
// become cards, session links open in-app, and the chat's own lines.

import XCTest
@testable import Cypher

@MainActor
final class WorkspaceTranscriptTests: XCTestCase {
    private func call(_ name: String, _ fields: [String: AnyHashable] = [:]) -> RenderToolCall {
        var all = fields
        all["name"] = name
        return RenderToolCall(tag: "unknown", fields: all)
    }

    // MARK: Chips

    func testWorkspaceToolChipsReadAsWhatTheyDid() {
        XCTAssertEqual(call("propose_plan").chipLabel, "Proposed a plan")
        XCTAssertEqual(call("propose_plan").chipDetail, "", "the doc drops the input")
        XCTAssertEqual(call("propose_plan", ["title": " Fix rounding "]).chipDetail, "Fix rounding")
        XCTAssertEqual(call("list_sessions").chipDetail, "all")
        XCTAssertEqual(call("list_sessions", ["status": "active", "since_hours": 6.0]).chipDetail, "active, last 6h")
        XCTAssertEqual(call("list_sessions", ["project": "cypher"]).chipDetail, "in cypher")
        XCTAssertEqual(call("session_activity").chipDetail, "last 24h", "a day by default")
        XCTAssertEqual(call("session_activity", ["since_hours": Int64(72)]).chipDetail, "last 3d")
        XCTAssertEqual(call("session_activity", ["since_hours": 1000.0]).chipDetail, "last 14d", "capped at two weeks")
        XCTAssertEqual(
            call("search_sessions", ["query": "flaky", "project": "cypher"]).chipDetail, "\"flaky\" in cypher")
        XCTAssertEqual(call("start_session", ["title": "Fix it"]).chipLabel, "Started session")
        XCTAssertEqual(call("list_schedule").chipLabel, "Checked routines", "the old name")
        XCTAssertEqual(call("ask_bot", ["bot": "Ledger"]).chipLabel, "Asked workspace")
        XCTAssertEqual(call("memory").chipLabel, "Memory", "no command kept")
        XCTAssertEqual(call("memory", ["command": "view", "path": "/project/testing.md"]).chipLabel, "Read memory")
        XCTAssertEqual(call("memory", ["command": "view", "path": "/project/testing.md"]).chipDetail, "testing.md")
        XCTAssertEqual(call("memory", ["command": "insert"]).chipLabel, "Updated memory")
        XCTAssertEqual(
            call("memory", ["command": "rename", "old_path": "/a.md", "new_path": "/b/c.md"]).chipDetail, "a.md → c.md")
        XCTAssertEqual(call("propose_plan").chipSymbol, "checklist")
        // Not a workspace tool: the ordinary chip.
        XCTAssertNil(call("bash_helper").botToolKind)
        XCTAssertNil(RenderToolCall(tag: "exec", fields: ["name": "propose_plan"]).botToolKind)
    }

    func testEveryWorkspaceToolEitherActsOrReads() {
        for kind in BotToolKind.allCases {
            // Waiting on a session is neither: it counts as a tool.
            XCTAssertTrue((kind.action != nil) != (kind.read != nil) || kind == .waitForSession, "\(kind) counts once")
            XCTAssertFalse(kind.chipContent(arg: { _ in nil }).label.isEmpty)
        }
    }

    func testTurnSummariesLeadWithTheWorkspacesActs() {
        func item(_ id: String, _ call: RenderToolCall) -> ToolItem {
            ToolItem(id: id, call: call, isError: false, resolved: true)
        }
        XCTAssertEqual(
            toolGroupSummary([
                item("a", call("read_session")), item("b", call("propose_plan")), item("c", call("read_session")),
            ]),
            "Proposed a plan · read 2 sessions")
        XCTAssertEqual(
            toolGroupSummary([
                item("a", call("start_session")), item("b", call("start_session")),
                item("c", call("create_worktree")), item("d", call("update_step")),
            ]),
            "Created a worktree · started 2 sessions · updated a step")
        XCTAssertEqual(
            toolGroupSummary([
                item("a", call("list_projects")), item("b", call("search_sessions")),
                item("c", RenderToolCall(tag: "search", fields: [:])),
            ]),
            "Looked over your work · 2 searches")
        XCTAssertEqual(toolGroupSummary([item("a", call("memory"))]), "Read memory")
        XCTAssertEqual(
            toolGroupSummary([item("a", call("start_session")), item("b", call("wait_for_session"))]),
            "Started a session · 1 tool")
    }

    // MARK: Plan card

    func testPlanPhasesFollowTheRecordAndPendingDecisions() throws {
        let demo = DemoDataset.standard()
        let phases = try ["work-rounding", "work-flaky", "work-notes", "work-loro", "work-docs"].map {
            try XCTUnwrap(demo.work(id: $0)).phase
        }
        XCTAssertEqual(phases, [.awaiting, .running, .done, .failed, .stopped])
        var work = try XCTUnwrap(demo.work(id: "work-rounding"))
        XCTAssertEqual(work.cardStatus(), "Awaiting approval")
        work.decision = PlanDecision(approve: true, at: 1)
        XCTAssertEqual(work.phase, .starting, "approved here, not started yet")
        work.decision = PlanDecision(approve: false, at: 1)
        XCTAssertEqual(work.phase, .cancelling)
        var running = try XCTUnwrap(demo.work(id: "work-flaky"))
        let started = try XCTUnwrap(running.startedAt)
        XCTAssertEqual(running.cardStatus(now: started + 12 * 60_000), "Running · 12m")
        running.stopRequestedAt = started + 1
        XCTAssertEqual(running.phase, .stopping)
        XCTAssertEqual(running.cardStatus(), "Stopping…")
        let done = try XCTUnwrap(demo.work(id: "work-notes"))
        XCTAssertEqual(done.cardStatus(), "Done · 1h")
    }

    func testPlanCardsFindTheirPlanByCallElseByTitle() {
        var records = BotRecords()
        let demo = DemoDataset.standard()
        records.work = demo.botRecords.work
        XCTAssertEqual(records.plan(forCall: "call-plan-flaky", chatId: "chat-atlas")?.id, "work-flaky")
        // Written before its call id was known: the chat's plan of that title.
        if let ix = records.work.firstIndex(where: { $0.id == "work-notes" }) { records.work[ix].toolCallId = nil }
        XCTAssertEqual(
            records.plan(forCall: "call-x", chatId: "chat-atlas", title: " Draft the 0.9 release notes ")?.id,
            "work-notes")
        XCTAssertNil(records.plan(forCall: "call-x", chatId: "chat-other", title: "Draft the 0.9 release notes"))
        XCTAssertNil(records.plan(forCall: "call-x", chatId: nil))
    }

    func testTheDemoChatShowsAPlanCardInEveryPhase() {
        let entries = DemoDataset.standard().sessionStore(for: "chat-atlas").fedEntries
        let plans = entries.flatMap(\.parts).compactMap { part -> String? in
            if case .tool(let id, let call, _, _) = part, call.botToolKind == .proposePlan { return id }
            return nil
        }
        XCTAssertEqual(
            plans, ["call-plan-docs", "call-plan-loro", "call-plan-notes", "call-plan-rounding", "call-plan-flaky"])
        XCTAssertEqual(entries.map(\.createdAt), entries.map(\.createdAt).sorted(), "in time order")
    }

    // MARK: Rows

    func testPlansHeldWritesAndRoutinePostsBecomeCards() {
        let entry = MessageEntry.fixture(
            "a",
            parts: [
                .tool(id: "r1", call: call("read_session"), isError: false, resolved: true),
                .tool(id: "plan", call: call("propose_plan"), isError: false, resolved: true),
                .tool(id: "held", call: call("memory"), isError: false, resolved: true),
                .tool(id: "plain", call: call("memory"), isError: false, resolved: true),
                .tool(id: "refused", call: call("propose_plan"), isError: true, resolved: true),
            ])
        let post = MessageEntry.fixture(
            "p",
            parts: [
                .post(
                    id: "x", text: "**Morning**",
                    from: .routine(routineId: "r", name: "Brief", outcome: .done, runAt: 1, threadChatId: "t"))
            ], createdAt: 2)
        let unknown = MessageEntry.fixture("u", parts: [.post(id: "y", text: "Hello", from: .unknown)], createdAt: 3)
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        let rows = TranscriptRowBuilder.rows(
            entries: [entry, post, unknown], pendingSends: [], parsers: &parsers, completed: &completed,
            memoryCards: ["held"])
        var plans: [String] = []
        var memory: [String] = []
        var headers = 0
        for row in rows {
            switch row.kind {
            case .botPlan(let id, let resolved):
                plans.append(id)
                XCTAssertTrue(resolved)
            case .memoryWrite(let id): memory.append(id)
            case .postHeader: headers += 1
            default: break
            }
        }
        XCTAssertEqual(plans, ["plan"], "a refused plan stays a chip")
        XCTAssertEqual(memory, ["held"], "a write nobody held stays a chip")
        XCTAssertEqual(headers, 1, "an unknown source is plain text")
    }

    func testMemoryCardsShowHeldThenDecidedWrites() throws {
        let records = DemoDataset.standard().botRecords
        XCTAssertTrue(records.memoryCardIds.isSuperset(of: ["call-memory-held", "call-memory-saved"]))
        guard case .held(_, let write)? = records.memoryCard("call-memory-held") else {
            return XCTFail("held")
        }
        XCTAssertFalse(write.path.isEmpty)
        XCTAssertEqual(records.memoryCard("call-memory-saved"), .decided(saved: true))
        XCTAssertNil(records.memoryCard("call-nothing"))
        XCTAssertEqual(MemoryWriteCard.fileName("/project/decisions/rounding.md/"), "rounding.md")
        XCTAssertEqual(MemoryWriteCard.reasonLine(""), "Held because this session read untrusted content")
        XCTAssertEqual(
            MemoryWriteCard.reasonLine(" read an answer from Harbor. "),
            "Held because this session read an answer from Harbor")
        XCTAssertEqual(MemoryWriteCard.previewLines("a\nb").1, false)
        XCTAssertEqual(MemoryWriteCard.previewLines((1...9).map(String.init).joined(separator: "\n")).1, true)
    }

    func testSessionLinksNameAChat() throws {
        XCTAssertEqual(SessionLink.chatId("cypher://session/chat-atlas-w2/"), "chat-atlas-w2")
        XCTAssertNil(SessionLink.chatId("cypher://session/"))
        XCTAssertNil(SessionLink.chatId("cypher://session/a b"))
        XCTAssertNil(SessionLink.chatId("https://example.com/session/x"))
        XCTAssertTrue(SessionLink.isSessionLink(try XCTUnwrap(URL(string: "cypher://session/%20"))))
    }

    // MARK: The chat's words

    func testAccessSummariesSayWhatAWorkspaceSeesAndDoes() {
        let names = ["a": "invoice-demo", "b": "cypher", "c": "webapp"]
        var bot = Bot(id: "b", name: "B", hostDeviceId: "d", chatId: "c", createdAt: 0)
        func summary() -> String { WorkspaceChatText.accessSummary(bot) { names[$0] } }
        XCTAssertEqual(summary(), "No access yet")
        XCTAssertFalse(WorkspaceChatText.hasAccess(bot))
        bot.scope = BotScope(projects: .selected(["a", "b"]))
        bot.capabilities = .newBot
        XCTAssertEqual(summary(), "invoice-demo and cypher · works directly · starts sessions")
        bot.capabilities = BotCapabilities()
        XCTAssertEqual(summary(), "invoice-demo and cypher · reads conversations")
        bot.scope = BotScope(projects: .selected(["a", "b", "c", "gone"]), transcripts: false)
        XCTAssertEqual(summary(), "3 projects · titles and status only")
        bot.scope = BotScope(projects: .all)
        bot.capabilities = BotCapabilities(workDirectly: false, coordinate: true, steer: true)
        XCTAssertEqual(summary(), "All projects · starts and steers sessions")
        XCTAssertEqual(WorkspaceChatText.needsYou(1), "1 needs you")
        XCTAssertEqual(WorkspaceChatText.needsYou(3), "3 need you")
        XCTAssertEqual(WorkspaceChatText.suggestions(firstProject: "cypher").count, 4)
        XCTAssertEqual(WorkspaceChatText.suggestions(firstProject: nil).count, 3)
    }

    func testShortcutsMoveOnePlaceAndStopAtTheEnds() {
        XCTAssertEqual(WorkspaceChatText.moved(["a", "b", "c"], "b", by: -1), ["b", "a", "c"])
        XCTAssertEqual(WorkspaceChatText.moved(["a", "b", "c"], "b", by: 1), ["a", "c", "b"])
        XCTAssertNil(WorkspaceChatText.moved(["a", "b"], "a", by: -1))
        XCTAssertNil(WorkspaceChatText.moved(["a", "b"], "b", by: 1))
        XCTAssertNil(WorkspaceChatText.moved(["a"], "z", by: 1))
        let shortcut = BotShortcut(
            id: "s", botId: "b", label: "Explain", request: "Explain {input} simply", createdAt: 0, updatedAt: 0)
        XCTAssertTrue(shortcut.wantsInput)
        XCTAssertEqual(shortcut.request(with: " the diff "), "Explain the diff simply")
    }

    func testHiddenChatsNameWhatTheyBelongTo() throws {
        let demo = DemoDataset.standard()
        func chat(_ id: String) throws -> Chat { try XCTUnwrap(demo.chats.first { $0.id == id }) }
        XCTAssertEqual(
            ThreadOrigin.of(try chat("chat-atlas-thread-flaky"), in: demo)?.text,
            "Plan · Stabilise the flaky sync tests")
        XCTAssertEqual(ThreadOrigin.of(try chat("chat-atlas-thread-flaky"), in: demo)?.page, .workItem("work-flaky"))
        XCTAssertEqual(ThreadOrigin.of(try chat("chat-atlas-routine-brief"), in: demo)?.text, "Routine · Morning brief")
        XCTAssertEqual(ThreadOrigin.of(try chat("chat-atlas-peer-ledger"), in: demo)?.text, "Question from Ledger")
        XCTAssertNil(ThreadOrigin.of(try chat("chat-atlas"), in: demo))
        XCTAssertNil(ThreadOrigin.of(try chat("chat-atlas-w2"), in: demo))
        // Read-only: the workspace's threads and memory passes; its chat and
        // a project routine's session stay writable.
        XCTAssertTrue(try chat("chat-atlas-thread-flaky").isReadOnlyThread)
        XCTAssertTrue(try chat("chat-memory-pass").isReadOnlyThread)
        XCTAssertFalse(try chat("chat-atlas").isReadOnlyThread)
        XCTAssertFalse(try chat("chat-atlas-routine-tests").isReadOnlyThread)
        // Their subtitles name the workspace, not its folder; sessions in a
        // project keep the project.
        XCTAssertEqual(WorkspaceChatText.threadOwner(try chat("chat-atlas-thread-flaky"), in: demo), "Atlas")
        XCTAssertEqual(WorkspaceChatText.threadOwner(try chat("chat-atlas"), in: demo), "Atlas")
        XCTAssertEqual(WorkspaceChatText.threadOwner(try chat("chat-memory-pass"), in: demo), "Memory")
        XCTAssertNil(WorkspaceChatText.threadOwner(try chat("chat-atlas-routine-tests"), in: demo))
        XCTAssertNil(WorkspaceChatText.threadOwner(try chat("chat-atlas-w2"), in: demo))
    }

    func testTheBoardSheetOpensFullHeightForFormsAndDecisions() {
        XCTAssertFalse(BotPage.overview.wantsFullHeight)
        XCTAssertFalse(BotPage.needsYou.wantsFullHeight)
        XCTAssertFalse(BotPage.activity.wantsFullHeight)
        for page: BotPage in [
            .approval("a"), .exchange("e"), .routineForm(routineId: nil), .connect, .workItem("w"),
        ] {
            XCTAssertTrue(page.wantsFullHeight, "\(page)")
        }
    }

    func testEmberOpensOnItsStartScreen() throws {
        let demo = DemoDataset.standard()
        let ember = try XCTUnwrap(demo.bot(id: DemoDataset.emberId))
        XCTAssertTrue(demo.sessionStore(for: ember.chatId).fedEntries.isEmpty)
        XCTAssertTrue(WorkspaceChatText.hasAccess(ember))
        XCTAssertEqual(demo.accessSummary(of: ember), "webapp · reads conversations")
        XCTAssertFalse(demo.activeChats.contains { $0.id == ember.chatId }, "its chat stays out of the lists")
    }
}

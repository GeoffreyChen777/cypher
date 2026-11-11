// The derived workspace lists (Needs you, work, routines, connections) on the
// demo dataset, the demo's actions, and the board's launch specs and words.

import XCTest
@testable import Cypher

@MainActor
final class BotDemoTests: XCTestCase {
    func testNeedsYouPutsTheMostPressingFirst() {
        // bot_records.rs `needs_you_orders_by_urgency`, plus the workspace
        // chat's own question first (bots.rs `bot_needs`).
        func work(_ id: String, _ state: WorkState, _ sessions: [String], bot: String = "b") -> BotWork {
            BotWork(
                id: id, botId: bot, title: id,
                steps: [WorkStep(id: "1", title: "s", state: .running, sessions: sessions)],
                state: state, chatId: "c", createdAt: 0)
        }
        var records = BotRecords()
        records.work = [
            work("plan", .proposed, []), work("run", .running, ["s1", "s2"]),
            work("other-bot", .proposed, [], bot: "x"),
            work("ended", .done, ["s3"]),
        ]
        records.approvals = [
            BotApproval(id: "a", botId: "b", threadChatId: "t", action: "Start", reason: "Peer", createdAt: 0)
        ]
        records.exchanges = [
            BotExchange(
                id: "e", botId: "b", connectionId: "k", direction: .incoming, question: "Q", state: .awaitingReview,
                askedAt: 0)
        ]
        let asking: Set<String> = ["s2", "s3", "c"]
        XCTAssertEqual(
            records.needsYou(botId: "b", chatId: "c", asking: asking.contains),
            [
                .chat("c"), .approval("a"), .asking(chatId: "s2", workId: "run"),
                .asking(chatId: "s3", workId: "ended"), .plan("plan"), .review("e"),
            ])
        XCTAssertEqual(records.needsYou(botId: "b", chatId: "quiet", asking: asking.contains).first, .approval("a"))
        XCTAssertEqual(records.startedBy(chatId: "s1")?.work.id, "run")
    }

    func testTheDemoWorkspaceNeedsYouForFourThings() throws {
        let demo = DemoDataset.standard()
        XCTAssertEqual(demo.bots.map(\.name), ["Atlas", "Scout", "Ember"])
        XCTAssertEqual(
            demo.needsYou(botId: DemoDataset.atlasId),
            [
                .approval("approval-staging"), .asking(chatId: "chat-atlas-w3", workId: "work-flaky"),
                .plan("work-rounding"), .review("ex-review"),
            ])
        XCTAssertEqual(demo.botStatus(try XCTUnwrap(demo.bot(id: DemoDataset.atlasId))), .needsYou(4))
        XCTAssertEqual(demo.botStatus(try XCTUnwrap(demo.bot(id: DemoDataset.scoutId))), .paused)
        XCTAssertEqual(
            demo.work(of: DemoDataset.atlasId).map(\.id),
            ["work-flaky", "work-rounding", "work-notes", "work-loro", "work-docs"])
        XCTAssertEqual(demo.routines(of: DemoDataset.atlasId).last?.id, "routine-triage", "manual runs last")
        XCTAssertEqual(
            demo.connections(of: DemoDataset.atlasId).map(\.id), ["conn-ledger", "conn-harbor", "conn-northwind"])
        XCTAssertEqual(demo.openInvites(of: DemoDataset.atlasId).map(\.id), ["invite-open"])
        XCTAssertEqual(demo.shortcuts(of: DemoDataset.atlasId).first?.label, "What's running?")
        XCTAssertEqual(Set(demo.activity(of: DemoDataset.atlasId).map(\.kind.group)), Set(ActivityGroup.allCases))
        XCTAssertEqual(demo.startedBy(chatId: "chat-atlas-w2")?.step.id, "2")
        // The plan card and memory cards find their records by tool call.
        let transcript = demo.sessionStore(for: "chat-atlas").fedEntries
        let toolIds = Set(
            transcript.flatMap(\.parts).compactMap { part -> String? in
                if case .tool(let id, _, _, _) = part { return id }
                return nil
            })
        XCTAssertTrue(toolIds.contains("call-plan-rounding"))
        let store = try XCTUnwrap(demo.memoryStore(scope: .workspace(botId: DemoDataset.atlasId)))
        XCTAssertNotNil(store.heldForCall("call-memory-held"))
        XCTAssertNotNil(store.decidedForCall("call-memory-saved"))
        XCTAssertTrue(toolIds.isSuperset(of: ["call-memory-held", "call-memory-saved"]))
        XCTAssertTrue(transcript.flatMap(\.parts).contains { if case .post = $0 { true } else { false } })
    }

    func testDemoSessionListsLeaveWorkspaceChatsOut() {
        let demo = DemoDataset.standard()
        let hidden = Set(demo.chats.filter(\.isHiddenFromSessions).map(\.id))
        XCTAssertTrue(
            hidden.isSuperset(of: ["chat-atlas", "chat-scout", "chat-memory-pass", "chat-atlas-routine-tests"]))
        for list in [demo.activeChats, demo.overviewChats, demo.projectlessChats, demo.archivedChats()] {
            XCTAssertTrue(Set(list.map(\.id)).isDisjoint(with: hidden))
        }
        XCTAssertTrue(
            demo.chats(in: "space-cypher").contains { $0.id == "chat-atlas-w2" }, "work's sessions stay listed")
        XCTAssertEqual(demo.chats(in: "space-edge").map(\.id), ["chat-deploy"])
    }

    func testDemoActionsKeepTheLiveGuards() async throws {
        let demo = DemoDataset.standard()
        try await demo.resolvePlan(workId: "work-rounding", approve: true)
        XCTAssertFalse(demo.needsYou(botId: DemoDataset.atlasId).contains(.plan("work-rounding")))
        do {
            try await demo.resolvePlan(workId: "work-rounding", approve: true)
            XCTFail("decided twice")
        } catch let error as BotActionError {
            XCTAssertEqual(error, .alreadyDecided)
        }
        do {
            try await demo.deleteWork(workId: "work-flaky")
            XCTFail("running work")
        } catch let error as BotActionError {
            XCTAssertEqual(error, .stillLive)
        }
        try await demo.deleteWork(workId: "work-notes")
        XCTAssertNil(demo.chat(id: "chat-atlas-thread-notes"))
        try await demo.reviewExchange(exchangeId: "ex-review", send: true, text: nil)
        XCTAssertFalse(demo.needsYou(botId: DemoDataset.atlasId).contains(.review("ex-review")))
        try await demo.orderShortcuts(botId: DemoDataset.atlasId, ids: ["shortcut-prs", "shortcut-status"])
        XCTAssertEqual(demo.shortcuts(of: DemoDataset.atlasId).first?.id, "shortcut-prs")
        try await demo.setBotPaused(botId: DemoDataset.scoutId, paused: false)
        XCTAssertEqual(demo.botStatus(try XCTUnwrap(demo.bot(id: DemoDataset.scoutId))), .hostOffline)
        do {
            _ = try await demo.createInvite(botId: DemoDataset.scoutId)
            XCTFail("Scout's host is offline")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "Scout's device is offline.")
        }
    }

    func testDemoMemoryEditsReachWatchers() async throws {
        let demo = DemoDataset.standard()
        let scope = MemoryScope.workspace(botId: DemoDataset.atlasId)
        var iterator = demo.watchMemory(scope: scope).makeAsyncIterator()
        let first = try await iterator.next()
        XCTAssertEqual(first?.pending.map(\.id), ["held-ledger"])
        _ = try await demo.editMemory(scope: scope, edit: .accept(id: "held-ledger"))
        let next = try await iterator.next()
        XCTAssertEqual(next?.pending, [])
        XCTAssertTrue(next?.files.first { $0.path == "/invoices.md" }?.text.contains("half-up") ?? false)
        XCTAssertEqual(demo.memoryStore(scope: scope)?.decidedForCall("call-memory-held")?.saved, true)
        XCTAssertNil(demo.memoryStore(scope: scope)?.heldForCall("call-memory-held"))
        _ = try await demo.editMemory(scope: scope, edit: .write(path: "/new.md", text: "# New"))
        let written = try await iterator.next()
        XCTAssertEqual(written?.files.map(\.path).first, "/MEMORY.md", "the index stays first")
        XCTAssertTrue(written?.files.contains { $0.path == "/new.md" } ?? false)
    }

    func testADemoStoreNothingWroteStartsEmptyAndTakesAFile() async throws {
        let demo = DemoDataset.standard()
        let scope = MemoryScope.project(spaceId: "space-webapp")
        XCTAssertNil(demo.memoryStore(scope: scope))
        var iterator = demo.watchMemory(scope: scope).makeAsyncIterator()
        let first = try await iterator.next()
        XCTAssertEqual(first?.files, [])
        XCTAssertEqual(first?.store.hostDeviceId, "dev-mac", "the project's device")
        let reply = try await demo.editMemory(scope: scope, edit: .write(path: "/notes.md", text: "# Notes"))
        XCTAssertEqual(reply, "Created /notes.md.")
        let written = try await iterator.next()
        XCTAssertEqual(written?.files.map(\.path), ["/notes.md"])
        XCTAssertEqual(demo.memoryStore(scope: scope)?.files, 1, "listed in Settings › Memory from now on")
    }

    func testADemoMemoryPassRunsAndEnds() async throws {
        let demo = DemoDataset.standard()
        let scope = MemoryScope.workspace(botId: DemoDataset.atlasId)
        _ = try await demo.editMemory(scope: scope, edit: .runPass)
        let store = try XCTUnwrap(demo.memoryStore(scope: scope))
        XCTAssertTrue(MemoryFormat.passWaiting(store))
        try await Task.sleep(for: .seconds(2))
        let ended = try XCTUnwrap(demo.memoryStore(scope: scope))
        XCTAssertFalse(MemoryFormat.passWaiting(ended), "the host ran it")
        XCTAssertEqual(ended.lastPass?.outcome, .skipped)
        XCTAssertEqual(ended.lastPass?.manual, true)
    }

    func testBoardLaunchSpecsNameEveryPage() {
        let cases: [(String, BotPage)] = [
            ("board:overview", .overview), ("board:needs", .needsYou), ("board:work", .work),
            ("board:work:work-flaky", .workItem("work-flaky")), ("board:approval:a1", .approval("a1")),
            ("board:asking:work-flaky:chat-atlas-w3", .asking(chatId: "chat-atlas-w3", workId: "work-flaky")),
            ("board:routines", .routines), ("board:routine:r1", .routine("r1")),
            ("board:routine-form", .routineForm(routineId: nil)),
            ("board:routine-form:r1", .routineForm(routineId: "r1")),
            ("board:projects", .projects), ("board:project:space-cypher", .project("space-cypher")),
            ("board:activity", .activity), ("board:connections", .connections),
            ("board:connection:conn-ledger", .connection("conn-ledger")),
            ("board:exchange:ex-review", .exchange("ex-review")),
            ("board:connect", .connect), ("board:memory", .memory(.workspace(botId: "b"))),
            ("board:memory:you", .memory(.you)),
            ("board:memory:project:space-cypher", .memory(.project(spaceId: "space-cypher"))),
            ("board:memory-file:/MEMORY.md", .memoryFile(.workspace(botId: "b"), path: "/MEMORY.md")),
        ]
        for (spec, page) in cases {
            XCTAssertEqual(BotPage(launchSpec: spec, botId: "b"), page, spec)
        }
        XCTAssertNil(BotPage(launchSpec: "board:nope", botId: "b"))
        XCTAssertNil(BotPage(launchSpec: "config", botId: "b"))
        XCTAssertEqual(BotPage.settling(.review("e")), .exchange("e"))
        XCTAssertNil(BotPage.settling(.chat("c")))
    }

    func testStatesReadInTheDesktopsWords() {
        XCTAssertEqual(BotTime.span(40_000), "40s")
        XCTAssertEqual(BotTime.span(12 * 60_000), "12m")
        XCTAssertEqual(BotTime.span(65 * 60_000), "1h 5m")
        XCTAssertEqual(BotTime.span(2 * 3_600_000), "2h")
        XCTAssertEqual(BotTime.span(3 * 86_400_000), "3d")
        let now: Int64 = 1_000_000_000_000
        XCTAssertEqual(BotTime.ago(now - 30_000, now: now), "just now")
        XCTAssertEqual(BotTime.ago(now - 5 * 60_000, now: now), "5m ago")
        XCTAssertEqual(BotTime.next(now + 25 * 60_000, now: now), "in 25m")
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // Monday 2026-11-09 16:15 UTC, seen the next day.
        let monday: Int64 = 1_794_240_900_000
        XCTAssertEqual(BotTime.past(monday, now: monday + 86_400_000, calendar: calendar), "Yesterday 16:15")
        XCTAssertEqual(BotTime.past(monday, now: monday + 3 * 86_400_000, calendar: calendar), "Nov 9, 16:15")
        XCTAssertEqual(BotTime.nextFull(monday, now: monday - 86_400_000, calendar: calendar), "Tomorrow 16:15")
        var work = BotWork(
            id: "w", botId: "b", title: "T",
            steps: [
                WorkStep(id: "1", title: "A", state: .done), WorkStep(id: "2", title: "B", state: .running),
                WorkStep(id: "3", title: "C"),
            ],
            state: .proposed, chatId: "c", createdAt: 0)
        XCTAssertEqual(work.phaseLine(), "Awaiting your approval · 3 steps")
        XCTAssertEqual(work.phase.tone, .waiting)
        work.state = .running
        work.startedAt = now - 12 * 60_000
        XCTAssertEqual(work.phaseLine(now: now), "Running · step 2 of 3 · 12m")
        XCTAssertEqual(work.currentStepLine, "Step 2 of 3 · B")
        work.stopRequestedAt = now
        XCTAssertEqual(work.phase, .stopping)
        work.state = .done
        work.endedAt = now + 13 * 60_000
        XCTAssertEqual(work.phaseLine(now: now), "Done · took 25m")
        XCTAssertEqual(work.status().tone, .done)
        XCTAssertEqual(ReplyMode.review.label, "Review replies")
    }

    func testEveryAvatarKeyDraws() {
        for face in BotFace.allCases {
            XCTAssertEqual(BotStyle.glyph(face.rawValue), .face(face))
        }
        for (key, _) in BotStyle.symbols {
            XCTAssertNotEqual(BotStyle.glyph(key), .initial, "\(key) has no SF Symbol on this OS")
        }
        XCTAssertEqual(BotStyle.glyph("unicorn"), .initial)
        for key in BotShortcut.icons {
            XCTAssertNotNil(UIImage(systemName: BotStyle.shortcutSymbol(key)), key)
        }
        XCTAssertNotNil(BotStyle.color("indigo"))
        XCTAssertNil(BotStyle.color("mauve"))
        XCTAssertEqual(BotStyle.initial("  atlas"), "A")
    }
}

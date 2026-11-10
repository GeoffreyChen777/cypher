// The board's logic apart from its views: the Needs you rows, the Work lists
// and their lines, routine states and order, project loads, the Connections
// summary, and the Activity log read apart, linked and grouped by day
// (ports of the desktop's board, needs, work, schedule and activity tests).

import XCTest
@testable import Cypher

@MainActor
final class BoardModelTests: XCTestCase {
    private func work(_ id: String, _ state: WorkState, at: Int64, steps: [WorkStep] = []) -> BotWork {
        BotWork(
            id: id, botId: "b", title: id, steps: steps, state: state, chatId: "c", createdAt: at, startedAt: at,
            endedAt: state == .running || state == .proposed ? nil : at)
    }

    private func step(_ title: String, _ state: StepState) -> WorkStep {
        WorkStep(id: title, title: title, state: state)
    }

    // MARK: Work

    func testWorkSortsIntoRunningProposedAndFinished() {
        // work.rs `work_sorts_into_running_proposed_and_finished`, through
        // the data layer's order.
        let demo = DemoDataset.standard()
        demo.botRecords.work = [
            work("old-run", .running, at: 1), work("new-run", .running, at: 5), work("plan", .proposed, at: 2),
            work("done", .done, at: 3), work("failed", .failed, at: 4), work("gone", .cancelled, at: 6),
        ]
        let lists = BoardWorkLists(demo.work(of: "b"))
        XCTAssertEqual(lists.running.map(\.id), ["new-run", "old-run"])
        XCTAssertEqual(lists.proposed.map(\.id), ["plan"])
        XCTAssertEqual(lists.finished.map(\.id), ["gone", "failed", "done"])
        XCTAssertEqual(lists.recent(2).map(\.id), ["failed", "done"], "cancelled proposals stay in the full list")
        XCTAssertTrue(BoardWorkLists([]).isEmpty)
    }

    func testTheDemoWorkReadsInItsLists() {
        let lists = BoardWorkLists(DemoDataset.standard().work(of: DemoDataset.atlasId))
        XCTAssertEqual(lists.running.map(\.id), ["work-flaky"])
        XCTAssertEqual(lists.proposed.map(\.id), ["work-rounding"])
        XCTAssertEqual(lists.finished.map(\.id), ["work-notes", "work-loro", "work-docs"])
    }

    func testLinesSayTheCurrentStepAndTheResult() {
        var running = work("w", .running, at: 0)
        running.steps = [step("Fix rounding", .done), step("Run the tests\nin web", .running), step("PR", .pending)]
        XCTAssertEqual(running.currentStepLine, "Step 2 of 3 · Run the tests in web")
        var done = work("w", .done, at: 0)
        done.endedAt = 60_000
        XCTAssertEqual(done.resultLine(), "Done · took 1m")
        done.result = "Opened PR #42\nwith the **fix**\n\n- and a list"
        XCTAssertEqual(done.resultLine(), "Opened PR #42 with the fix", "the first paragraph, plain")
    }

    func testTheRequestShowsOnlyWhenItSaysMore() {
        var item = work("Fix the build", .proposed, at: 0)
        XCTAssertFalse(item.showsRequest, "no request")
        item.instruction = "fix the build."
        XCTAssertFalse(item.showsRequest, "it repeats the title")
        item.instruction = "Fix the build on CI; it fails on Linux."
        XCTAssertTrue(item.showsRequest)
    }

    func testAProjectEveryStepNamesIsNamedOnce() {
        var item = work("w", .proposed, at: 0)
        item.steps = [WorkStep(id: "1", title: "a", project: "p"), WorkStep(id: "2", title: "b", project: "p")]
        XCTAssertEqual(item.commonProject, "p")
        item.steps.append(WorkStep(id: "3", title: "c", project: "q"))
        XCTAssertNil(item.commonProject)
        item.steps = []
        XCTAssertNil(item.commonProject)
    }

    func testMarkdownReadsPlainly() {
        XCTAssertEqual(
            plainMarkdown("## Title\nSee [the doc](https://x.y) and `code` **now**\n| a | b |\n|---|---|\n| 1 | 2 |"),
            "Title\nSee the doc and code now\na · b\n1 · 2")
        XCTAssertEqual(plainMarkdown("[not a link"), "[not a link")
    }

    // MARK: Needs you

    func testNeedsYouRowsSayWhatEachItemNeeds() throws {
        let demo = DemoDataset.standard()
        let bot = try XCTUnwrap(demo.bot(id: DemoDataset.atlasId))
        let rows = demo.boardNeedRows(bot)
        XCTAssertEqual(
            rows.map(\.item),
            [
                .approval("approval-staging"), .asking(chatId: "chat-atlas-w3", workId: "work-flaky"),
                .plan("work-rounding"), .review("ex-review"),
            ])
        XCTAssertEqual(rows[0].title, "Start a session in webapp: run the e2e suite against staging")
        XCTAssertEqual(rows[0].sub, "This thread read an answer from Harbor (Sam Ortiz's workspace).")
        XCTAssertEqual(rows[1].title, "Re-run the webapp e2e suite")
        XCTAssertEqual(rows[1].sub, "Asking · in webapp")
        XCTAssertEqual(rows[2].sub, "Plan to approve · 3 steps")
        XCTAssertEqual(rows[3].sub, "Review the answer to Ledger")
        XCTAssertEqual(rows.map(\.symbol), ["checkmark.shield", "questionmark.circle", "checklist", "person.2"])

        let chat = try XCTUnwrap(demo.boardNeedRow(.chat("chat-atlas"), bot: bot))
        XCTAssertEqual(chat.title, "Atlas asked you something")
        XCTAssertNil(BotPage.settling(chat.item), "its own question opens the chat")
        XCTAssertNil(demo.boardNeedRow(.plan("gone"), bot: bot), "a record that is gone is left out")
    }

    // MARK: Routines

    func testRoutineStatesSayTheMostPressingFirst() {
        let now: Int64 = 1_000_000_000
        func routine(
            _ schedule: Schedule = .daily(time: "09:00", days: []), enabled: Bool = true, next: Int64? = nil,
            last: RunOutcome? = nil, lastAt: Int64 = 0, asked: Int64? = nil
        ) -> BotRoutine {
            BotRoutine(
                id: "r", botId: "b", name: "R", prompt: "p", schedule: schedule, enabled: enabled, createdAt: 0,
                updatedAt: 0, runRequestedAt: asked, lastRun: last.map { RunRecord(at: lastAt, outcome: $0) },
                nextRunAt: next)
        }
        XCTAssertEqual(BoardRoutineState(routine(last: .running), now: now), .running)
        XCTAssertEqual(BoardRoutineState(routine(last: .done, lastAt: 5, asked: 9), now: now), .starting)
        XCTAssertEqual(BoardRoutineState(routine(next: now + 60_000, last: .done, lastAt: 9, asked: 5), now: now), .next(now + 60_000))
        XCTAssertEqual(BoardRoutineState(routine(.once(at: "2026-01-01T09:00"), last: .done), now: now), .done)
        XCTAssertEqual(BoardRoutineState(routine(enabled: false, next: now + 1), now: now), .off)
        XCTAssertEqual(BoardRoutineState(routine(.manual), now: now), .manual)
        XCTAssertEqual(BoardRoutineState(routine(next: now - 1), now: now), .unplanned, "a past plan is no plan")
        XCTAssertEqual(BoardRoutineState.next(now + 25 * 60_000).note(now: now), "in 25m")
        XCTAssertEqual(BoardRoutineState.off.note(now: now), "off")
        XCTAssertTrue(BoardRoutineState.done.dim)
    }

    func testRoutinesOrderByNextRunThenManualThenOff() {
        let demo = DemoDataset.standard()
        var ordered = BoardRoutineState.ordered(demo.routines(of: DemoDataset.atlasId))
        XCTAssertEqual(ordered.last?.id, "routine-triage", "manual after every scheduled one")
        let nexts = ordered.dropLast().map(\.nextRunAt)
        XCTAssertEqual(nexts, nexts.sorted { ($0 ?? 0) < ($1 ?? 0) })
        var off = demo.routines(of: DemoDataset.atlasId)[0]
        off.enabled = false
        ordered = BoardRoutineState.ordered(demo.routines(of: DemoDataset.atlasId) + [off])
        XCTAssertEqual(ordered.last?.enabled, false, "switched off last")
        XCTAssertEqual(
            demo.routine(id: "routine-tests")?.whereAndWhen { _ in "cypher" }, "cypher · Daily 02:00")
        XCTAssertEqual(
            demo.routine(id: "routine-brief")?.whereAndWhen { _ in nil }, "This workspace · Weekdays 09:00")
    }

    // MARK: Projects, connections, notice

    func testProjectLoadsCountSessionsInScope() throws {
        let demo = DemoDataset.standard()
        let bot = try XCTUnwrap(demo.bot(id: DemoDataset.atlasId))
        let loads = demo.boardProjects(bot)
        XCTAssertEqual(loads.map(\.space.id), ["space-cypher", "space-webapp"])
        let webapp = try XCTUnwrap(loads.last)
        XCTAssertGreaterThanOrEqual(webapp.asking, 1, "the e2e session asks")
        XCTAssertTrue(webapp.note.contains("asking"))
        XCTAssertEqual(demo.boardProjectMode(bot, space: webapp.space), "works directly")
        var excluded = bot
        excluded.scope.excludeSessions = ["chat-atlas-w3"]
        XCTAssertEqual(demo.boardProjects(excluded).last?.asking, webapp.asking - 1)

        XCTAssertEqual(BoardProjectLoad(space: webapp.space, sessions: 3).note, "3 sessions")
        XCTAssertEqual(BoardProjectLoad(space: webapp.space, sessions: 1).note, "1 session")
        XCTAssertEqual(BoardProjectLoad(space: webapp.space, sessions: 3, running: 2, asking: 1).note, "1 asking · 2 running")
    }

    func testConnectionsSayWhatWaitsForReviewElseHowManyThereAre() async throws {
        let demo = DemoDataset.standard()
        XCTAssertEqual(demo.boardConnections(DemoDataset.atlasId).text, "1 to review")
        XCTAssertEqual(demo.boardConnections(DemoDataset.atlasId).reviews, ["ex-review"])
        try await demo.reviewExchange(exchangeId: "ex-review", send: false, text: nil)
        XCTAssertEqual(demo.boardConnections(DemoDataset.atlasId).text, "3 connected · 1 open invite")
        XCTAssertEqual(demo.boardConnections(DemoDataset.scoutId).text, "None yet")
    }

    func testOnlySomethingOffEarnsANotice() {
        XCTAssertNil(BoardNotice.of(paused: false, hostOnline: true, host: "Mac", chat: .idle))
        XCTAssertEqual(BoardNotice.of(paused: false, hostOnline: false, host: "Mac", chat: .idle), .offline("Mac"))
        XCTAssertEqual(BoardNotice.of(paused: true, hostOnline: false, host: "Mac", chat: .errored), .paused)
        XCTAssertEqual(BoardNotice.of(paused: false, hostOnline: true, host: "Mac", chat: .errored), .failed)
        let demo = DemoDataset.standard()
        XCTAssertNil(demo.boardNotice(demo.bot(id: DemoDataset.atlasId)!))
        XCTAssertEqual(demo.boardNotice(demo.bot(id: DemoDataset.scoutId)!), .paused)
    }

    // MARK: Activity

    func testEntriesSplitIntoVerbSubjectAndDetail() {
        let cases: [(String, ActivityParts)] = [
            ("Ran \"Nightly tests\"", ActivityParts(verb: "Ran", subject: "Nightly tests")),
            (
                "\"Morning brief\" failed: harness protocol error: pi exited",
                ActivityParts(verb: "Failed", subject: "Morning brief", detail: "harness protocol error: pi exited")
            ),
            (
                "Started \"Add currency field\" in invoice-demo on Ada’s Mac mini",
                ActivityParts(
                    verb: "Started", subject: "Add currency field", detail: "in invoice-demo on Ada’s Mac mini")
            ),
            (
                "The plan failed: Add currency support to invoices",
                ActivityParts(verb: "The plan failed", subject: "Add currency support to invoices")
            ),
            ("Connected with \"Ledger\" (Dana's workspace)", ActivityParts(verb: "Connected with", subject: "Ledger · Dana")),
            ("\"Ledger\" (Dana's workspace) disconnected", ActivityParts(verb: "Disconnected", subject: "Ledger · Dana")),
            (
                "Asked \"Ledger\" (Dana's workspace): Is the fix out?",
                ActivityParts(verb: "Asked", subject: "Ledger · Dana", detail: "Is the fix out?")
            ),
            (
                "Declined after review: \"Ledger\" (Dana's workspace) asked Can you share it?",
                ActivityParts(verb: "Declined after review", subject: "Ledger · Dana", detail: "asked Can you share it?")
            ),
            ("Created: /workspace/routines.md", ActivityParts(verb: "Created", subject: "routines.md", detail: "/workspace")),
            (
                "Added 15 lines, removed 12: /workspace/MEMORY.md",
                ActivityParts(verb: "Edited", subject: "MEMORY.md", detail: "Added 15 lines, removed 12 · /workspace")
            ),
            (
                "Memory consolidated: I reorganized `/workspace/MEMORY.md` into an index. It links two files.",
                ActivityParts(
                    verb: "Memory consolidated",
                    detail: "I reorganized /workspace/MEMORY.md into an index. It links two files.")
            ),
            (
                "Created worktree /Users/a/worktrees/demo/swift-onyx (cypher/swift-onyx) in invoice-demo",
                ActivityParts(
                    verb: "Created worktree", subject: "swift-onyx", detail: "(cypher/swift-onyx) in invoice-demo")
            ),
            ("Something else entirely", ActivityParts(verb: "Something else entirely")),
            // Curly quotes read the same.
            (
                "Proposed “Fix invoice rounding in CSV exports”",
                ActivityParts(verb: "Proposed", subject: "Fix invoice rounding in CSV exports")
            ),
            (
                "“Upgrade Loro to 1.6” failed: snapshot decoding",
                ActivityParts(verb: "Failed", subject: "Upgrade Loro to 1.6", detail: "snapshot decoding")
            ),
            (
                "Nightly tests failed: 2 tests in registry_sync",
                ActivityParts(verb: "Nightly tests failed", subject: "2 tests in registry_sync")
            ),
        ]
        for (text, want) in cases {
            XCTAssertEqual(ActivityParts(text), want, text)
        }
    }

    func testAFailureIsMarked() {
        func entry(_ kind: ActivityKind, _ text: String) -> BotActivity {
            BotActivity(id: "a", botId: "b", at: 0, kind: kind, text: text)
        }
        XCTAssertTrue(entry(.workFailed, "Upgrade broke").failed)
        XCTAssertTrue(entry(.routineRan, "\"Morning brief\" failed: why").failed)
        XCTAssertFalse(entry(.routineRan, "Ran \"Morning brief\"").failed)
    }

    func testEntriesLinkToTheirWorkThenSessionThenProject() {
        var entry = BotActivity(id: "a", botId: "b", at: 0, kind: .sessionStarted, text: "x")
        let listed: (String) -> ActivityLink? = { .chat($0) }
        let unknown: (String) -> ActivityLink? = { _ in nil }
        XCTAssertEqual(ActivityLink.of(entry, chat: listed, hasProject: { _ in true }), .none)
        entry.spaceId = "s"
        XCTAssertEqual(ActivityLink.of(entry, chat: listed, hasProject: { _ in true }), .project("s"))
        XCTAssertEqual(ActivityLink.of(entry, chat: listed, hasProject: { _ in false }), .none)
        entry.chatId = "c"
        XCTAssertEqual(ActivityLink.of(entry, chat: listed, hasProject: { _ in true }), .chat("c"))
        XCTAssertEqual(ActivityLink.of(entry, chat: unknown, hasProject: { _ in true }), .project("s"))
        entry.workId = "w"
        XCTAssertEqual(ActivityLink.of(entry, chat: listed, hasProject: { _ in true }), .work("w"))

        let demo = DemoDataset.standard()
        XCTAssertEqual(demo.activityChatLink("chat-atlas-w3"), .chat("chat-atlas-w3"))
        XCTAssertEqual(demo.activityChatLink("chat-atlas"), .chat("chat-atlas"), "its own chat opens as a chat")
        XCTAssertEqual(demo.activityChatLink("chat-atlas-thread-notes"), .thread("chat-atlas-thread-notes"))
        XCTAssertNil(demo.activityChatLink("nowhere"))
    }

    func testActivityGroupsByDayNewestFirst() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let now = Int64(Date(timeIntervalSince1970: 1_794_312_000).timeIntervalSince1970 * 1000)  // Tue Nov 10 2026 12:00 UTC
        let hour: Int64 = 3_600_000
        func entry(_ id: String, _ ago: Int64) -> BotActivity {
            BotActivity(id: id, botId: "b", at: now - ago, kind: .planProposed, text: id)
        }
        let entries = [
            entry("a", hour), entry("b", 2 * hour), entry("c", 20 * hour), entry("d", 72 * hour),
            entry("e", 372 * 24 * hour),
        ]
        let days = ActivityDay.group(entries, now: now, calendar: calendar)
        XCTAssertEqual(days.map(\.title), ["Today", "Yesterday", "Sat, Nov 7", "Nov 3, 2025"])
        XCTAssertEqual(days.map { $0.entries.map(\.id) }, [["a", "b"], ["c"], ["d"], ["e"]])
        XCTAssertEqual(ActivityDay.group([], now: now, calendar: calendar), [])
    }

    func testRecentMessagesKeepTheTextOfTheLastFew() {
        func entry(_ id: String, _ role: MessageRole, _ text: String) -> MessageEntry {
            MessageEntry(
                id: id, role: role, parts: [.text(id: "t", text: text)], createdAt: 0, deviceId: "d",
                status: .complete)
        }
        let recent = recentMessages(
            [
                entry("1", .user, "first"), entry("2", .assistant, "  second\n\nline "), entry("3", .assistant, ""),
                entry("4", .user, "third"),
            ], count: 2)
        XCTAssertEqual(recent.map(\.fromWorkspace), [false, true])
        XCTAssertEqual(recent.map(\.text), ["second line", "third"])
    }
}

// The demo's workspaces, for screenshots and UI tests: Atlas, on the demo
// MacBook over the cypher and webapp projects, busy with plausible developer
// work (a plan awaiting approval, work running with a session asking,
// finished, failed and stopped work, routines, shortcuts, three connections
// and memory), and Scout, paused on the offline server. Every id is stable
// so `-route bot:bot-atlas` and the board's `-sheet board:…` specs can name
// them.
//
// Hidden chats carry their workspace markers (thread kinds as the host
// stamps them), so the session lists must leave them out; the sessions work
// started are ordinary project chats.

import Foundation

extension DemoDataset {
    static let atlasId = "bot-atlas"
    static let scoutId = "bot-scout"

    /// Workspaces, their chats and records, onto the standard dataset.
    func installWorkspaces(now: Int64) {
        let minute: Int64 = 60_000
        let hour: Int64 = 3_600_000
        let day: Int64 = 86_400_000
        let config = ChatConfig(harness: "pi", model: "demo/pi", reasoning: "high", sandbox: "workspace-write")

        bots = [
            Bot(
                id: Self.atlasId, name: "Atlas", icon: "bot-visor", color: "indigo",
                hostDeviceId: "dev-mac", chatId: "chat-atlas",
                instructions: """
                    Keep changes small and reviewable. Run the tests before you call anything done, \
                    and never push to main — open a PR instead.
                    """,
                scope: BotScope(projects: .selected(["space-cypher", "space-webapp"])),
                capabilities: .newBot, paused: false, createdAt: now - 21 * day),
            Bot(
                id: Self.scoutId, name: "Scout", icon: "bot-round", color: "teal",
                hostDeviceId: "dev-vps", chatId: "chat-scout",
                instructions: "Watch the edge deploys and tell me when something looks off.",
                scope: BotScope(projects: .selected(["space-edge"]), transcripts: false),
                capabilities: BotCapabilities(), paused: true, createdAt: now - 9 * day),
        ]

        // MARK: Chats

        func hidden(
            _ id: String, _ title: String, device: String = "dev-mac", marker: BotChatMarker,
            at: Int64, preview: String, spaceId: String? = nil
        ) -> Chat {
            Chat(
                id: id, deviceId: device, title: title, archived: false,
                cwd: spaceId == nil ? "/Users/dev/.cypher/bots/\(marker.botId)" : "/Users/dev/cypher",
                branch: nil, checkoutId: nil, config: config, lastMessagePreview: preview,
                lastMessageAt: at, createdAt: at - 10 * minute, spaceId: spaceId, lastSeenAt: at, bot: marker)
        }
        func session(
            _ id: String, _ title: String, space: String, at: Int64, created: Int64, preview: String,
            branch: String = "main", seen: Bool = true
        ) -> Chat {
            Chat(
                id: id, deviceId: "dev-mac", title: title, archived: false,
                cwd: space == "space-webapp" ? "/Users/dev/webapp" : "/Users/dev/cypher",
                branch: branch, checkoutId: nil, config: config, lastMessagePreview: preview,
                lastMessageAt: at, createdAt: created, spaceId: space, lastSeenAt: seen ? at : at - minute)
        }
        let atlas = BotChatMarker(botId: Self.atlasId)
        let atlasWork = { (workId: String) in
            BotChatMarker(botId: Self.atlasId, origin: .owner, thread: .work(workId: workId))
        }
        chats += [
            hidden(
                "chat-atlas", "Atlas", marker: atlas, at: now - 20 * minute,
                preview: "I proposed four steps: find, fix, prove it on webapp's e2e suite, then open a PR."),
            hidden(
                "chat-scout", "Scout", device: "dev-vps", marker: BotChatMarker(botId: Self.scoutId),
                at: now - 2 * day, preview: "Paused. Resume me when the edge migration is done."),
            // Orchestration threads.
            hidden(
                "chat-atlas-thread-flaky", "Stabilise the flaky sync tests", marker: atlasWork("work-flaky"),
                at: now - 3 * minute, preview: "Waiting on the webapp session's question."),
            hidden(
                "chat-atlas-thread-notes", "Draft the 0.9 release notes", marker: atlasWork("work-notes"),
                at: now - 3 * hour, preview: "Release notes drafted in docs/releases/0.9.md."),
            hidden(
                "chat-atlas-thread-loro", "Upgrade Loro to 1.6", marker: atlasWork("work-loro"),
                at: now - 26 * hour, preview: "The upgrade breaks snapshot decoding."),
            hidden(
                "chat-atlas-thread-docs", "Rewrite the onboarding guide", marker: atlasWork("work-docs"),
                at: now - 2 * day, preview: "Stopped by you."),
            // Routine threads: the workspace routine's own, and the project
            // routine's hidden coding session.
            hidden(
                "chat-atlas-routine-brief", "Morning brief",
                marker: BotChatMarker(
                    botId: Self.atlasId, origin: .routine, thread: .routine(routineId: "routine-brief")),
                at: Self.lastAt(9, 0, now: now) + 2 * minute, preview: "Nightly tests failed at 02:14."),
            hidden(
                "chat-atlas-routine-tests", "Nightly tests",
                marker: BotChatMarker(
                    botId: Self.atlasId, origin: .routine, thread: .routineSession(routineId: "routine-tests")),
                at: Self.lastAt(2, 14, now: now), preview: "2 tests failed in registry_sync.",
                spaceId: "space-cypher"),
            // A connection's questions run in a peer thread.
            hidden(
                "chat-atlas-peer-ledger", "Ledger",
                marker: BotChatMarker(
                    botId: Self.atlasId, origin: .peer, thread: .peer(connectionId: "conn-ledger")),
                at: now - 40 * minute, preview: "Drafted an answer for review."),
            // A memory consolidation pass: hidden everywhere.
            Chat(
                id: "chat-memory-pass", deviceId: "dev-mac", title: "Consolidate memory", archived: false,
                cwd: "/Users/dev/.cypher/memory", branch: nil, checkoutId: nil, config: config,
                lastMessagePreview: "Merged 3 notes into build-and-test.md.", lastMessageAt: now - 7 * hour,
                createdAt: now - 7 * hour - 4 * minute, spaceId: nil, lastSeenAt: now - 7 * hour,
                memory: MemoryChatMarker(
                    storeId: "mem-p-space-cypher", scope: .project(spaceId: "space-cypher"),
                    passId: "pass-cypher-1")),
            // Sessions Atlas's work started: ordinary project sessions.
            session(
                "chat-atlas-w1", "Find the flaky sync tests", space: "space-cypher", at: now - 15 * minute,
                created: now - 18 * minute, preview: "registry_reconnect and chat_room_catch_up flake on CI."),
            session(
                "chat-atlas-w2", "Fix the registry reconnect race", space: "space-cypher", at: now - 20_000,
                created: now - 14 * minute, preview: "Holding the cursor until the ack lands…",
                branch: "atlas/reconnect-race"),
            session(
                "chat-atlas-w3", "Re-run the webapp e2e suite", space: "space-webapp", at: now - 2 * minute,
                created: now - 9 * minute, preview: "Which staging database should the suite use?", seen: false),
            session(
                "chat-atlas-w4", "Collect merged PRs since 0.8", space: "space-cypher",
                at: now - 3 * hour - 10 * minute,
                created: now - 4 * hour, preview: "37 PRs merged since v0.8.0."),
            session(
                "chat-atlas-w5", "Bump loro to 1.6", space: "space-cypher", at: now - 26 * hour,
                created: now - 27 * hour, preview: "Snapshot decoding fails after the bump.", branch: "atlas/loro-1.6"),
        ]
        // Live rows and the host are stamped a year ahead: a negative age
        // reads as fresh (`effectiveStatus`, `deviceOnline`), so the board
        // never goes stale while someone explores it, and no timer re-renders
        // every screen to keep it so.
        let fresh = now + 365 * day
        if let mac = devices.firstIndex(where: { $0.id == "dev-mac" }) { devices[mac].lastSeenAt = fresh }
        func live(_ id: String, _ status: SessionStatus, started: Int64) {
            sessions[id] = SessionRow(
                chatId: id, deviceId: "dev-mac", status: status, startedAt: started, updatedAt: fresh)
        }
        live("chat-atlas-w2", .working, started: now - 6 * minute)
        live("chat-atlas-w3", .awaitingInput, started: now - 9 * minute)
        live("chat-atlas-thread-flaky", .working, started: now - 18 * minute)
        sessions["chat-atlas"] = SessionRow(
            chatId: "chat-atlas", deviceId: "dev-mac", status: .idle, startedAt: nil, updatedAt: now - 20 * minute)

        // MARK: Work

        botRecords.work = [
            BotWork(
                id: "work-docs", botId: Self.atlasId, title: "Rewrite the onboarding guide",
                instruction: "Rewrite docs/onboarding.md for the new device pairing flow.",
                summary: "Rewrite the guide around pairing, with screenshots from the current build.",
                steps: [
                    WorkStep(
                        id: "1", title: "Outline the new pairing flow", project: "space-cypher", state: .done,
                        startedAt: now - 2 * day - 50 * minute, endedAt: now - 2 * day - 30 * minute),
                    WorkStep(
                        id: "2", title: "Rewrite docs/onboarding.md", project: "space-cypher", state: .failed,
                        note: "Stopped", startedAt: now - 2 * day - 30 * minute, endedAt: now - 2 * day),
                ],
                state: .stopped, chatId: "chat-atlas", toolCallId: "call-plan-docs",
                threadChatId: "chat-atlas-thread-docs", createdAt: now - 2 * day - hour,
                startedAt: now - 2 * day - 50 * minute, endedAt: now - 2 * day,
                result: "Stopped before the rewrite finished. The outline is in the thread.",
                decision: PlanDecision(approve: true, at: now - 2 * day - 52 * minute),
                stopRequestedAt: now - 2 * day - minute),
            BotWork(
                id: "work-loro", botId: Self.atlasId, title: "Upgrade Loro to 1.6",
                instruction: "Upgrade loro to 1.6 everywhere and make sure snapshots still load.",
                steps: [
                    WorkStep(
                        id: "1", title: "Bump loro in Cargo.toml and fix the build", project: "space-cypher",
                        state: .failed, sessions: ["chat-atlas-w5"], note: "Snapshot decoding fails",
                        startedAt: now - 27 * hour, endedAt: now - 26 * hour),
                    WorkStep(id: "2", title: "Run the doc round-trip tests", project: "space-cypher", state: .skipped),
                ],
                state: .failed, chatId: "chat-atlas", toolCallId: "call-plan-loro",
                threadChatId: "chat-atlas-thread-loro", createdAt: now - 28 * hour,
                startedAt: now - 27 * hour, endedAt: now - 26 * hour,
                result: """
                    Failed: loro 1.6 changes the snapshot header, and `DocDisk` can't read snapshots \
                    written by 1.5. Upgrading needs a migration step first.
                    """,
                decision: PlanDecision(approve: true, at: now - 27 * hour - 2 * minute)),
            BotWork(
                id: "work-notes", botId: Self.atlasId, title: "Draft the 0.9 release notes",
                instruction: "Draft release notes for 0.9 from everything merged since 0.8.",
                summary: "Group the merged PRs by area and write user-facing notes.",
                steps: [
                    WorkStep(
                        id: "1", title: "Collect merged PRs since 0.8", project: "space-cypher", state: .done,
                        sessions: ["chat-atlas-w4"], startedAt: now - 4 * hour, endedAt: now - 3 * hour - 30 * minute),
                    WorkStep(
                        id: "2", title: "Write docs/releases/0.9.md", project: "space-cypher", state: .done,
                        startedAt: now - 3 * hour - 30 * minute, endedAt: now - 3 * hour),
                ],
                state: .done, chatId: "chat-atlas", toolCallId: "call-plan-notes",
                threadChatId: "chat-atlas-thread-notes", createdAt: now - 4 * hour - 5 * minute,
                startedAt: now - 4 * hour, endedAt: now - 3 * hour,
                result: """
                    Drafted **docs/releases/0.9.md** from 37 merged PRs.

                    - **Workspaces:** routines, shortcuts and connections
                    - **iPhone:** session fork and rewind, the workspace browser
                    - **Fixes:** 12, among them the registry reconnect loop

                    Two PRs had no description; I left them under *Other*.
                    """,
                decision: PlanDecision(approve: true, at: now - 4 * hour - 2 * minute)),
            BotWork(
                id: "work-flaky", botId: Self.atlasId, title: "Stabilise the flaky sync tests",
                instruction: "The sync tests have failed on CI three times this week. Make them reliable.",
                summary: "Find the flaky tests, fix the race behind them, and prove it on the e2e suite.",
                steps: [
                    WorkStep(
                        id: "1", title: "Find the flaky tests", project: "space-cypher", state: .done,
                        sessions: ["chat-atlas-w1"], note: "registry_reconnect, chat_room_catch_up",
                        startedAt: now - 18 * minute, endedAt: now - 15 * minute),
                    WorkStep(
                        id: "2", title: "Fix the registry reconnect race", project: "space-cypher",
                        detail: "Hold the hello cursor until the ack for the pending batch arrives.",
                        state: .running, sessions: ["chat-atlas-w2"], startedAt: now - 14 * minute),
                    WorkStep(
                        id: "3", title: "Re-run the webapp e2e suite", project: "space-webapp", state: .asking,
                        sessions: ["chat-atlas-w3"], note: "Asking which staging database to use",
                        startedAt: now - 9 * minute),
                    WorkStep(id: "4", title: "Open a PR with the fix", project: "space-cypher"),
                ],
                state: .running, chatId: "chat-atlas", toolCallId: "call-plan-flaky",
                threadChatId: "chat-atlas-thread-flaky", createdAt: now - 20 * minute,
                startedAt: now - 18 * minute, decision: PlanDecision(approve: true, at: now - 19 * minute)),
            BotWork(
                id: "work-rounding", botId: Self.atlasId, title: "Fix invoice rounding in CSV exports",
                instruction: """
                    The CSV export and the web app disagree on invoice totals again. Find out why \
                    and fix it in both places.
                    """,
                summary: "The export rounds each line; the web app rounds the total. Round once, at the total.",
                steps: [
                    WorkStep(
                        id: "1", title: "Trace rounding in the export pipeline", project: "space-cypher",
                        detail: "Read `crates/export/src/csv.rs` and find where line totals are rounded."),
                    WorkStep(
                        id: "2", title: "Round once, at the invoice total", project: "space-webapp",
                        detail: "Change `lib/invoice/total.ts` to sum unrounded lines, then round half-even."),
                    WorkStep(
                        id: "3", title: "Add a regression test for half-cent totals", project: "space-webapp"),
                ],
                state: .proposed, chatId: "chat-atlas", toolCallId: "call-plan-rounding",
                createdAt: now - 22 * minute),
        ]

        botRecords.approvals = [
            BotApproval(
                id: "approval-staging", botId: Self.atlasId, threadChatId: "chat-atlas-thread-flaky",
                workId: "work-flaky", action: "Start a session in webapp: run the e2e suite against staging",
                reason: "This thread read an answer from Harbor (Sam Ortiz's workspace).",
                createdAt: now - 4 * minute),
            BotApproval(
                id: "approval-old", botId: Self.atlasId, threadChatId: "chat-atlas-thread-loro",
                workId: "work-loro", action: "Answer the cypher session's question: \"Keep the 1.5 snapshots?\"",
                reason: "The session was not started by Atlas.", createdAt: now - 27 * hour,
                state: .approved, decision: PlanDecision(approve: true, at: now - 27 * hour + minute)),
        ]

        // MARK: Routines

        botRecords.routines = [
            BotRoutine(
                id: "routine-brief", botId: Self.atlasId, name: "Morning brief",
                prompt: """
                    Summarise what changed in my projects since yesterday: merged PRs, failing CI, \
                    sessions waiting on me. Lead with what needs me.
                    """,
                schedule: .daily(time: "09:00", days: [1, 2, 3, 4, 5]), writeNotes: true,
                createdAt: now - 20 * day, updatedAt: now - 6 * day,
                threadChatId: "chat-atlas-routine-brief",
                lastRun: RunRecord(
                    at: Self.lastAt(9, 0, now: now), endedAt: Self.lastAt(9, 0, now: now) + 2 * minute, outcome: .done,
                    summary: "Nightly tests failed, CI flaked twice on the sync tests, release notes drafted.",
                    chatId: "chat-atlas-routine-brief"),
                nextRunAt: Self.nextAt(9, 0, weekdaysOnly: true, now: now)),
            BotRoutine(
                id: "routine-tests", botId: Self.atlasId, name: "Nightly tests",
                prompt: "Run the full test suite on main. If anything fails, find the cause and write it up.",
                target: .project(spaceId: "space-cypher", worktree: true, model: nil),
                schedule: .daily(time: "02:00", days: []), workDirectly: true,
                createdAt: now - 14 * day, updatedAt: now - 14 * day,
                threadChatId: "chat-atlas-routine-tests",
                worktreePath: "/Users/dev/.cypher/worktrees/cypher-nightly",
                lastRun: RunRecord(
                    at: Self.lastAt(2, 0, now: now), endedAt: Self.lastAt(2, 0, now: now) + 14 * minute,
                    outcome: .failed,
                    summary: "2 tests failed in registry_sync: reconnect_keeps_cursor, ack_before_rows.",
                    chatId: "chat-atlas-routine-tests"),
                nextRunAt: Self.nextAt(2, 0, weekdaysOnly: false, now: now)),
            BotRoutine(
                id: "routine-triage", botId: Self.atlasId, name: "Triage new issues",
                prompt: "Read the issues opened this week, label them, and suggest an owner for each.",
                schedule: .manual, createdAt: now - 5 * day, updatedAt: now - 5 * day),
            BotRoutine(
                id: "routine-release", botId: Self.atlasId, name: "Cut the 0.9 release",
                prompt: "Remind me to cut the 0.9 release, with the checklist from docs/releasing.md.",
                schedule: .once(at: Self.onceSpec(daysAhead: 2, hour: 10, now: now)),
                createdAt: now - day, updatedAt: now - day,
                nextRunAt: Self.todayAt(10, 0, now: now) + 2 * day),
            // Switched off: it only runs when asked.
            BotRoutine(
                id: "routine-deps", botId: Self.atlasId, name: "Audit dependencies",
                prompt: "Check every project for outdated or vulnerable dependencies and list what to upgrade.",
                schedule: .daily(time: "08:00", days: [1]), enabled: false, workDirectly: true,
                createdAt: now - 12 * day, updatedAt: now - 3 * day,
                lastRun: RunRecord(
                    at: now - 8 * day, endedAt: now - 8 * day + 6 * minute, outcome: .done,
                    summary: "3 upgrades: `tokio` 1.41, `serde_json` 1.0.133, `loro` 1.6 (breaking).")),
            BotRoutine(
                id: "routine-scout-logs", botId: Self.scoutId, name: "Check edge deploy logs",
                prompt: "Read the last two hours of edge deploy logs and flag errors.",
                schedule: .interval(minutes: 120), createdAt: now - 8 * day, updatedAt: now - 8 * day,
                lastRun: RunRecord(
                    at: now - 2 * day, outcome: .skipped, summary: "Skipped: Scout is paused."),
                nextRunAt: nil),
        ]

        // MARK: Shortcuts

        botRecords.shortcuts = [
            BotShortcut(
                id: "shortcut-status", botId: Self.atlasId, label: "What's running?", icon: "checklist",
                request: "What's running in my projects right now, and what needs me?", order: 0,
                createdAt: now - 10 * day, updatedAt: now - 10 * day),
            BotShortcut(
                id: "shortcut-fix-tests", botId: Self.atlasId, label: "Fix tests", icon: "test-tube",
                request: "Start a session in cypher to fix the failing tests. {input}", order: 1,
                createdAt: now - 9 * day, updatedAt: now - 9 * day),
            BotShortcut(
                id: "shortcut-notes", botId: Self.atlasId, label: "Release notes", icon: "document",
                request: "Draft release notes from the PRs merged since the last tag.", order: 2,
                createdAt: now - 6 * day, updatedAt: now - 6 * day),
            BotShortcut(
                id: "shortcut-prs", botId: Self.atlasId, label: "Review PRs", icon: "code",
                request: "Summarise the open pull requests in webapp and flag the risky ones.", order: 3,
                createdAt: now - 2 * day, updatedAt: now - 2 * day),
        ]

        // MARK: Connections

        botRecords.connections = [
            BotConnection(
                id: "conn-ledger", botId: Self.atlasId,
                peer: PeerBot(
                    roomId: "room-ledger", name: "Ledger", ownerName: "Dana Whitfield",
                    ownerAvatarUrl: "https://avatars.githubusercontent.com/u/583231?v=4"),
                createdAt: now - 12 * day, replyMode: .review, threadChatId: "chat-atlas-peer-ledger",
                lastExchangeAt: now - 40 * minute),
            BotConnection(
                id: "conn-harbor", botId: Self.atlasId,
                peer: PeerBot(roomId: "room-harbor", name: "Harbor", ownerName: "Sam Ortiz"),
                createdAt: now - 8 * day, replyMode: .auto,
                scope: BotScope(projects: .selected(["space-webapp"]), transcripts: false),
                lastExchangeAt: now - 5 * hour),
            BotConnection(
                id: "conn-northwind", botId: Self.atlasId,
                peer: PeerBot(roomId: "room-northwind", name: "Northwind", ownerName: "Priya Raman"),
                createdAt: now - 4 * day, replyMode: .off, lastExchangeAt: now - day),
            BotConnection(
                id: "conn-orbit", botId: Self.atlasId,
                peer: PeerBot(roomId: "room-orbit", name: "Orbit", ownerName: "Lee Chen"),
                createdAt: now - 30 * day, replyMode: .auto, revokeRequestedAt: now - 15 * day,
                revokedAt: now - 15 * day),
        ]
        botRecords.invites = [
            BotInvite(
                id: "invite-open", botId: Self.atlasId, createdAt: now - 2 * hour,
                expiresAt: now - 2 * hour + 7 * day, uses: 0, maxUses: 1),
            BotInvite(
                id: "invite-used", botId: Self.atlasId, createdAt: now - 8 * day - hour,
                expiresAt: now - day - hour, uses: 1, maxUses: 1),
        ]
        botRecords.exchanges = [
            BotExchange(
                id: "ex-review", botId: Self.atlasId, connectionId: "conn-ledger", direction: .incoming,
                question:
                    "Does the webapp still export invoices as CSV with per-line tax? We're matching our importer to it.",
                answer: """
                    Yes. Each line carries its own tax column (`tax_amount`, two decimals), and the \
                    invoice total is rounded once at the end. A fix that changes how totals round is \
                    awaiting approval, so expect totals to shift by a cent on some invoices.
                    """,
                state: .awaitingReview, askedAt: now - 40 * minute),
            BotExchange(
                id: "ex-working", botId: Self.atlasId, connectionId: "conn-ledger", direction: .outgoing,
                question: "Which rounding mode does Ledger use for EU invoice totals?",
                state: .working, askedAt: now - 12 * minute),
            BotExchange(
                id: "ex-harbor-out", botId: Self.atlasId, connectionId: "conn-harbor", direction: .outgoing,
                question: "Is the billing API on staging running 2.14 yet?",
                answer: "Yes, since this morning's deploy (2.14.1). Staging's database was reset at the same time.",
                state: .answered, askedAt: now - 5 * hour - 10 * minute, answeredAt: now - 5 * hour),
            BotExchange(
                id: "ex-harbor-in", botId: Self.atlasId, connectionId: "conn-harbor", direction: .incoming,
                question: "Does webapp pin the billing API client to a version?",
                answer: "It pins `@acme/billing-client` to ^2.13 in package.json, so 2.14 is picked up.",
                state: .answered, askedAt: now - 2 * day, answeredAt: now - 2 * day + 3 * minute),
            BotExchange(
                id: "ex-northwind", botId: Self.atlasId, connectionId: "conn-northwind", direction: .incoming,
                question: "Can you share the deploy settings for webapp's production environment?",
                state: .declined, askedAt: now - day, answeredAt: now - day),
        ]

        // MARK: Activity

        var log: [(Int64, ActivityKind, String, String?, String?, String?)] = [
            (
                22 * minute, .planProposed, "Proposed “Fix invoice rounding in CSV exports”", "chat-atlas",
                "work-rounding", nil
            ),
            (
                4 * minute, .approvalAsked, "Asked to start a session in webapp", "chat-atlas-thread-flaky",
                "work-flaky", "space-webapp"
            ),
            (
                9 * minute, .sessionStarted, "Started “Re-run the webapp e2e suite” in webapp", "chat-atlas-w3",
                "work-flaky", "space-webapp"
            ),
            (12 * minute, .peerAsked, "Asked Ledger which rounding mode EU invoices use", nil, nil, nil),
            (
                14 * minute, .sessionStarted, "Started “Fix the registry reconnect race” in cypher", "chat-atlas-w2",
                "work-flaky", "space-cypher"
            ),
            (
                14 * minute + 30_000, .worktreeCreated, "Made a worktree for atlas/reconnect-race in cypher", nil,
                "work-flaky", "space-cypher"
            ),
            (
                18 * minute, .sessionStarted, "Started “Find the flaky sync tests” in cypher", "chat-atlas-w1",
                "work-flaky", "space-cypher"
            ),
            (19 * minute, .planApproved, "Approved “Stabilise the flaky sync tests”", nil, "work-flaky", nil),
            (
                40 * minute, .peerAnswered, "Drafted an answer to Ledger for your review", "chat-atlas-peer-ledger",
                nil, nil
            ),
            (
                3 * hour, .workFinished, "Finished “Draft the 0.9 release notes”", "chat-atlas-thread-notes",
                "work-notes", nil
            ),
            (3 * hour + 5 * minute, .noteWritten, "Saved release conventions to memory", nil, nil, nil),
            (5 * hour, .peerAsked, "Asked Harbor whether staging runs billing 2.14", nil, nil, nil),
            (7 * hour, .noteWritten, "Consolidated cypher's memory: 3 notes merged", nil, nil, "space-cypher"),
            (
                now - Self.lastAt(9, 0, now: now) - 2 * minute, .routineRan, "Ran Morning brief",
                "chat-atlas-routine-brief", nil, nil
            ),
            (
                now - Self.lastAt(2, 0, now: now) - 14 * minute, .routineRan,
                "Nightly tests failed: 2 tests in registry_sync", "chat-atlas-routine-tests", nil, "space-cypher"
            ),
            (
                26 * hour, .workFailed, "“Upgrade Loro to 1.6” failed: snapshot decoding", "chat-atlas-thread-loro",
                "work-loro", nil
            ),
            (
                27 * hour - minute, .sessionAnswered, "Answered the cypher session about 1.5 snapshots",
                "chat-atlas-w5", "work-loro", "space-cypher"
            ),
            (day, .peerAnswered, "Declined Northwind's question (replies are off)", nil, nil, nil),
            (
                2 * day, .workStopped, "Stopped “Rewrite the onboarding guide”", "chat-atlas-thread-docs", "work-docs",
                nil
            ),
            (2 * day, .peerAnswered, "Answered Harbor about the billing client pin", nil, nil, nil),
            (4 * day, .connectionAdded, "Connected to Northwind (Priya Raman)", nil, nil, nil),
            (15 * day, .connectionRevoked, "Disconnected from Orbit (Lee Chen)", nil, nil, nil),
        ]
        log.sort { $0.0 < $1.0 }
        botRecords.activity = log.enumerated().map { ix, entry in
            BotActivity(
                id: "act-\(ix)", botId: Self.atlasId, at: now - entry.0, kind: entry.1, text: entry.2,
                chatId: entry.3, workId: entry.4, spaceId: entry.5)
        }
        botRecords.activity.append(
            BotActivity(
                id: "act-scout-1", botId: Self.scoutId, at: now - 2 * day, kind: .routineRan,
                text: "Skipped Check edge deploy logs: paused"))

        installMemory(now: now)
    }

    // MARK: Memory

    private func installMemory(now: Int64) {
        let hour: Int64 = 3_600_000
        let day: Int64 = 86_400_000
        let atlasThread = MemoryAuthor.workspace(botId: Self.atlasId, chatId: "chat-atlas", title: "Atlas")
        let pass = MemoryPass(
            id: "pass-cypher-1", at: now - 7 * hour - 4 * 60_000, endedAt: now - 7 * hour,
            outcome: .done, summary: "Merged 3 notes into build-and-test.md and dropped one stale fact.",
            changes: 2, tokens: 18_400, chatId: "chat-memory-pass", sources: 6, undo: "0a1b2c")

        func view(
            _ id: String, _ scope: MemoryScope, files: [MemoryFile], pending: [MemoryPending] = [],
            changes: [MemoryChange] = [], lastPass: MemoryPass? = nil, held: [HeldWrite] = [],
            decided: [HeldDecision] = []
        ) -> MemoryView {
            let sorted = files.sorted { a, b in
                if (a.path == MemoryLimits.index) != (b.path == MemoryLimits.index) {
                    return a.path == MemoryLimits.index
                }
                return a.path < b.path
            }
            let store = MemoryStore(
                id: id, scope: scope, hostDeviceId: "dev-mac", files: files.count,
                bytes: Int64(files.reduce(0) { $0 + $1.text.utf8.count }), pending: pending.count,
                preview: clip(sorted.first?.text ?? "", max: 1_200), createdAt: now - 30 * day,
                updatedAt: changes.first?.at ?? now - day, lastPass: lastPass,
                month: MonthUsage(month: Self.monthKey(now), tokens: 61_200, passes: 4),
                held: held, decided: decided)
            return MemoryView(store: store, files: sorted, pending: pending, changes: changes)
        }

        let you = view(
            "mem-u-demo", .you,
            files: [
                MemoryFile(
                    path: "/MEMORY.md",
                    text: """
                        # About me
                        - I work mostly in Swift and Rust; explain TypeScript changes in more detail.
                        - Prefer small PRs with a short why in the description.
                        - See [style.md](style.md) for how I like code reviews written.
                        """,
                    createdAt: now - 30 * day, updatedAt: now - 3 * day),
                MemoryFile(
                    path: "/style.md",
                    text: """
                        # Review style
                        - Lead with the one thing that must change.
                        - Quote the line, then say why.
                        """,
                    author: .session(chatId: "chat-tabs", title: "Tool group header colors"),
                    createdAt: now - 12 * day, updatedAt: now - 12 * day),
            ],
            changes: [
                MemoryChange(
                    id: "you-1", at: now - 3 * day, author: .owner, path: "/MEMORY.md", action: .edit,
                    summary: "Added 1 line", before: "# About me\n- I work mostly in Swift and Rust.", ownerText: true,
                    undo: "v-you-1")
            ],
            lastPass: MemoryPass(
                id: "pass-you-1", at: now - day - 3 * hour, endedAt: now - day - 3 * hour + 120_000, outcome: .skipped,
                summary: "Nothing new since the last pass."))

        let cypher = view(
            "mem-p-space-cypher", .project(spaceId: "space-cypher"),
            files: [
                MemoryFile(
                    path: "/MEMORY.md",
                    text: """
                        # cypher
                        - Build: `cargo build --workspace`; iOS: see [build-and-test.md](build-and-test.md).
                        - Registry rows are LWW per field; never write a field you don't own.
                        - Decisions live in [decisions/](decisions/rounding.md).
                        """,
                    author: .consolidation(passId: "pass-cypher-1"), createdAt: now - 25 * day,
                    updatedAt: now - 7 * hour),
                MemoryFile(
                    path: "/build-and-test.md",
                    text: """
                        # Build and test
                        - `scripts/check.sh rust` before pushing.
                        - iOS unit tests: `scripts/ci/ios-unit-tests.sh` with `IOS_SIMULATOR` set.
                        - The sync tests need `--test-threads=1` locally.
                        """,
                    author: .consolidation(passId: "pass-cypher-1"), createdAt: now - 20 * day,
                    updatedAt: now - 7 * hour),
                MemoryFile(
                    path: "/decisions/rounding.md",
                    text: "# Rounding\nRound money once, at the total, half-even.",
                    author: atlasThread, createdAt: now - 5 * day, updatedAt: now - 5 * day),
                MemoryFile(
                    path: "/release.md",
                    text: "# Releasing\n- Tag from main only.\n- Release notes go in docs/releases/<version>.md.",
                    author: .workspace(
                        botId: Self.atlasId, chatId: "chat-atlas-thread-notes", title: "Draft the 0.9 release notes"),
                    createdAt: now - 3 * hour, updatedAt: now - 3 * hour),
            ],
            pending: [
                MemoryPending(
                    id: "pending-cypher-1", at: now - 50 * 60_000,
                    author: .session(chatId: "chat-deploy", title: "Wrangler deploy hygiene"),
                    path: "/deploy.md", command: "create",
                    args: .object(["path": .string("/project/deploy.md")]),
                    reason: "The session fetched a web page.",
                    preview: "# Deploying\n- `wrangler deploy --env production` from apps/edge.")
            ],
            changes: [
                MemoryChange(
                    id: "cypher-3", at: now - 3 * hour,
                    author: .workspace(
                        botId: Self.atlasId, chatId: "chat-atlas-thread-notes", title: "Draft the 0.9 release notes"),
                    path: "/release.md", action: .create, summary: "Created the file", undo: "v-cypher-3"),
                MemoryChange(
                    id: "cypher-2", at: now - 7 * hour, author: .consolidation(passId: "pass-cypher-1"),
                    path: "/build-and-test.md", action: .edit, summary: "Merged 3 notes",
                    before: "# Build and test\n- `scripts/check.sh rust` before pushing.", undo: "v-cypher-2"),
                MemoryChange(
                    id: "cypher-1", at: now - 7 * hour, author: .consolidation(passId: "pass-cypher-1"),
                    path: "/notes.md", action: .delete, summary: "Removed the file",
                    before: "- The sync tests need --test-threads=1"),
            ],
            lastPass: pass)

        let atlas = view(
            "mem-w-\(Self.atlasId)", .workspace(botId: Self.atlasId),
            files: [
                MemoryFile(
                    path: "/MEMORY.md",
                    text: """
                        # Atlas
                        - The owner reviews plans on their phone: keep plan titles short.
                        - webapp's e2e suite runs against staging; ask before touching its database.
                        - Ledger (Dana) imports our CSV exports.
                        """,
                    author: atlasThread, createdAt: now - 20 * day, updatedAt: now - day),
                MemoryFile(
                    path: "/people.md",
                    text: "# People\n- Dana Whitfield owns Ledger.\n- Sam Ortiz runs staging (Harbor).",
                    author: atlasThread, createdAt: now - 12 * day, updatedAt: now - 8 * day),
                MemoryFile(
                    path: "/invoices.md",
                    text: "# Invoices\n- Totals must match between the CSV export and the web app.",
                    author: atlasThread, createdAt: now - 22 * 60_000, updatedAt: now - 22 * 60_000),
            ],
            pending: [
                MemoryPending(
                    id: "held-ledger", at: now - 21 * 60_000, author: atlasThread, path: "/invoices.md",
                    command: "str_replace",
                    args: .object([
                        "path": .string("/workspace/invoices.md"),
                        "new_str": .string("- Ledger rounds EU totals half-up."),
                    ]),
                    reason: "This run read an answer from Ledger.",
                    preview: """
                        # Invoices
                        - Totals must match between the CSV export and the web app.
                        - Ledger rounds EU totals half-up.
                        """,
                    toolCallId: "call-memory-held")
            ],
            changes: [
                MemoryChange(
                    id: "atlas-2", at: now - 22 * 60_000, author: atlasThread, path: "/invoices.md",
                    action: .create, summary: "Created the file", undo: "v-atlas-2"),
                MemoryChange(
                    id: "atlas-1", at: now - day, author: atlasThread, path: "/MEMORY.md", action: .edit,
                    summary: "Added 1 line", before: "# Atlas\n- The owner reviews plans on their phone.", undo: "v-atlas-1"),
            ],
            held: [
                HeldWrite(
                    id: "held-ledger", chatId: "chat-atlas", toolCallId: "call-memory-held", path: "/invoices.md",
                    preview: "- Ledger rounds EU totals half-up.", reason: "This run read an answer from Ledger.",
                    at: now - 21 * 60_000)
            ],
            decided: [
                HeldDecision(id: "held-invoices", toolCallId: "call-memory-saved", saved: true, at: now - 22 * 60_000)
            ])

        for memory in [you, cypher, atlas] {
            setMemoryView(memory)
        }
    }

    // MARK: Transcripts

    /// A workspace chat's or thread's transcript, nil for any other chat.
    static func workspaceTranscript(for chatId: String, now: Int64) -> [MessageEntry]? {
        let minute: Int64 = 60_000
        let hour: Int64 = 3_600_000
        func user(_ id: String, _ text: String, at: Int64) -> MessageEntry {
            MessageEntry(
                id: id, role: .user, parts: [.text(id: "\(id)-t", text: text)], createdAt: at,
                deviceId: "ios-demo", status: .complete)
        }
        func reply(_ id: String, _ parts: [MessagePart], at: Int64) -> MessageEntry {
            MessageEntry(id: id, role: .assistant, parts: parts, createdAt: at, deviceId: "dev-mac", status: .complete)
        }
        func tool(_ id: String, _ name: String, isError: Bool = false) -> MessagePart {
            // A workspace tool as the doc keeps it: an unknown call by name.
            .tool(
                id: id, call: RenderToolCall(tag: "unknown", fields: ["name": name]), isError: isError, resolved: true)
        }
        switch chatId {
        case "chat-atlas":
            let briefAt = lastAt(9, 0, now: now)
            return [
                MessageEntry(
                    id: "atlas-post-brief", role: .assistant,
                    parts: [
                        .post(
                            id: "post-brief",
                            text: """
                                **Good morning. Three things from overnight**

                                1. Nightly tests failed at 02:14: `reconnect_keeps_cursor` and `ack_before_rows` \
                                in registry_sync.
                                2. CI on main failed twice yesterday, both times in the sync tests.
                                3. The 0.9 release notes are drafted in `docs/releases/0.9.md`.
                                """,
                            from: .routine(
                                routineId: "routine-brief", name: "Morning brief", outcome: .done, runAt: briefAt,
                                threadChatId: "chat-atlas-routine-brief"))
                    ], createdAt: briefAt + 2 * minute, deviceId: "dev-mac", status: .complete),
                user(
                    "atlas-u2",
                    "The CSV export and the web app disagree on invoice totals again. Find out why and propose a fix.",
                    at: now - 26 * minute),
                reply(
                    "atlas-a2",
                    [
                        tool("call-search-1", "search_sessions"),
                        tool("call-read-2", "read_session"),
                        tool("call-git-1", "project_git"),
                        .text(
                            id: "t1",
                            text: """
                                Found it. The export rounds **each line** before summing, while the web app \
                                sums unrounded lines and rounds the total. On invoices with many half-cent \
                                lines the two drift by a cent or two.
                                """),
                        tool("call-memory-saved", "memory"),
                        tool("call-plan-rounding", "propose_plan"),
                        tool("call-memory-held", "memory"),
                        .text(
                            id: "t2",
                            text: """
                                The plan rounds once, at the total, in both places, and adds a regression test. \
                                Nothing runs until you approve.
                                """),
                    ], at: now - 22 * minute),
                user(
                    "atlas-u1", "The sync tests have failed on CI three times this week. Can you make them reliable?",
                    at: now - 21 * minute),
                reply(
                    "atlas-a1",
                    [
                        .text(id: "t1", text: "Let me look at the recent CI sessions first."),
                        tool("call-list-1", "list_sessions"),
                        tool("call-read-1", "read_session"),
                        tool("call-plan-flaky", "propose_plan"),
                        .text(
                            id: "t2",
                            text: """
                                Two tests flake: `registry_reconnect` and `chat_room_catch_up`. Both race the \
                                hello cursor against a pending ack. I proposed four steps: find, fix, prove it on \
                                webapp's e2e suite, then open a PR.
                                """),
                    ], at: now - 20 * minute),
            ]
        case "chat-scout":
            return [
                user("scout-u1", "Watch the edge deploys and tell me if anything breaks.", at: now - 9 * 86_400_000),
                reply(
                    "scout-a1",
                    [
                        tool("call-scout-list", "list_projects"),
                        .text(
                            id: "t1",
                            text: "I can see **edge** on hetzner-01. I'll check its deploy logs every two hours."),
                    ], at: now - 9 * 86_400_000 + minute),
            ]
        case "chat-atlas-thread-flaky":
            return [
                user(
                    "flaky-u1",
                    "Brief: the sync tests have failed on CI three times this week. Make them reliable.",
                    at: now - 18 * minute),
                reply(
                    "flaky-a1",
                    [
                        tool("call-flaky-start-1", "start_session"),
                        tool("call-flaky-wait-1", "wait_for_session"),
                        tool("call-flaky-step-1", "update_step"),
                        tool("call-flaky-tree", "create_worktree"),
                        tool("call-flaky-start-2", "start_session"),
                        tool("call-flaky-start-3", "start_session"),
                        .text(
                            id: "t1",
                            text: """
                                Step 1 found the flaky pair. The fix is under way in a worktree, and the \
                                webapp session is waiting on an answer about staging.
                                """),
                    ], at: now - 3 * minute),
            ]
        case "chat-atlas-thread-notes", "chat-atlas-thread-loro", "chat-atlas-thread-docs":
            let (brief, outcome): (String, String) =
                switch chatId {
                case "chat-atlas-thread-notes":
                    ("Draft release notes for 0.9 from everything merged since 0.8.", "Drafted docs/releases/0.9.md.")
                case "chat-atlas-thread-loro":
                    ("Upgrade loro to 1.6 everywhere.", "The upgrade breaks snapshot decoding; stopping here.")
                default:
                    ("Rewrite docs/onboarding.md for the new pairing flow.", "Stopped by the owner.")
                }
            return [
                user("\(chatId)-u", "Brief: \(brief)", at: now - 4 * hour),
                reply(
                    "\(chatId)-a",
                    [
                        tool("\(chatId)-start", "start_session"),
                        tool("\(chatId)-wait", "wait_for_session"),
                        tool("\(chatId)-finish", "finish_work"),
                        .text(id: "t1", text: outcome),
                    ], at: now - 3 * hour),
            ]
        case "chat-atlas-routine-brief":
            let at = lastAt(9, 0, now: now)
            return [
                user("brief-u", "Summarise what changed in my projects since yesterday.", at: at),
                reply(
                    "brief-a",
                    [
                        tool("brief-list", "list_sessions"),
                        tool("brief-activity", "list_activity"),
                        .text(id: "t1", text: "Nightly tests failed, CI flaked twice, and the 0.9 notes are drafted."),
                    ], at: at + 2 * minute),
            ]
        case "chat-atlas-routine-tests":
            let at = lastAt(2, 0, now: now)
            return [
                user("tests-u", "Run the full test suite on main. If anything fails, find the cause.", at: at),
                reply(
                    "tests-a",
                    [
                        .tool(
                            id: "tests-exec",
                            call: RenderToolCall(tag: "exec", fields: ["command": "cargo test --workspace"]),
                            isError: true, resolved: true),
                        .text(
                            id: "t1",
                            text: "2 tests failed in `registry_sync`: `reconnect_keeps_cursor` and `ack_before_rows`."),
                    ], at: at + 14 * minute),
            ]
        case "chat-atlas-peer-ledger":
            return [
                user(
                    "ledger-u",
                    "Ledger asks: Does the webapp still export invoices as CSV with per-line tax?",
                    at: now - 41 * minute),
                reply(
                    "ledger-a",
                    [
                        tool("ledger-read", "read_session"),
                        .text(id: "t1", text: "Drafted an answer; it waits for the owner's review."),
                    ], at: now - 40 * minute),
            ]
        case "chat-atlas-w1":
            return [
                user("w1-u", "Find which sync tests flake on CI and why.", at: now - 18 * minute),
                reply(
                    "w1-a",
                    [
                        .text(
                            id: "t1",
                            text: "`registry_reconnect` and `chat_room_catch_up` flake: both race the hello cursor.")
                    ],
                    at: now - 15 * minute),
            ]
        case "chat-atlas-w2":
            return [
                user(
                    "w2-u", "Fix the registry reconnect race: hold the cursor until the pending ack lands.",
                    at: now - 14 * minute),
                MessageEntry(
                    id: "w2-a", role: .assistant,
                    parts: [
                        .tool(
                            id: "w2-read",
                            call: RenderToolCall(
                                tag: "readFile", fields: ["path": "apps/ios/Cypher/Sync/RegistryClient.swift"]),
                            isError: false, resolved: true),
                        .text(id: "t1", text: "Holding the cursor until the ack lands…"),
                    ], createdAt: now - 20_000, deviceId: "dev-mac", status: .streaming),
            ]
        case "chat-atlas-w3":
            let question = "Which staging database should the e2e suite use?"
            return [
                user("w3-u", "Re-run the webapp e2e suite against staging and report failures.", at: now - 9 * minute),
                reply(
                    "w3-a",
                    [
                        .text(id: "t1", text: "Staging's database was reset this morning. Before I run the suite:"),
                        .input(
                            id: "w3-ask", requestId: "w3-ask",
                            questions: [
                                UserInputQuestion(
                                    id: "q1", header: "Staging database", question: question,
                                    options: ["The fresh staging database", "A copy of yesterday's snapshot"],
                                    multiSelect: false)
                            ], resolved: false),
                    ], at: now - 2 * minute),
            ]
        default:
            return nil
        }
    }

    // MARK: Times

    /// Today at `hour:minute`, local time, as epoch ms.
    static func todayAt(_ hour: Int, _ minute: Int, now: Int64) -> Int64 {
        let date = Date(timeIntervalSince1970: TimeInterval(now) / 1000)
        let at = Calendar.current.date(bySettingHour: hour, minute: minute, second: 0, of: date) ?? date
        return Int64(at.timeIntervalSince1970 * 1000)
    }

    /// The latest `hour:minute` at or before now: today's, or yesterday's.
    static func lastAt(_ hour: Int, _ minute: Int, now: Int64) -> Int64 {
        let today = todayAt(hour, minute, now: now)
        return today <= now ? today : today - 86_400_000
    }

    /// The next `hour:minute` after now, on a weekday when asked.
    static func nextAt(_ hour: Int, _ minute: Int, weekdaysOnly: Bool, now: Int64) -> Int64 {
        let calendar = Calendar.current
        var at = todayAt(hour, minute, now: now)
        for _ in 0..<8 {
            let date = Date(timeIntervalSince1970: TimeInterval(at) / 1000)
            let weekday = calendar.component(.weekday, from: date)  // 1 = Sunday
            if at > now && (!weekdaysOnly || (2...6).contains(weekday)) { return at }
            at += 86_400_000
        }
        return at
    }

    /// A once-only schedule's "YYYY-MM-DDTHH:MM", `daysAhead` from today.
    static func onceSpec(daysAhead: Int, hour: Int, now: Int64) -> String {
        let at = todayAt(hour, 0, now: now) + Int64(daysAhead) * 86_400_000
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd'T'HH:mm"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(at) / 1000))
    }

    static func monthKey(_ now: Int64) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM"
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(now) / 1000))
    }
}

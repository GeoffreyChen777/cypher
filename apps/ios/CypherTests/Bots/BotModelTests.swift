// Workspace models against the engine's serde JSON (crates/proto/src/bot.rs,
// bot_records.rs, bot_wire.rs, memory.rs): fixtures in the exact Rust
// shapes, missing fields taking the Rust defaults, unknown enum strings
// reading as the narrowest case, and encodings the engine accepts.

import XCTest
@testable import Cypher

/// `json` decoded as `T`.
func decodeFixture<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

/// `value` encoded, then read back as a JSON value (key order free).
func encodedJSON<T: Encodable>(_ value: T) throws -> JSONValue {
    try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value))
}

func jsonFixture(_ json: String) throws -> JSONValue {
    try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))
}

final class BotModelTests: XCTestCase {
    func testBotRowsDecodeFromTheRegistryAndFromCreateBot() throws {
        // registry.rs `upsert_bot`: createdAt is epoch ms.
        let row = try decodeFixture(
            Bot.self,
            #"""
            {"id":"b1","name":"Atlas","icon":"bot-visor","color":"indigo","hostDeviceId":"mac","chatId":"c1",
             "scope":{"projects":{"mode":"selected","spaceIds":["s1"]},"includeArchived":false,"transcripts":true},
             "capabilities":{"workDirectly":true,"coordinate":true,"steer":false},"paused":false,
             "createdAt":1762761600000}
            """#)
        XCTAssertEqual(row.createdAt, 1_762_761_600_000)
        XCTAssertEqual(row.scope.projects, .selected(["s1"]))
        XCTAssertTrue(row.scope.seesSpace("s1"))
        XCTAssertFalse(row.scope.seesSpace("s2"))
        XCTAssertEqual(row.capabilities, .newBot)
        // CreateBot's reply is the proto `Bot`: chrono's RFC 3339 time.
        let reply = try decodeFixture(
            Bot.self,
            #"{"id":"b2","name":"Scout","hostDeviceId":"vps","chatId":"c2","createdAt":"2026-11-10T08:00:00.123456789Z"}"#
        )
        XCTAssertEqual(reply.createdAt, 1_794_297_600_123)
        XCTAssertEqual(reply.scope, BotScope(), "a missing scope sees nothing")
        XCTAssertEqual(reply.capabilities, BotCapabilities(), "rows from before capabilities are read-only")
        XCTAssertFalse(reply.paused)
        XCTAssertNil(reply.icon)
    }

    func testAnUnreadableScopeReadsAsTheEmptyScope() throws {
        // rows.rs RawBot: a scope from a newer app sees nothing, never more.
        let bot = try decodeFixture(
            Bot.self,
            #"""
            {"id":"b","name":"N","hostDeviceId":"d","chatId":"c","createdAt":1,
             "scope":{"projects":{"mode":"everythingNew"}},"capabilities":"garbage"}
            """#)
        XCTAssertEqual(bot.scope, BotScope())
        XCTAssertEqual(bot.capabilities, BotCapabilities())
    }

    func testScopeDefaultsAndWireShape() throws {
        let empty = try decodeFixture(BotScope.self, "{}")
        XCTAssertEqual(empty, BotScope(), "serde and Swift defaults agree")
        XCTAssertTrue(empty.transcripts)
        XCTAssertEqual(empty.projects, .noProjects)
        XCTAssertEqual(
            try encodedJSON(BotScope(projects: .selected(["s1"]))),
            try jsonFixture(
                #"{"projects":{"mode":"selected","spaceIds":["s1"]},"includeArchived":false,"transcripts":true}"#),
            "empty session lists stay off the wire")
        let full = BotScope(
            projects: .all, includeSessions: ["a"], excludeSessions: ["b"], includeArchived: true, transcripts: false)
        XCTAssertEqual(
            try decodeFixture(BotScope.self, String(decoding: JSONEncoder().encode(full), as: UTF8.self)), full)
        XCTAssertEqual(
            try encodedJSON(BotScope()),
            try jsonFixture(
                #"{"projects":{"mode":"none"},"includeArchived":false,"transcripts":true}"#))
        XCTAssertNotNil(BotScope(includeSessions: Array(repeating: "x", count: 2_001)).validationError)
    }

    func testChatMarkersReadOldRowsAsOwnerAndUnknownOriginsAsPeer() throws {
        let old = try decodeFixture(BotChatMarker.self, #"{"botId":"b1"}"#)
        XCTAssertEqual(old, BotChatMarker(botId: "b1", origin: .owner, thread: .main))
        XCTAssertTrue(old.isMain)
        XCTAssertEqual(try encodedJSON(old), try jsonFixture(#"{"botId":"b1","origin":"owner"}"#))
        let future = try decodeFixture(BotChatMarker.self, #"{"botId":"b1","origin":"auditor"}"#)
        XCTAssertEqual(future.origin, .peer)
        let work = try decodeFixture(
            BotChatMarker.self, #"{"botId":"b1","origin":"owner","thread":{"kind":"work","workId":"w1"}}"#)
        XCTAssertEqual(work.thread, .work(workId: "w1"))
        XCTAssertTrue(work.isBotThread)
        let routineSession = try decodeFixture(
            BotChatMarker.self,
            #"{"botId":"b1","origin":"routine","thread":{"kind":"routineSession","routineId":"r1"}}"#)
        XCTAssertFalse(routineSession.isBotThread, "a project routine's session runs as a plain session")
        XCTAssertEqual(
            try decodeFixture(BotThread.self, #"{"kind":"somethingNew"}"#), .unknown)
        XCTAssertEqual(
            try decodeFixture(BotThread.self, #"{"kind":"peer","connectionId":"k"}"#), .peer(connectionId: "k"))
        let memory = try decodeFixture(
            MemoryChatMarker.self, #"{"storeId":"mem-p-s","scope":{"kind":"project","spaceId":"s"},"passId":"p1"}"#)
        XCTAssertEqual(memory.scope, .project(spaceId: "s"))
    }

    func testWorkDefaultsMissingFieldsAndDerivesItsState() throws {
        // bot_records.rs `records_round_trip_and_default_missing_fields`.
        let work = try decodeFixture(
            BotWork.self,
            #"""
            {"id":"w1","botId":"b","title":"Fix it","chatId":"c","createdAt":1,
             "steps":[{"id":"1","title":"Do","state":"running","sessions":["s1"]},
                      {"id":"2","title":"Then","sessions":["s2","s3"]},{"id":"3","title":"Last","state":"done"}],
             "newField":true}
            """#)
        XCTAssertEqual(work.state, .proposed)
        XCTAssertEqual(work.instruction, "")
        XCTAssertTrue(work.awaitingApproval)
        XCTAssertTrue(work.isLive)
        XCTAssertEqual(work.sessionIds, ["s1", "s2", "s3"])
        XCTAssertEqual(work.steps[1].state, .pending)
        XCTAssertEqual(work.progress.done, 1)
        XCTAssertEqual(work.progress.total, 3)
        XCTAssertEqual(work.currentStep?.index, 0)
        var decided = work
        decided.decision = PlanDecision(approve: true, at: 2)
        XCTAssertFalse(decided.awaitingApproval)
        decided.steps[0].state = .done
        XCTAssertEqual(decided.currentStep?.index, 1, "else the first pending step")
        // A step without sessions keeps them off the wire, as serde skips them.
        let step = try encodedJSON(WorkStep(id: "1", title: "T"))
        XCTAssertEqual(step, try jsonFixture(#"{"id":"1","title":"T","state":"pending"}"#))
        XCTAssertThrowsError(
            try decodeFixture(
                BotWork.self, #"{"id":"w","botId":"b","title":"t","chatId":"c","createdAt":1,"state":"zzz"}"#),
            "an unknown work state fails the row, which projection skips")
    }

    func testAnEndedPlanClosesTheStepsItLeftOpen() throws {
        // bot_records.rs `an_ended_plan_closes_the_steps_it_left_open`.
        func states(_ state: String) throws -> [StepState] {
            try decodeFixture(
                BotWork.self,
                #"""
                {"id":"w","botId":"b","title":"t","chatId":"c","createdAt":1,"endedAt":90,"state":"\#(state)",
                 "steps":[{"id":"1","title":"a","state":"done"},{"id":"2","title":"b","state":"running"},
                          {"id":"3","title":"c","state":"asking"},{"id":"4","title":"d"}]}
                """#
            ).steps.map(\.state)
        }
        XCTAssertEqual(try states("running"), [.done, .running, .asking, .pending], "a live plan keeps its steps")
        XCTAssertEqual(try states("done"), [.done, .done, .done, .skipped])
        XCTAssertEqual(try states("failed"), [.done, .failed, .failed, .skipped])
        XCTAssertEqual(try states("stopped"), [.done, .skipped, .skipped, .skipped])
        XCTAssertEqual(try states("cancelled"), [.done, .running, .asking, .pending], "it never ran")
        // A step closed here ended when its plan did, so its timer stops.
        let failed = try decodeFixture(
            BotWork.self,
            #"""
            {"id":"w","botId":"b","title":"t","chatId":"c","createdAt":1,"endedAt":90,"state":"failed",
             "steps":[{"id":"1","title":"a","state":"running","startedAt":10}]}
            """#)
        XCTAssertEqual(failed.steps[0].endedAt, 90)
    }

    func testUnknownKindsReadAsTheirFallbacks() throws {
        XCTAssertEqual(try decodeFixture([ActivityKind].self, #"["future","planProposed"]"#), [.other, .planProposed])
        XCTAssertEqual(try decodeFixture(BotCapabilities.self, "{}"), BotCapabilities())
        XCTAssertEqual(try decodeFixture(BotPost.self, #"{"kind":"answer","from":"x"}"#), .unknown)
        let post = try decodeFixture(
            BotPost.self,
            #"{"kind":"routine","routineId":"r","name":"Morning brief","outcome":"done","runAt":5,"threadChatId":"t"}"#)
        XCTAssertEqual(
            post, .routine(routineId: "r", name: "Morning brief", outcome: .done, runAt: 5, threadChatId: "t"))
        XCTAssertEqual(
            try decodeFixture(BotPost.self, String(decoding: JSONEncoder().encode(post), as: UTF8.self)), post)
    }

    func testActivityGroupsAsTheBoardFiltersThem() {
        XCTAssertEqual(ActivityKind.approvalAsked.group, .plans)
        XCTAssertEqual(ActivityKind.worktreeCreated.group, .sessions)
        XCTAssertEqual(ActivityKind.taskRan.group, .routines)
        XCTAssertEqual(ActivityKind.noteWritten.group, .memory)
        XCTAssertEqual(ActivityKind.connectionRevoked.group, .connections)
        XCTAssertEqual(ActivityKind.other.group, .plans)
        XCTAssertEqual(ActivityGroup.allCases.map(\.label), ["Plans", "Sessions", "Routines", "Memory", "Connections"])
    }

    func testRoutinesDefaultAndRoundTripTheEngineShape() throws {
        let bare = try decodeFixture(
            BotRoutine.self, #"{"id":"r","botId":"b","name":"N","prompt":"P","createdAt":1,"updatedAt":2}"#)
        XCTAssertTrue(bare.enabled, "enabled defaults on")
        XCTAssertEqual(bare.target, .workspace)
        XCTAssertEqual(bare.schedule, .daily(time: "09:00", days: []))
        XCTAssertFalse(bare.writeNotes)
        let json = #"""
            {"id":"r1","botId":"b1","name":"Nightly tests","prompt":"Run the tests.",
             "target":{"kind":"project","spaceId":"s1","worktree":true},
             "schedule":{"kind":"daily","time":"02:00","days":[1,2,3,4,5]},"enabled":false,"writeNotes":false,
             "workDirectly":true,"createdAt":10,"updatedAt":20,"threadChatId":"t1",
             "lastRun":{"at":30,"endedAt":40,"outcome":"failed","summary":"2 failed","chatId":"t1"},"nextRunAt":50}
            """#
        let routine = try decodeFixture(BotRoutine.self, json)
        XCTAssertEqual(routine.target, .project(spaceId: "s1", worktree: true, model: nil))
        XCTAssertEqual(routine.lastRun?.outcome, .failed)
        XCTAssertFalse(routine.lastRun?.manual ?? true)
        XCTAssertEqual(try encodedJSON(routine), try jsonFixture(json), "what saveBotRoutine sends is the Rust shape")
        XCTAssertNil(routine.validationError)
        var nameless = routine
        nameless.name = "  "
        XCTAssertEqual(nameless.validationError, "Name the routine.")
        XCTAssertEqual(
            try encodedJSON(Schedule.interval(minutes: 30)), try jsonFixture(#"{"kind":"interval","minutes":30}"#))
        XCTAssertEqual(try encodedJSON(Schedule.manual), try jsonFixture(#"{"kind":"manual"}"#))
        XCTAssertEqual(
            try encodedJSON(Schedule.once(at: "2026-11-12T10:00")),
            try jsonFixture(#"{"kind":"once","at":"2026-11-12T10:00"}"#))
        XCTAssertEqual(
            try encodedJSON(RunRecord(at: 1, outcome: .done, manual: true)),
            try jsonFixture(#"{"at":1,"outcome":"done","manual":true}"#))
    }

    func testSchedulesDescribeThemselvesAsTheDesktopDoes() {
        XCTAssertEqual(Schedule.daily(time: "09:00", days: []).describe, "Daily 09:00")
        XCTAssertEqual(Schedule.daily(time: "09:00", days: [7, 6, 5, 4, 3, 2, 1]).describe, "Daily 09:00")
        XCTAssertEqual(Schedule.daily(time: "18:30", days: [1, 2, 3, 4, 5]).describe, "Weekdays 18:30")
        XCTAssertEqual(Schedule.daily(time: "10:00", days: [7, 6]).describe, "Weekends 10:00")
        XCTAssertEqual(Schedule.daily(time: "08:15", days: [3, 1, 1]).describe, "Mon, Wed 08:15")
        XCTAssertEqual(Schedule.interval(minutes: 120).describe, "Every 2 h")
        XCTAssertEqual(Schedule.interval(minutes: 60).describe, "Every hour")
        XCTAssertEqual(Schedule.interval(minutes: 45).describe, "Every 45 min")
        XCTAssertEqual(Schedule.manual.describe, "Manual")
        XCTAssertEqual(Schedule.once(at: "2026-11-10T09:00").describe, "Once, Tue Nov 10 09:00")
        XCTAssertEqual(Schedule.once(at: "soon").describe, "Once")
        XCTAssertNotNil(Schedule.interval(minutes: 5).validationError)
        XCTAssertNotNil(Schedule.daily(time: "25:00", days: []).validationError)
        XCTAssertNotNil(Schedule.daily(time: "09:00", days: [8]).validationError)
        XCTAssertNil(Schedule.daily(time: "9:05", days: [1]).validationError)
        XCTAssertNotNil(Schedule.once(at: "2026-11-10 09:00").validationError)
    }

    func testShortcutsFillTheirBlankAndReadBackInOrder() throws {
        let shortcut = try decodeFixture(
            BotShortcut.self,
            #"""
            {"id":"s1","botId":"b1","label":"Fix tests","icon":"test-tube",
             "request":"Start a session in invoice-demo to fix the tests. {input}","createdAt":1,"updatedAt":1}
            """#)
        XCTAssertEqual(shortcut.order, 0)
        XCTAssertTrue(shortcut.wantsInput)
        XCTAssertEqual(
            shortcut.request(with: "  Focus on tax. "),
            "Start a session in invoice-demo to fix the tests. Focus on tax.")
        var records = BotRecords()
        for (id, order, bot) in [("c", 2, "b1"), ("a", 0, "b1"), ("x", 0, "b2"), ("b", 1, "b1")] {
            var s = shortcut
            s.id = id
            s.order = Int64(order)
            s.botId = bot
            records.shortcuts.append(s)
        }
        XCTAssertEqual(records.shortcuts(of: "b1").map(\.id), ["a", "b", "c"])
    }

    func testNetworkRecordsDefaultTheirOwnerFields() throws {
        let connection = try decodeFixture(
            BotConnection.self,
            #"{"id":"k","botId":"b","peer":{"roomId":"r","name":"Ledger"},"createdAt":1}"#)
        XCTAssertEqual(connection.replyMode, .auto)
        XCTAssertNil(connection.scope)
        XCTAssertEqual(connection.peer.ownerName, "")
        XCTAssertTrue(connection.isLive)
        let invite = try decodeFixture(
            BotInvite.self, #"{"id":"i","botId":"b","createdAt":1,"expiresAt":100,"maxUses":1}"#)
        XCTAssertTrue(invite.isOpen(nowMs: 50))
        XCTAssertFalse(invite.isOpen(nowMs: 100), "expired")
        var used = invite
        used.uses = 1
        XCTAssertFalse(used.isOpen(nowMs: 50))
        let exchange = try decodeFixture(
            BotExchange.self,
            #"""
            {"id":"e","botId":"b","connectionId":"k","direction":"incoming","question":"Q?",
             "answer":"Draft","state":"awaitingReview","askedAt":5}
            """#)
        XCTAssertTrue(exchange.awaitingReview)
        var reviewed = exchange
        reviewed.review = ReviewDecision(send: true, text: nil, at: 6)
        XCTAssertFalse(reviewed.awaitingReview)
        XCTAssertEqual(try encodedJSON(reviewed.review), try jsonFixture(#"{"send":true,"at":6}"#))
        let created = try decodeFixture(
            InviteCreated.self, #"{"inviteId":"i","code":"room.secret","expiresAt":9}"#)
        XCTAssertEqual(created.maxUses, 1)
        XCTAssertEqual(created.code, "room.secret")
    }

    func testOnlyAvatarHostsGiveAPeerItsPhoto() {
        // bot_wire.rs `only_avatar_hosts_give_a_peer_its_photo`.
        for ok in [
            "https://avatars.githubusercontent.com/u/123?v=4",
            "https://lh3.googleusercontent.com/a/ACg8oc",
            "https://www.gravatar.com/avatar/abc?d=identicon",
            "https://GRAVATAR.com/avatar/abc",
        ] {
            XCTAssertEqual(peerAvatarURL(ok)?.absoluteString, ok, ok)
        }
        for bad in [
            "http://avatars.githubusercontent.com/u/1",
            "https://evil.example/pixel.png",
            "https://githubusercontent.com.evil.example/a.png",
            "https://evilgravatar.com/a.png",
            "https://user@avatars.githubusercontent.com/u/1",
            "https://avatars.githubusercontent.com:8443/u/1",
            "https://avatars.githubusercontent.com/u/1 x",
            "file:///etc/passwd",
            "",
        ] {
            XCTAssertNil(peerAvatarURL(bad), bad)
        }
    }

    func testClipTrimsAndMarksTheCut() {
        XCTAssertEqual(clip("  short  ", max: 10), "short")
        XCTAssertEqual(clip("abcdefghij", max: 5), "abcd…")
        XCTAssertEqual(clip("abcde", max: 5), "abcde")
    }

    func testMemoryShapesRoundTrip() throws {
        XCTAssertEqual(
            try decodeFixture(MemoryScope.self, #"{"kind":"project","spaceId":"s"}"#), .project(spaceId: "s"))
        XCTAssertEqual(
            try encodedJSON(MemoryScope.role(agent: "planner", spaceId: nil)),
            try jsonFixture(#"{"kind":"role","agent":"planner"}"#))
        XCTAssertThrowsError(try decodeFixture(MemoryScope.self, #"{"kind":"team"}"#))
        let edits: [(MemoryEdit, String)] = [
            (.write(path: "/MEMORY.md", text: "x"), #"{"op":"write","path":"/MEMORY.md","text":"x"}"#),
            (.delete(path: "/a.md"), #"{"op":"delete","path":"/a.md"}"#),
            (.rename(from: "/a.md", to: "/b.md"), #"{"op":"rename","from":"/a.md","to":"/b.md"}"#),
            (.accept(id: "p1"), #"{"op":"accept","id":"p1"}"#),
            (.discard(id: "p1"), #"{"op":"discard","id":"p1"}"#),
            (.undo(changeId: "c1"), #"{"op":"undo","changeId":"c1"}"#),
            (.undoPass(passId: "x"), #"{"op":"undoPass","passId":"x"}"#),
            (.setConsolidation(enabled: true, hour: 3), #"{"op":"setConsolidation","enabled":true,"hour":3}"#),
            (.runPass, #"{"op":"runPass"}"#),
        ]
        for (edit, json) in edits {
            XCTAssertEqual(try encodedJSON(edit), try jsonFixture(json), json)
            XCTAssertEqual(try decodeFixture(MemoryEdit.self, json), edit, json)
        }
    }

    func testMemoryStoreRowsDefaultEveryCount() throws {
        let store = try decodeFixture(MemoryStore.self, #"{"id":"mem-u-x","scope":{"kind":"you"}}"#)
        XCTAssertEqual(store.consolidation, Consolidation(enabled: true, hour: 3))
        XCTAssertEqual(store.files, 0)
        XCTAssertEqual(store.hostDeviceId, "")
        XCTAssertTrue(store.held.isEmpty)
        XCTAssertEqual(
            try encodedJSON(store),
            try jsonFixture(
                #"""
                {"id":"mem-u-x","scope":{"kind":"you"},"hostDeviceId":"","files":0,"bytes":0,"pending":0,"preview":"",
                 "createdAt":0,"updatedAt":0,"consolidation":{"enabled":true,"hour":3}}
                """#))
        XCTAssertEqual(try decodeFixture(Consolidation.self, #"{"hour":5}"#), Consolidation(enabled: true, hour: 5))
        XCTAssertEqual(try decodeFixture(MemoryPass.self, "{}").outcome, .running)
    }

    func testAMemoryViewFrameDecodes() throws {
        let view = try decodeFixture(
            MemoryView.self,
            #"""
            {"store":{"id":"mem-w-b1","scope":{"kind":"workspace","botId":"b1"},"hostDeviceId":"mac","files":1,
                      "bytes":12,"pending":1,"preview":"# Atlas","createdAt":1,"updatedAt":2,
                      "consolidation":{"enabled":false,"hour":4},
                      "lastPass":{"id":"p","at":1,"outcome":"done","changes":2,"tokens":900,"sources":3,"manual":false},
                      "month":{"month":"2026-11","tokens":900,"passes":1},
                      "held":[{"id":"h","chatId":"c","toolCallId":"call-1","path":"/a.md","reason":"Read a page","at":3}],
                      "decided":[{"id":"d","toolCallId":"call-0","saved":true,"at":2}]},
             "files":[{"path":"/MEMORY.md","text":"# Atlas","author":{"kind":"owner"},"createdAt":1,"updatedAt":2}],
             "pending":[{"id":"h","at":3,"author":{"kind":"workspace","botId":"b1","chatId":"c","title":"Atlas"},
                         "path":"/a.md","command":"create","args":{"path":"/workspace/a.md"},"reason":"Read a page",
                         "toolCallId":"call-1"}],
             "changes":[{"id":"x","at":2,"author":{"kind":"consolidation","passId":"p"},"path":"/MEMORY.md",
                         "action":{"kind":"rename","from":"/old.md"},"summary":"Renamed"}]}
            """#)
        XCTAssertEqual(view.store.heldForCall("call-1")?.path, "/a.md")
        XCTAssertEqual(view.store.decidedForCall("call-0")?.saved, true)
        XCTAssertNil(view.store.heldForCall("call-0"))
        XCTAssertEqual(view.pending.first?.author.label, "Atlas")
        XCTAssertEqual(view.changes.first?.action, .rename(from: "/old.md"))
        XCTAssertEqual(view.changes.first?.author.label, "Consolidation")
        XCTAssertEqual(MemoryAuthor.session(chatId: "c", title: " ").label, "A session")
        XCTAssertEqual(MemoryAuthor.owner.label, "You")
        XCTAssertTrue(validMemoryPath("/decisions/rounding.md"))
        for bad in ["MEMORY.md", "/notes.txt", "/../x.md", "/a//b.md", "/.hidden.md", "/a\\b.md", "/a|b.md"] {
            XCTAssertFalse(validMemoryPath(bad), bad)
        }
    }
}

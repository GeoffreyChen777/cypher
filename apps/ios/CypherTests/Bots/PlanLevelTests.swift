// A plan's permission level (docs/design/agent-bots.md §6.6): the new
// BotWork and PlanSettings fields read backward-compatibly, and the picker's
// choice reaches the registry as exactly what the engine's owner path writes
// (bots.rs `ResolveBotPlan` / `SetBotWorkLevel`: the owner field `level`).

import XCTest
@testable import Cypher

@MainActor
final class PlanLevelTests: XCTestCase {
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

    private func put(_ kind: String, _ id: String, _ json: String) throws {
        guard case .object(let fields) = try jsonFixture(json) else { return XCTFail("not an object") }
        doc.write(kind: kind, id: id, op: .upsert, set: fields)
    }

    private func plan(_ id: String, state: String, extra: String = "") throws {
        try put(
            "botWork", id,
            #"{"id":"\#(id)","botId":"b1","title":"Plan","chatId":"bot-chat","createdAt":10,"state":"\#(state)"\#(extra)}"#
        )
    }

    private func lastSet() throws -> [String: JSONValue] {
        let op = try XCTUnwrap(doc.pending.last?.ops.first)
        XCTAssertEqual(op.kind, "botWork")
        XCTAssertEqual(op.op, .update, "a field set never creates the row")
        return op.set ?? [:]
    }

    // MARK: Reading

    func testRowsFromBeforeLevelsReadAsEditProject() throws {
        let old = try decodeFixture(
            BotWork.self, #"{"id":"w","botId":"b","title":"t","state":"running","chatId":"c","createdAt":0}"#)
        XCTAssertNil(old.level)
        XCTAssertNil(old.proposedLevel)
        XCTAssertNil(old.levelQuote)
        XCTAssertNil(old.appliedLevel)
        XCTAssertEqual(old.effectiveLevel.label, "Edit project")
        XCTAssertEqual(old.levelInEffect.label, "Edit project")
        XCTAssertFalse(old.levelPending)
        let settings = try decodeFixture(PlanSettings.self, #"{"timeLimitMinutes":60}"#)
        XCTAssertNil(settings.level)
        XCTAssertEqual(settings.planLevel.sandbox, "workspace-write")
        XCTAssertEqual(try encodedJSON(PlanSettings()), try jsonFixture("{}"), "nothing new written back")
    }

    func testTheOwnersChoiceWinsOverTheProposal() throws {
        let work = try decodeFixture(
            BotWork.self,
            #"""
            {"id":"w","botId":"b","title":"t","state":"running","chatId":"c","createdAt":0,
             "level":"danger-full-access","proposedLevel":"read-only","levelQuote":"run whatever you need",
             "appliedLevel":"read-only"}
            """#)
        XCTAssertEqual(work.effectiveLevel.label, "Full access")
        XCTAssertEqual(work.levelInEffect.label, "Ask before edits")
        XCTAssertTrue(work.levelPending, "the host hasn't given it to the sessions yet")
        XCTAssertEqual(work.levelQuote, "run whatever you need")
        var proposed = work
        proposed.level = nil
        XCTAssertEqual(proposed.effectiveLevel.label, "Ask before edits")
        proposed.state = .proposed
        XCTAssertFalse(proposed.levelPending, "only a running plan applies one")

        // A level from a newer engine keeps the row (and reads as the default).
        let newer = try decodeFixture(
            BotWork.self,
            #"{"id":"w","botId":"b","title":"t","chatId":"c","createdAt":0,"level":"sandboxed","proposedLevel":7}"#)
        XCTAssertEqual(newer.level, "sandboxed")
        XCTAssertNil(newer.proposedLevel)
        XCTAssertEqual(newer.effectiveLevel.label, "Edit project")

        let bot = try decodeFixture(
            Bot.self,
            #"""
            {"id":"b","name":"Bob","hostDeviceId":"d","chatId":"c","createdAt":0,
             "plans":{"runningMax":2,"level":"danger-full-access"}}
            """#)
        XCTAssertEqual(bot.plans.planLevel.label, "Full access")
        XCTAssertEqual(bot.plans.running, 2, "the rest of the settings still read")
        XCTAssertFalse(bot.plans.isDefault)
        XCTAssertEqual(
            try encodedJSON(PlanSettings(level: "read-only")), try jsonFixture(#"{"level":"read-only"}"#))

        let activity = try decodeFixture(ActivityKind.self, #""planLevelChanged""#)
        XCTAssertEqual(activity, .planLevelChanged)
        XCTAssertEqual(activity.group, .plans)
    }

    func testTheSettingsDraftStoresTheDefaultAsNone() throws {
        let bot = try decodeFixture(
            Bot.self, #"{"id":"b","name":"Bob","hostDeviceId":"d","chatId":"c","createdAt":0}"#)
        var draft = BotSettingsDraft(bot: bot)
        XCTAssertEqual(draft.planLevel.label, "Edit project")
        draft.planLevel = PermissionLevel.of(sandbox: PermissionLevel.fullAccess)
        XCTAssertEqual(draft.plans.level, "danger-full-access")
        XCTAssertEqual(try encodedJSON(draft.plans), try jsonFixture(#"{"level":"danger-full-access"}"#))
        draft.planLevel = PermissionLevel.of(sandbox: "workspace-write")
        XCTAssertNil(draft.plans.level, "follows a later default")
        XCTAssertTrue(draft.plans.isDefault)
    }

    func testLevelsRankStrictestFirst() {
        let ask = PermissionLevel.of(sandbox: "read-only")
        let edit = PermissionLevel.of(sandbox: "workspace-write")
        let full = PermissionLevel.of(sandbox: "danger-full-access")
        XCTAssertTrue(full.isAbove(edit))
        XCTAssertTrue(edit.isAbove(ask))
        XCTAssertFalse(ask.isAbove(ask))
        XCTAssertFalse(edit.isAbove(full))
        XCTAssertEqual(PermissionLevel.all.map(\.label), ["Ask before edits", "Edit project", "Full access"])
    }

    // MARK: Writing

    func testApprovingConfirmsTheLevelPickedOnTheCard() async throws {
        try plan("w1", state: "proposed", extra: #","proposedLevel":"read-only""#)
        let store = store()
        // The picker on the card: the owner field, alone.
        try await store.setWorkLevel(workId: "w1", level: "danger-full-access")
        XCTAssertEqual(try lastSet(), ["level": .string("danger-full-access")])
        XCTAssertEqual(store.botRecords.work.first?.effectiveLevel.label, "Full access", "the card shows it at once")
        let writes = doc.pending.count
        try await store.setWorkLevel(workId: "w1", level: "danger-full-access")
        XCTAssertEqual(doc.pending.count, writes, "the level it already has writes nothing")
        // Approve: the decision and the level, in one update.
        try await store.resolvePlan(workId: "w1", approve: true)
        let set = try lastSet()
        XCTAssertEqual(Set(set.keys), ["decision", "level"])
        XCTAssertEqual(set["level"], .string("danger-full-access"))
    }

    func testApprovingAsProposedWritesTheProposalsLevel() async throws {
        try plan("w1", state: "proposed", extra: #","proposedLevel":"read-only""#)
        try plan("w2", state: "proposed")
        let store = store()
        try await store.resolvePlan(workId: "w1", approve: true)
        XCTAssertEqual(try lastSet()["level"], .string("read-only"))
        try await store.resolvePlan(workId: "w2", approve: false)
        XCTAssertEqual(Set(try lastSet().keys), ["decision"], "rejecting confirms no level")
    }

    func testARunningPlansLevelChangesAndAnEndedOneRefuses() async throws {
        try plan("run", state: "running", extra: #","level":"workspace-write","appliedLevel":"workspace-write""#)
        try plan("done", state: "done")
        let store = store()
        try await store.setWorkLevel(workId: "run", level: "read-only")
        XCTAssertEqual(try lastSet(), ["level": .string("read-only")])
        let work = try XCTUnwrap(store.botRecords.work.first { $0.id == "run" })
        XCTAssertEqual(work.effectiveLevel.label, "Ask before edits")
        XCTAssertTrue(work.levelPending, "until its host gives it to the sessions")
        let writes = doc.pending.count
        do {
            try await store.setWorkLevel(workId: "done", level: "read-only")
            XCTFail("an ended plan")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "Only a proposed or running plan's permission level can change.")
        }
        do {
            try await store.setWorkLevel(workId: "run", level: "standard")
            XCTFail("not a sandbox value")
        } catch is BotActionError {}
        do {
            try await store.setWorkLevel(workId: "gone", level: "read-only")
            XCTFail("no such plan")
        } catch let error as BotActionError {
            XCTAssertEqual(error.message, "This plan no longer exists.")
        }
        XCTAssertEqual(doc.pending.count, writes)
    }

    func testTheDemoHostAppliesALevelChange() async throws {
        let demo = DemoDataset.standard()
        let flaky = try XCTUnwrap(demo.work(id: "work-flaky"))
        XCTAssertEqual(flaky.effectiveLevel.label, "Full access")
        XCTAssertFalse(flaky.levelPending)
        try await demo.setWorkLevel(workId: "work-flaky", level: "workspace-write")
        XCTAssertEqual(demo.work(id: "work-flaky")?.effectiveLevel.label, "Edit project")
        XCTAssertEqual(demo.work(id: "work-flaky")?.levelPending, true)
        try await demo.resolvePlan(workId: "work-rounding", approve: true)
        XCTAssertEqual(demo.work(id: "work-rounding")?.level, "workspace-write")
        do {
            try await demo.setWorkLevel(workId: "work-notes", level: "read-only")
            XCTFail("an ended plan")
        } catch is BotActionError {}
    }
}

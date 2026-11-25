// The plan-question rule (Bots/PlanAsks.swift) on the shared vectors in
// protocol/vectors/plan-asks-v1.json (crates/proto/tests/plan_asks_vectors.rs
// runs the same file), and the reads built on it: the session row's
// `asking`, Needs you, the card's asking chat and its offline state.

import XCTest

@testable import Cypher

final class PlanAsksTests: XCTestCase {
    private func json(_ value: Any) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: JSONSerialization.data(withJSONObject: value))
    }

    private func indicator(_ status: SessionStatus?) -> String {
        switch status {
        case .working: "working"
        case .awaitingInput: "awaitingInput"
        case .errored: "errored"
        case .idle, nil: "none"
        }
    }

    func testSharedVectors() throws {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/plan-asks-v1.json")
        let root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        let now = try XCTUnwrap((root["now"] as? NSNumber)?.int64Value)
        let cases = try XCTUnwrap(root["cases"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 15)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let online = Set(try XCTUnwrap(vector["online"] as? [String]))
            let chats = try XCTUnwrap(vector["chats"] as? [[String: Any]])
            let tree = ChatTree(
                links: try chats.map { chat in
                    (
                        try XCTUnwrap(chat["id"] as? String),
                        (chat["child"] as? [String: Any])?["parentChatId"] as? String,
                        chat["archived"] as? Bool ?? false
                    )
                })
            var sessions: [String: SessionRow] = [:]
            for raw in try XCTUnwrap(vector["sessions"] as? [[String: Any]]) {
                let fields = try XCTUnwrap(try json(raw).objectValue)
                let row = try XCTUnwrap(SessionRow(fields: fields), name)
                sessions[row.chatId] = row
            }
            let work = try JSONDecoder().decode(
                BotWork.self, from: JSONSerialization.data(withJSONObject: try XCTUnwrap(vector["work"])))
            let question = { (chat: String) in
                findPlanQuestion(
                    tree: tree, chatId: chat, sessions: { sessions[$0] }, deviceOnline: online.contains, now: now)
            }
            let records = BotRecords(work: [work])
            let needs = records.needsYou(botId: "b", chatId: nil) { question($0) != nil }
            let expect = try XCTUnwrap(vector["expect"] as? [String: Any])
            let wantNeeds = try XCTUnwrap(expect["needsYou"] as? [[String: Any]])
            XCTAssertEqual(needs.count, wantNeeds.count, "\(name): needsYou")
            for (item, want) in zip(needs, wantNeeds) {
                guard case .asking(let chatId, let workId) = item else {
                    XCTFail("\(name): only questions here, got \(item)")
                    continue
                }
                XCTAssertEqual(chatId, want["chatId"] as? String, "\(name): chatId")
                XCTAssertEqual(workId, want["workId"] as? String, "\(name): workId")
                let found = try XCTUnwrap(question(chatId), name)
                XCTAssertEqual(found.chatId, want["askingChatId"] as? String, "\(name): askingChatId")
                XCTAssertEqual(found.offline, want["offline"] as? Bool, "\(name): offline")
                XCTAssertEqual(
                    sessions[found.chatId]?.asking?.requestId, want["requestId"] as? String, "\(name): requestId")
            }
            let steps = work.settledSteps { question($0) != nil }.map(\.rawValue)
            XCTAssertEqual(steps, expect["steps"] as? [String], "\(name): steps")
            for (chat, want) in try XCTUnwrap(expect["indicators"] as? [String: String]) {
                XCTAssertEqual(
                    indicator(effectiveStatus(sessions[chat], now: now)), want, "\(name): indicator of \(chat)")
            }
        }
    }

    func testAskingDecodesLenientlyAndOldRowsHaveNone() throws {
        let base: [String: Any] = [
            "chatId": "s1", "deviceId": "mac", "status": "awaitingInput", "updatedAt": 1_000,
        ]
        let old = try XCTUnwrap(SessionRow(fields: try XCTUnwrap(try json(base).objectValue)))
        XCTAssertNil(old.asking)
        var full = base
        full["asking"] = [
            "requestId": "m1", "askedAt": 900, "permission": "escalate", "askedBy": "reviewer", "fromChatId": "k1",
        ]
        let row = try XCTUnwrap(SessionRow(fields: try XCTUnwrap(try json(full).objectValue)))
        XCTAssertEqual(
            row.asking,
            SessionAsk(requestId: "m1", askedAt: 900, permission: "escalate", askedBy: "reviewer", fromChatId: "k1"))
        // Cleared on the host (null) or malformed: no question, the row stays.
        for bad: Any in [NSNull(), ["askedAt": 900], "r1", ["requestId": ""]] {
            var fields = base
            fields["asking"] = bad
            let row = try XCTUnwrap(SessionRow(fields: try XCTUnwrap(try json(fields).objectValue)))
            XCTAssertNil(row.asking, "\(bad)")
        }
    }

    func testAnOpenQuestionNeverAgesOutButAnOldRowStillDoes() {
        let now: Int64 = 10_000_000
        var row = SessionRow(chatId: "s1", deviceId: "mac", status: .awaitingInput, updatedAt: now - sessionStaleMs - 1)
        // Old host: stale reads as nothing, as before.
        XCTAssertNil(effectiveStatus(row, now: now))
        XCTAssertEqual(sessionAsk(row, now: now, hostOnline: true), .none)
        row.asking = SessionAsk(requestId: "r1", askedAt: now - 60_000)
        XCTAssertEqual(effectiveStatus(row, now: now), .awaitingInput)
        XCTAssertEqual(sessionAsk(row, now: now, hostOnline: true), .asking)
        XCTAssertEqual(sessionAsk(row, now: now, hostOnline: false), .askingOffline)
        // A run that died under it left the row idle: still asking.
        row.status = .idle
        XCTAssertEqual(effectiveStatus(row, now: now), .awaitingInput)
        // Working rows keep their 45 s gate.
        row.asking = nil
        row.status = .working
        XCTAssertNil(effectiveStatus(row, now: now))
    }

    func testTheWalkIsBoundedAndSkipsEndedSubtrees() {
        // A chain s0 → s1 → … → s10: only 8 levels under s0 are searched.
        let links = (1...10).map { ("s\($0)", Optional("s\($0 - 1)"), false) }
        let tree = ChatTree(links: [("s0", nil, false)] + links)
        XCTAssertEqual(tree.question("s0") { $0 == "s8" ? .asking : .none }?.chatId, "s8")
        XCTAssertNil(tree.question("s0") { $0 == "s9" ? .asking : .none })
        // Children in id order, level by level; an archived chat hides its own.
        let wide = ChatTree(links: [("r", nil, false), ("b", "r", false), ("a", "r", true), ("a1", "a", false)])
        XCTAssertEqual(wide.children(of: "r"), ["a", "b"])
        XCTAssertNil(wide.question("r") { $0 == "a1" ? .asking : .none })
        XCTAssertEqual(wide.question("r") { $0 == "b" || $0 == "a" ? .asking : .none }?.chatId, "b")
    }

    /// The demo's plan with a question in every way one asks: each reaches
    /// Needs you, the card answers in the chat that asks, the offline one
    /// says so, and the counts follow.
    @MainActor func testDemoPlanQuestionsReachNeedsYou() throws {
        let demo = DemoDataset.standard()
        demo.addPlanAsks()
        let bot = try XCTUnwrap(demo.bot(id: DemoDataset.atlasId))
        let rows = demo.boardNeedRows(bot)
        XCTAssertEqual(
            rows.map(\.item),
            DemoDataset.planAskChats.map { .asking(chatId: $0.session, workId: DemoDataset.planAsksWorkId) })
        XCTAssertEqual(rows.map(\.askingChatId), DemoDataset.planAskChats.map(\.asker))
        XCTAssertEqual(rows.map(\.waiting), [false, false, false, false, true])
        XCTAssertTrue(rows[4].sub.hasPrefix(BoardNeedRow.waitingForDevice))
        XCTAssertEqual(demo.botStatus(bot), .needsYou(5))
        // The step session whose run ended asks through its subagent's chat.
        XCTAssertNotEqual(effectiveStatus(demo.sessions["chat-pa-sub"], now: nowMs()), .awaitingInput)
        XCTAssertTrue(demo.sessionAsking("chat-pa-sub"))
        // Answered (the host clears `asking`): the item is gone.
        demo.sessions["chat-pa-restart"]?.asking = nil
        XCTAssertFalse(
            demo.needsYou(botId: bot.id).contains(
                .asking(chatId: "chat-pa-restart", workId: DemoDataset.planAsksWorkId)))
        XCTAssertEqual(demo.needsYouCount(botId: bot.id), 4)
    }

    func testSteeredSessionsAreThePlansButNotStarted() throws {
        let work = try JSONDecoder().decode(
            BotWork.self,
            from: Data(
                #"""
                {"id": "w", "botId": "b", "title": "Plan", "chatId": "c", "createdAt": 0, "state": "running",
                 "steered": ["x", "s1"],
                 "steps": [{"id": "1", "title": "A", "state": "done", "sessions": ["s1"]},
                           {"id": "2", "title": "B", "state": "running"},
                           {"id": "3", "title": "C", "state": "pending"}]}
                """#.utf8))
        XCTAssertEqual(work.planChatIds, ["s1", "x"])
        XCTAssertEqual(work.sessionIds, ["s1"])
        XCTAssertEqual(work.settledSteps { $0 == "x" }, [.done, .asking, .pending])
        XCTAssertEqual(work.settledSteps { _ in false }, [.done, .running, .pending])
        let records = BotRecords(work: [work])
        XCTAssertEqual(records.needsYou(botId: "b", chatId: nil) { $0 == "x" }, [.asking(chatId: "x", workId: "w")])
    }

    @MainActor func testAPlanSessionsPushOpensItsAskingPage() throws {
        let demo = DemoDataset.standard()
        demo.addPlanAsks()
        let payload = PushPayload(
            eventId: UUID().uuidString, scope: String(repeating: "a", count: 64), chatId: "chat-atlas",
            projectId: DemoDataset.atlasId, kind: "input", workspaceId: DemoDataset.atlasId,
            item: .asking("chat-pa-perm"))
        XCTAssertEqual(
            demo.notificationTarget(payload),
            .workspace(
                botId: DemoDataset.atlasId, chatId: "chat-atlas",
                page: .asking(chatId: "chat-pa-perm", workId: DemoDataset.planAsksWorkId)))
    }
}

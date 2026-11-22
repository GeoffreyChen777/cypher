// Plan bodies (docs/design/plan-body.md): `PlanText.of` on the shared vectors
// in protocol/vectors/plan-text-v1.json (crates/proto/tests/plan_text_vectors.rs
// runs the same file), the session store's `planBodies` read from a real
// Loro doc, and the demo's long plans.

import Loro
import XCTest

@testable import Cypher

@MainActor
final class PlanTextTests: XCTestCase {
    func testPlanTextVectors() throws {
        let url = try TestSupport.repoRoot().appendingPathComponent("protocol/vectors/plan-text-v1.json")
        let root = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        let cases = try XCTUnwrap(root["planText"] as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(cases.count, 10)
        for vector in cases {
            let name = try XCTUnwrap(vector["name"] as? String)
            let work = try JSONDecoder().decode(
                BotWork.self, from: JSONSerialization.data(withJSONObject: try XCTUnwrap(vector["work"])))
            let bodies = vector["bodies"].flatMap { $0 is NSNull ? nil : WorkBodies(json: $0) }
            let text = PlanText.of(work, bodies)
            let expect = try XCTUnwrap(vector["expect"] as? [String: Any])
            let details = try XCTUnwrap(expect["details"] as? [Any]).map { $0 as? String }
            XCTAssertEqual(text.instruction, expect["instruction"] as? String, "\(name): instruction")
            XCTAssertEqual(text.details, details, "\(name): details")
            XCTAssertEqual(text.plan.rawValue, expect["plan"] as? String, "\(name): plan")
            XCTAssertEqual(text.result, expect["result"] as? String, "\(name): result")
            XCTAssertEqual(text.resultSource.rawValue, expect["resultSource"] as? String, "\(name): result source")
            XCTAssertEqual(text.isLoading, expect["loading"] as? Bool, "\(name): loading")
        }
    }

    func testRowsReadTheirRevsAndOldRowsHaveNone() throws {
        let old = try decodeFixture(
            BotWork.self, #"{"id":"w","botId":"b","title":"t","chatId":"c","createdAt":1,"result":"done"}"#)
        XCTAssertNil(old.bodyRev)
        XCTAssertNil(old.resultRev)
        let long = try decodeFixture(
            BotWork.self,
            #"{"id":"w","botId":"b","title":"t","chatId":"c","createdAt":1,"bodyRev":"5c1e9a03d27b4f68","resultRev":"a7"}"#
        )
        XCTAssertEqual(long.bodyRev, "5c1e9a03d27b4f68")
        XCTAssertEqual(long.resultRev, "a7")
    }

    /// Writes `planBodies` the way plan_bodies.rs `put_plan_part` does: a
    /// map per work id holding plain JSON values.
    private func doc(_ entries: [String: [String: Any]], flat: [String: LoroValue] = [:]) throws -> LoroDoc {
        let doc = LoroDoc()
        let map = doc.getMap(id: "planBodies")
        for (workId, parts) in entries {
            let entry = try map.insertContainer(key: workId, child: LoroMap())
            for (part, value) in parts { try entry.insert(key: part, v: LoroValue.fromJSON(value)) }
        }
        for (key, value) in flat { try map.insert(key: key, v: value) }
        doc.commit()
        return doc
    }

    func testTheStoreReadsOnlyTheEntriesAskedFor() throws {
        let plan: [String: Any] = [
            "rev": "r1", "instruction": "全部 \"quoted\"\n", "details": ["1": "one", "2": "two"], "at": 5,
        ]
        let doc = try doc(
            [
                "w1": ["plan": plan, "result": ["rev": "r2", "text": "done", "at": 6] as [String: Any]],
                "w2": ["plan": "not a body", "result": ["rev": "r3", "text": "t", "at": 7] as [String: Any]],
                "w3": ["plan": plan],
            ], flat: ["flat": .i64(value: 7)])
        let read = SessionStore.readPlanBodies(of: doc, ids: ["w1", "w2", "flat", "missing"])
        XCTAssertEqual(Set(read.keys), ["w1", "w2", "flat"], "only what was asked for, and is there")
        XCTAssertEqual(
            read["w1"]?.plan,
            PlanBody(rev: "r1", instruction: "全部 \"quoted\"\n", details: ["1": "one", "2": "two"], at: 5))
        XCTAssertEqual(read["w1"]?.result, PlanResult(rev: "r2", text: "done", at: 6))
        XCTAssertNil(read["w2"]?.plan, "a part that doesn't read is missing")
        XCTAssertEqual(read["w2"]?.result?.text, "t", "the other part still reads")
        XCTAssertEqual(read["flat"], WorkBodies(), "an entry that isn't a map is empty")
        XCTAssertTrue(SessionStore.readPlanBodies(of: doc, ids: []).isEmpty)
        XCTAssertTrue(SessionStore.readPlanBodies(of: LoroDoc(), ids: ["w1"]).isEmpty, "a doc without the map")
    }

    func testAWatchReadsItsEntryUntilItEnds() throws {
        let store = SessionStore(chatId: "chat-ws", config: DemoDataset.dummyConfig, offline: true)
        let map = store.doc.getMap(id: "planBodies")
        let entry = try map.insertContainer(key: "w1", child: LoroMap())
        try entry.insert(
            key: "plan", v: LoroValue.fromJSON(["rev": "r1", "instruction": "full", "at": 1] as [String: Any]))
        store.doc.commit()
        XCTAssertTrue(store.planBodies.isEmpty, "nothing is read without a watch")

        store.watchPlanBody("w1")
        store.watchPlanBody("w1")
        store.watchPlanBody("w-none")
        XCTAssertEqual(store.planBodies["w1"]?.plan?.instruction, "full")
        XCTAssertNil(store.planBodies["w-none"])

        // A later write shows on the next read.
        try entry.insert(key: "result", v: LoroValue.fromJSON(["rev": "r2", "text": "all", "at": 2] as [String: Any]))
        store.doc.commit()
        store.refreshPlanBodies()
        XCTAssertEqual(store.planBodies["w1"]?.result?.text, "all")

        store.unwatchPlanBody("w1")
        XCTAssertNotNil(store.planBodies["w1"], "another page still shows it")
        store.unwatchPlanBody("w1")
        XCTAssertNil(store.planBodies["w1"])
        store.unwatchPlanBody("w-none")
        store.refreshPlanBodies()
        XCTAssertTrue(store.planBodies.isEmpty)
    }

    func testTheDemosLongPlansReadInFullOnceTheirBodiesAreHere() throws {
        let demo = DemoDataset.standard()
        demo.addLongPlans(bodies: false)
        let plan = try XCTUnwrap(demo.work(id: DemoDataset.longPlanWorkId))
        let done = try XCTUnwrap(demo.work(id: DemoDataset.longResultWorkId))
        // The rows are previews, as the host's clamp makes them.
        XCTAssertNotNil(plan.bodyRev)
        XCTAssertEqual(plan.instruction.count, BotCaps.instruction)
        XCTAssertTrue(plan.instruction.hasSuffix("…"))
        XCTAssertLessThanOrEqual(plan.steps.compactMap(\.detail).reduce(0) { $0 + $1.count }, 4_000)
        XCTAssertTrue(plan.steps.contains { $0.detail?.hasSuffix("…") == true }, "a detail was cut")
        XCTAssertNotNil(done.resultRev)
        XCTAssertEqual(done.result?.count, BotCaps.result)

        let store = demo.sessionStore(for: plan.chatId)
        store.watchPlanBody(plan.id)
        store.watchPlanBody(done.id)
        let loading = PlanText.of(plan, store.planBodies[plan.id])
        XCTAssertEqual(loading.plan, .preview)
        XCTAssertEqual(loading.instruction, plan.instruction)
        XCTAssertEqual(PlanText.of(done, store.planBodies[done.id]).resultSource, .preview)

        demo.writePlanBodies()
        let full = PlanText.of(plan, store.planBodies[plan.id])
        XCTAssertEqual(full.plan, .body)
        XCTAssertGreaterThan(full.instruction.count, plan.instruction.count)
        XCTAssertGreaterThan(full.details.compactMap { $0 }.reduce(0) { $0 + $1.count }, 4_000)
        XCTAssertFalse(full.isLoading)
        let result = PlanText.of(done, store.planBodies[done.id])
        XCTAssertEqual(result.resultSource, .body)
        XCTAssertGreaterThan(result.result?.count ?? 0, BotCaps.result)
    }

    func testPreviewsShareTheirBudgetLongestFirst() {
        let previews = DemoDataset.previews(
            [String(repeating: "a", count: 3_000), String(repeating: "b", count: 1_500), "c"], budget: 4_000)
        XCTAssertEqual(previews.map(\.count), [2_499, 1_500, 1])
        XCTAssertTrue(previews[0].hasSuffix("…"))
        XCTAssertEqual(DemoDataset.previews(["short"], budget: 4_000), ["short"])
    }
}

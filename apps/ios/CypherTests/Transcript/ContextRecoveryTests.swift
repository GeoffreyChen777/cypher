import Loro
import XCTest

@testable import Cypher

/// Context recovery on the phone — transcript/recovery.rs tests, ported, and
/// the two additive doc fields: the resent prompt's `recovery` record and the
/// session row's `recovery` progress.
@MainActor
final class ContextRecoveryTests: XCTestCase {
    private func record() -> ContextRecovery {
        ContextRecovery(
            context: "ctx", trigger: "can't help", matchedIn: .response,
            excerpt: "Sorry, I can't help.", restartAfter: "a1",
            restartPreview: "Here is the plan", attempt: 1, model: nil)
    }

    func testTheCardNamesWhereTheChatRestarted() {
        var record = record()
        XCTAssertEqual(record.restartLabel, "After \u{201c}Here is the plan\u{201d}")
        record.restartPreview = nil
        XCTAssertEqual(record.restartLabel, "After the previous answer")
        record.restartAfter = nil
        XCTAssertEqual(record.restartLabel, "The beginning of the chat")
    }

    func testTheCaptionCountsAttemptsAfterTheFirst() {
        var record = record()
        XCTAssertEqual(record.captionLabel, "Restarted with context")
        record.attempt = 2
        XCTAssertEqual(record.captionLabel, "Restarted with context \u{b7} attempt 2")
    }

    func testTheCardNamesWhatSetItOff() {
        var record = record()
        XCTAssertEqual(record.triggerLabel, "\u{201c}can't help\u{201d} in the agent\u{2019}s answer")
        record.matchedIn = .error
        record.trigger = "429"
        XCTAssertEqual(record.triggerLabel, "\u{201c}429\u{201d} in an error")
    }

    func testTheStripSaysWhatTheRecoveryIsDoing() {
        var progress = RecoveryProgress(
            stage: .writingContext, trigger: "can't help", matchedIn: .response,
            attempt: 1, startedAt: 1_000_000)
        var text = progress.trailerText(now: 1_007_000)
        XCTAssertEqual(text.word, "Writing context to retry")
        XCTAssertEqual(text.tail, "7s \u{b7} matched \u{201c}can't help\u{201d}")
        progress.stage = .restarting
        progress.attempt = 2
        text = progress.trailerText(now: 1_000_000)
        XCTAssertEqual(text.word, "Restarting with added context")
        XCTAssertTrue(text.tail.hasSuffix("\u{b7} attempt 2"), text.tail)
    }

    func testARecoveryLeftBehindStopsShowing() {
        let progress = RecoveryProgress(
            stage: .writingContext, trigger: "x", matchedIn: .error, attempt: 1, startedAt: 0)
        XCTAssertTrue(progress.isLive(now: RecoveryProgress.staleAfterMs - 1))
        XCTAssertFalse(progress.isLive(now: RecoveryProgress.staleAfterMs))
    }

    func testTheSessionRowsProgressDecodesChronoTimestamps() throws {
        let value: JSONValue = .object([
            "stage": .string("restarting"), "trigger": .string("429"),
            "matchedIn": .string("error"), "attempt": .int(2),
            "startedAt": .string("2026-10-14T21:39:44.123456789Z"),
        ])
        let progress = try XCTUnwrap(RecoveryProgress(value))
        XCTAssertEqual(progress.stage, .restarting)
        XCTAssertEqual(progress.matchedIn, .error)
        XCTAssertEqual(progress.attempt, 2)
        XCTAssertEqual(progress.startedAt, 1_792_013_984_123)
        XCTAssertEqual(RecoveryProgress.rfc3339Ms("2026-10-14T21:39:44Z"), 1_792_013_984_000)
        XCTAssertEqual(RecoveryProgress.rfc3339Ms("2026-10-14T22:39:44.5+01:00"), 1_792_013_984_500)
        // Lenient: a newer stage, or a malformed field, drops the recovery.
        XCTAssertNil(RecoveryProgress(.object(["stage": .string("later")])))
        XCTAssertNil(RecoveryProgress(nil))
    }

    func testTheResentPromptCarriesItsRecordAndAMalformedOneCostsOnlyItself() throws {
        let json: [String: Any] = [
            "messages": [
                [
                    "id": "u1", "role": "user", "createdAt": 1,
                    "parts": [["id": "t0", "kind": "text", "text": "Run them"]],
                    "recovery": [
                        "context": "Use the tunnel.", "trigger": "I can't access",
                        "matchedIn": "response", "excerpt": "I can't access it",
                        "restartAfter": "a0", "attempt": 1, "model": "anthropic/claude-haiku-4-5",
                    ],
                ],
                [
                    "id": "u2", "role": "user", "createdAt": 2,
                    "parts": [["id": "t0", "kind": "text", "text": "Again"]],
                    "recovery": ["context": "missing the rest"],
                ],
            ]
        ]
        let root = try XCTUnwrap(LoroValue.fromJSON(json).mapValue)
        let entries = SessionStore.decodeEntries(root: root)
        XCTAssertEqual(entries.map(\.id), ["u1", "u2"])
        let record = try XCTUnwrap(entries[0].recovery)
        XCTAssertEqual(record.context, "Use the tunnel.")
        XCTAssertEqual(record.restartAfter, "a0")
        XCTAssertNil(record.restartPreview)
        XCTAssertEqual(record.model, "anthropic/claude-haiku-4-5")
        XCTAssertNil(entries[1].recovery)

        let rows = buildRows(entries)
        guard case .user(_, false, let captioned) = rows[0].kind else { return XCTFail("Missing bubble") }
        XCTAssertEqual(captioned, record)
        var plain = entries[0]
        plain.recovery = nil
        XCTAssertNotEqual(rows[0].version, buildRows([plain])[0].version)
    }

    func testTheDemoShowcasesEveryState() throws {
        let demo = DemoDataset.standard()
        let ids = demo.chats.filter { $0.spaceId == DemoDataset.recoverySpaceId }.map(\.id)
        XCTAssertEqual(ids.count, 5)
        XCTAssertEqual(demo.sessions["chat-recovery"]?.recovery?.stage, .writingContext)
        XCTAssertEqual(demo.sessions["chat-recovery-restarting"]?.recovery?.stage, .restarting)
        let captions = { (chatId: String) in
            demo.sessionStore(for: chatId).entries.compactMap(\.recovery).map(\.captionLabel)
        }
        XCTAssertEqual(captions("chat-recovery-done"), ["Restarted with context"])
        XCTAssertEqual(captions("chat-recovery-limit"), ["Restarted with context \u{b7} attempt 3"])
    }

    func testTheReplayRewindsAndResendsWithTheContext() async throws {
        let demo = DemoDataset.standard()
        let chatId = DemoDataset.recoveryReplayChatId
        let store = demo.sessionStore(for: chatId)
        demo.recoveryTask?.cancel()
        demo.playRecoveryStory(store: store, chatId: chatId, prompt: "Deploy it.")
        let replay = try XCTUnwrap(demo.recoveryTask)
        // The stages the strip shows on the way.
        let sampler = Task { @MainActor in
            var stages: [RecoveryStage] = []
            while !Task.isCancelled {
                if let stage = demo.sessions[chatId]?.recovery?.stage, stages.last != stage { stages.append(stage) }
                try? await Task.sleep(nanoseconds: 50_000_000)
            }
            return stages
        }
        await replay.value
        sampler.cancel()
        let stages = await sampler.value
        XCTAssertEqual(stages, [.writingContext, .restarting])
        let entries = store.entries
        XCTAssertEqual(entries.count, 4, "The failed prompt and reply left the transcript")
        let resent = try XCTUnwrap(entries[2].recovery)
        XCTAssertEqual(resent.restartAfter, "p2")
        XCTAssertEqual(entries[3].status, .complete)
        XCTAssertEqual(demo.sessions[chatId]?.status, .idle)
        XCTAssertNil(demo.sessions[chatId]?.recovery)
    }
}

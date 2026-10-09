import XCTest
@testable import Cypher

/// An append-mode translation (the agent's answer, a rule, the translation)
/// folds its original behind a "Show original" toggle, collapsed by default —
/// transcript.rs `append_translation_*` tests, ported.
@MainActor
final class TranslationFoldTests: XCTestCase {
    private let original = "The answer.\n\n- one\n- two"
    private let translated = "答案。\n\n- 一\n- 二"

    private func entry(_ id: String, _ text: String, agentText: String?,
                       status: MessageStatus = .complete) -> MessageEntry {
        MessageEntry(id: id, role: .assistant,
                     parts: [.text(id: "t0", text: text, agentText: agentText)],
                     createdAt: 1, deviceId: "d", status: status, continuationOf: nil)
    }

    private func rows(_ entries: [MessageEntry]) -> [TranscriptRow] {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        return TranscriptRowBuilder.rows(entries: entries, pendingSends: [],
                                         parsers: &parsers, completed: &completed)
    }

    private func hasToggle(_ rows: [TranscriptRow]) -> Bool {
        rows.contains {
            if case .translationOriginal = $0.kind { return true }
            return false
        }
    }

    func testAppendTranslationFoldsItsOriginalBehindAToggle() {
        let built = rows([entry("m1", "\(original)\(translationAppendSeparator)\(translated)",
                                agentText: original)])
        // Toggle, the original's prose run, the rule, the translation's run.
        XCTAssertEqual(built.map(\.id), ["m1#t0.original", "m1#t0.0", "m1#t0.2", "m1#t0.3"])
        guard case .translationOriginal(let hidden) = built[0].kind else {
            return XCTFail("the part opens on the toggle")
        }
        XCTAssertEqual(hidden, 2)
        XCTAssertTrue(built[0].turnStart)

        // Collapsed by default: the translation keeps its block ids and the
        // reply's timestamp; everything before it is folded away.
        let folded = TranscriptRowBuilder.foldClosedToggles(built, pins: [:])
        XCTAssertEqual(folded.map(\.id), ["m1#t0.original", "m1#t0.3"])
        XCTAssertNotNil(folded.last?.timestamp)
        // It now follows the toggle, not the rule: a block gap, not a
        // same-part one.
        XCTAssertEqual(folded[1].topGap, TranscriptView.gapBlock)

        let open = TranscriptRowBuilder.foldClosedToggles(built, pins: ["m1#t0.original": true])
        XCTAssertEqual(open.map(\.id), built.map(\.id))
    }

    func testAppendTranslationShowsWholeUntilTheTranslationStarts() {
        // Replace mode, and an append rendering whose translation is empty.
        for text in ["答案。", "\(original)\(translationAppendSeparator)"] {
            XCTAssertFalse(hasToggle(rows([entry("m1", text, agentText: original, status: .streaming)])))
        }
        // No agent text: an ordinary reply that happens to contain a rule.
        XCTAssertFalse(hasToggle(rows([entry("m1", "\(original)\(translationAppendSeparator)\(translated)",
                                             agentText: nil)])))
    }

    func testAnOriginalEndingInsideAnOpenFenceShowsWhole() {
        // The rule lands inside the unclosed code block, so the rendering
        // does not split where the original ends.
        let fenced = "Run this:\n\n```sh\nmake test"
        let text = "\(fenced)\(translationAppendSeparator)\(translated)"
        XCTAssertFalse(hasToggle(rows([entry("m1", text, agentText: fenced)])))
    }

    func testFoldStateKeepsRoundIndicesOnTheRenderedRows() {
        let user = { (id: String) in
            MessageEntry(id: id, role: .user, parts: [.text(id: "t0", text: "Question \(id)")],
                         createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil)
        }
        let entries = [user("u1"),
                       entry("a1", "\(original)\(translationAppendSeparator)\(translated)",
                             agentText: original),
                       user("u2")]
        let cache = TranscriptBuilderCache()
        for open: [String: Bool] in [[:], ["a1#t0.original": true], [:]] {
            let rendered = cache.rows(revision: 1, entries: entries, pendingSends: [],
                                      togglePins: open)
            XCTAssertEqual(rendered.count, open.isEmpty ? 4 : 6)
            for round in cache.rounds {
                XCTAssertEqual(rendered[round.rowIndex].id, round.rowId)
            }
            XCTAssertEqual(cache.rounds.map(\.rowId), ["u1", "u2"])
        }
    }
}

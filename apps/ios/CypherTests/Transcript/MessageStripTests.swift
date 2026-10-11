import XCTest
import Loro
@testable import Cypher

/// The strip under each settled message — date, answering model, fork,
/// restart and copy (transcript.rs strip / rows.rs tests, ported).
@MainActor
final class MessageStripTests: XCTestCase {
    func testTheStripRidesTheLastRowOfASettledEntryWhateverItIs() {
        let call = RenderToolCall(tag: "exec", fields: ["command": "ls"])
        let built = buildRows([
            .fixture("u1", role: .user, parts: [.text(id: "t", text: "Go")], createdAt: 5),
            .fixture(
                "a1",
                parts: [
                    .text(id: "t0", text: "First.\n\nSecond."),
                    .tool(id: "c1", call: call, isError: false, resolved: true),
                ],
                createdAt: 7),
            .fixture("a2", parts: [.error(id: "e", message: "boom")], createdAt: 9),
        ])
        let stamped = built.filter { $0.timestamp != nil }
        XCTAssertEqual(stamped.map(\.id), ["u1", "a1#g0", "a2#e"])
        XCTAssertEqual(stamped.map(\.timestamp), [5, 7, 9])
        XCTAssertTrue(stamped.allSatisfy(\.copyable))
        // Every row of a settled entry may anchor a fork, not just the last.
        XCTAssertTrue(built.allSatisfy(\.settled))
    }

    func testAStreamingReplyHasNoStripUntilItSettles() {
        let text = MessagePart.text(id: "t0", text: "Partial")
        let live = buildRows([.fixture("a1", parts: [text], status: .streaming)])
        XCTAssertNil(live.last?.timestamp)
        XCTAssertFalse(live.contains(where: \.settled))
        let done = buildRows([.fixture("a1", parts: [text], status: .complete)])
        XCTAssertEqual(done.last?.timestamp, 1)
        XCTAssertNotEqual(live.last?.version, done.last?.version)
    }

    func testAPendingSendHasNoStrip() {
        let built = buildRows([], pending: [PendingSend(messageId: "p1", text: "Hi", at: 3)])
        XCTAssertNil(built.last?.timestamp)
        XCTAssertFalse(built.last?.settled ?? true)
    }

    func testAToolOnlyReplyHasNothingToCopy() {
        let call = RenderToolCall(tag: "exec", fields: ["command": "ls"])
        let built = buildRows([
            .fixture("a1", parts: [.tool(id: "c1", call: call, isError: false, resolved: true)])
        ])
        XCTAssertEqual(built.last?.timestamp, 1)
        XCTAssertEqual(built.last?.copyable, false)
    }

    func testTheAnsweringModelsDecodeJoinAndLabelTheStrip() throws {
        let json: [String: Any] = [
            "messages": [
                [
                    "id": "a1", "role": "assistant", "status": "complete", "createdAt": 4,
                    "parts": [["id": "t0", "kind": "text", "text": "One"]],
                    "models": [["model": "claude-opus-5-5", "requested": "claude-opus-5-5-20261001"]],
                ],
                [
                    "id": "a1c", "role": "assistant", "status": "complete", "createdAt": 5,
                    "continuationOf": "a1",
                    "parts": [["id": "t1", "kind": "text", "text": "Two"]],
                    "models": [
                        ["model": "claude-opus-5-5", "requested": "claude-opus-5-5-20261001"],
                        ["model": "gpt-5", "requested": "claude-opus-5-5"],
                    ],
                ],
            ]
        ]
        let root = try XCTUnwrap(LoroValue.fromJSON(json).mapValue)
        let entries = SessionStore.decodeEntries(root: root)
        XCTAssertEqual(entries.count, 1)
        XCTAssertEqual(entries[0].models.map(\.model), ["claude-opus-5-5", "gpt-5"])

        let label = try XCTUnwrap(buildRows(entries).last?.answered)
        XCTAssertEqual(label.text, "claude-opus-5-5, gpt-5")
        // A dated snapshot of the requested model is the same model.
        XCTAssertEqual(label.substituted, "Requested claude-opus-5-5, answered by gpt-5")
    }

    func testAFoldedTailHandsItsStripToTheToggle() {
        let entry = MessageEntry.fixture(
            "a1",
            parts: [.text(id: "t0", text: "Partial"), .reasoning(id: "r1", text: "Then…")],
            status: .aborted)
        var withModel = entry
        withModel.models = [AnsweredModel(model: "m")]
        let folded = TranscriptRowBuilder.foldClosedToggles(buildRows([withModel]), pins: [:])
        XCTAssertEqual(folded.last?.id, "a1#r1.thought")
        XCTAssertEqual(folded.last?.answered?.text, "m")
        XCTAssertEqual(folded.last?.copyable, true)
        let unlabeled = TranscriptRowBuilder.foldClosedToggles(buildRows([entry]), pins: [:])
        XCTAssertNotEqual(folded.last?.version, unlabeled.last?.version)
    }

    func testModelFamiliesIgnoreSpellingsOfTheSameModel() {
        let same = [
            ("anthropic/Claude-Opus-5-5", "claude-opus-5-5"),
            ("claude-opus-5-5[1m]", "claude-opus-5-5"),
            ("gpt-5-2026-09-01", "gpt-5"),
            ("claude-opus-5-20261001", "claude-opus-5"),
            ("claude-opus-5@20261001", "claude-opus-5"),
            ("gpt-5-latest", "gpt-5"),
        ]
        for (requested, model) in same {
            XCTAssertFalse(AnsweredModel(model: model, requested: requested).substituted, requested)
        }
        XCTAssertTrue(AnsweredModel(model: "gpt-5", requested: "claude-opus-5-5").substituted)
        XCTAssertFalse(AnsweredModel(model: "gpt-5", requested: nil).substituted)
        XCTAssertNil(AnsweredLabel([]))
    }

    func testCopyKeepsMarkdownAndSkipsToolInternals() {
        let call = RenderToolCall(tag: "exec", fields: ["command": "ls"])
        let reply = MessageEntry.fixture(
            "a1",
            parts: [
                .text(id: "t0", text: "# Plan\n\n- **one**"),
                .tool(id: "c1", call: call, isError: false, resolved: true),
                .reasoning(id: "r", text: "hidden"),
                .text(id: "t1", text: "  "),
                .error(id: "e", message: "boom"),
            ])
        XCTAssertEqual(MessageCopy.text(reply), "# Plan\n\n- **one**\n\nboom")
        let toolOnly = MessageEntry.fixture(
            "a2", parts: [.tool(id: "c", call: call, isError: false, resolved: true)])
        XCTAssertNil(MessageCopy.text(toolOnly))
    }

    func testCopyingAPromptMatchesItsBubble() {
        let link = Mentions.fileLink(path: "src/main.rs", isDir: false)
        let raw = "Look at \(link)\n\nAttached images (local files):\n- /tmp/a.png"
        let prompt = MessageEntry.fixture("u1", role: .user, parts: [.text(id: "t", text: raw)])
        let copied = MessageCopy.text(prompt)
        XCTAssertEqual(copied, Mentions.project("Look at \(link)").display)
        XCTAssertFalse(copied?.contains("cypher-file:") ?? true)
        XCTAssertFalse(copied?.contains("/tmp/a.png") ?? true)
    }

    func testTheTimestampReadsLikeTheDesktops() throws {
        let utc = try XCTUnwrap(TimeZone(identifier: "UTC"))
        // 2026-07-01 15:45 UTC.
        XCTAssertEqual(MessageStripFormat.timestamp(1_782_920_700_000, in: utc), "Jul 1, 3:45 PM")
        // 2026-10-09 09:05 UTC: no leading zero on the hour, minutes padded.
        XCTAssertEqual(MessageStripFormat.timestamp(1_791_536_700_000, in: utc), "Oct 9, 9:05 AM")
    }

    func testTheRewindReplyDecodes() throws {
        let rewound = Data(
            #"{"kind":"rewound","chat":{"id":"c"},"mode":"editUser","composerText":"Again","removedMessageIds":["u2","a2","a2c"]}"#
                .utf8)
        XCTAssertEqual(
            try JSONDecoder().decode(RewindResponse.self, from: rewound),
            .rewound(composerText: "Again", removed: 3))
        let refused = Data(#"{"kind":"unavailable","reason":"liveSession","message":"Still running."}"#.utf8)
        XCTAssertEqual(
            try JSONDecoder().decode(RewindResponse.self, from: refused),
            .unavailable(message: "Still running."))
    }
}

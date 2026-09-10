import XCTest
@testable import Cypher

final class QuestionPanelTests: XCTestCase {
    private func question(_ prompt: String = "Which source?", header: String? = nil,
                          options: [String] = ["Remote", "Local"],
                          multi: Bool = false) -> UserInputQuestion {
        UserInputQuestion(id: "q1", header: header ?? prompt, question: prompt,
                          options: options, multiSelect: multi)
    }

    func testDuplicateDialogTitleAndContextArePresentedOnce() {
        let raw = "Which source?\n\nContext:\n设备各有自己的 catalog。\nKeep **this** detail."
        let wire = question(raw)
        let presentation = QuestionPresentation(wire)
        XCTAssertEqual(presentation.header, "Your input")
        XCTAssertEqual(presentation.prompt, "Which source?")
        XCTAssertEqual(presentation.context, "设备各有自己的 catalog。\nKeep **this** detail.")
        XCTAssertEqual(wire.header, raw)
        XCTAssertEqual(wire.question, raw, "Presentation must not mutate the protocol payload")
    }

    func testShortNamedHeaderIsPreservedButDuplicateHeadingIsNot() {
        XCTAssertEqual(QuestionPresentation(question()).header, "Your input")
        XCTAssertEqual(QuestionPresentation(question(header: "Catalog source")).header, "Catalog source")
        XCTAssertNil(QuestionPresentation(question()).context)
    }

    func testContextKeepsNestedMarkersAndDoesNotSwallowOtherSections() {
        let context = "Read this literally.\n\nContext:\nA quoted subsection.\n\nSelected option:\nNot a comment stage."
        let presentation = QuestionPresentation(question("Choose?\n\nContext:\n\(context)"))
        XCTAssertEqual(presentation.context, context)
        XCTAssertNil(presentation.selection)
        XCTAssertFalse(presentation.isOptionalComment)
    }

    func testOptionalCommentSeparatesTheSelectionFromContext() {
        let presentation = QuestionPresentation(question(
            "Choose?\n\nContext:\nOriginal details.\n\nSelected options:\n- Remote\n- Local",
            header: "Optional comment", options: []))
        XCTAssertEqual(presentation.header, "Optional comment")
        XCTAssertEqual(presentation.prompt, "Choose?")
        XCTAssertEqual(presentation.context, "Original details.")
        XCTAssertEqual(presentation.selection, "- Remote\n- Local")
        XCTAssertTrue(presentation.isOptionalComment)
        XCTAssertFalse(QuestionPresentation(question(header: "Optional comment")).isOptionalComment)
    }

    func testSingleSelectCanBeChangedBeforeExplicitConfirmation() {
        let q = question()
        var draft = QuestionAnswerDraft()
        XCTAssertFalse(draft.hasAnswer(for: q))
        draft.select("Remote", for: q)
        XCTAssertTrue(draft.hasAnswer(for: q))
        draft.select("Local", for: q)
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["Local"])
    }

    func testMultiSelectRepliesInWireOrderAndSupportsDeselecting() {
        let q = question(multi: true)
        var draft = QuestionAnswerDraft()
        draft.select("Local", for: q)
        draft.select("Remote", for: q)
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["Remote", "Local"])
        draft.select("Remote", for: q)
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["Local"])
    }

    func testCustomDraftRejectsWhitespaceAndOptionsClearIt() {
        let q = question()
        var draft = QuestionAnswerDraft()
        draft.typed[q.id] = " \n "
        XCTAssertFalse(draft.hasAnswer(for: q))
        draft.typed[q.id] = "  自定义 answer  "
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["自定义 answer"])
        draft.select("Local", for: q)
        XCTAssertNil(draft.typed[q.id])
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["Local"])
    }

    func testDialogSentinelAndOptionLabelsAreNeverRewrittenInAnswers() {
        let sentinel = QuestionPresentation.customAnswerOption
        let q = question(options: ["Remote (Recommended)", sentinel])
        var draft = QuestionAnswerDraft()
        draft.select(sentinel, for: q)
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, [sentinel])
        draft.select("Remote (Recommended)", for: q)
        XCTAssertEqual(draft.answers(for: [q]).first?.labels, ["Remote (Recommended)"])
    }

    func testMultiplePagesKeepSeparateAnswersAndEmptyCommentCanBeSkipped() {
        let first = question()
        var second = question("Add anything?", header: "Optional comment", options: [])
        second.id = "q2"
        var draft = QuestionAnswerDraft()
        draft.select("Remote", for: first)
        let answers = draft.answers(for: [first, second])
        XCTAssertEqual(answers.map(\.questionId), ["q1", "q2"])
        XCTAssertEqual(answers.map(\.labels), [["Remote"], []])
    }

    func testTranscriptChipDoesNotRepeatContextAndResolutionChangesVersion() throws {
        let q = question("Choose?\n\nContext:\nLong private background")
        func row(resolved: Bool) throws -> TranscriptRow {
            let entry = MessageEntry(id: "entry", role: .assistant,
                parts: [.input(id: "input", requestId: "request", questions: [q], resolved: resolved)],
                createdAt: 0, deviceId: "device", status: .complete)
            var parsers: [String: IncrementalMarkdownParser] = [:]
            var completed: [String: CompletedParse] = [:]
            return try XCTUnwrap(TranscriptRowBuilder.rows(entries: [entry], pendingSends: [],
                parsers: &parsers, completed: &completed).first)
        }
        let pending = try row(resolved: false)
        let resolved = try row(resolved: true)
        if case .inputChip(let header, _) = resolved.kind {
            XCTAssertEqual(header, "Your input")
        } else {
            XCTFail("Expected question chip")
        }
        XCTAssertNotEqual(pending.version, resolved.version)
        XCTAssertEqual(pending.id, resolved.id)
    }
}

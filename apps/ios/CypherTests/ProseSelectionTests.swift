import XCTest
import UIKit
@testable import Cypher

/// Consecutive prose blocks render as one selectable text, so a drag
/// selection can cross paragraphs, headings and list items; code blocks,
/// tables, quotes and rules keep views of their own.
@MainActor
final class ProseSelectionTests: XCTestCase {
    private func rows(_ text: String, status: MessageStatus = .complete) -> [TranscriptRow] {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        let entry = MessageEntry(id: "m", role: .assistant, parts: [.text(id: "t", text: text)],
                                 createdAt: 1, deviceId: "d", status: status, continuationOf: nil)
        return TranscriptRowBuilder.rows(entries: [entry], pendingSends: [],
                                         parsers: &parsers, completed: &completed)
    }

    private func kinds(_ rows: [TranscriptRow]) -> [String] {
        rows.map { row in
            switch row.kind {
            case .prose(let blocks, let streaming): return "prose\(blocks.count)\(streaming ? " live" : "")"
            case .markdown(let block, _):
                if case .codeBlock = block { return "code" }
                if case .blockquote = block { return "quote" }
                return "block"
            default: return "other"
            }
        }
    }

    func testProseBlocksShareARowAndCodeQuotesBreakIt() {
        let reply = """
        # Plan

        First paragraph.

        - one
        - two

        ```swift
        let x = 1
        ```

        After the code.

        > A quote.

        Last paragraph.
        """
        let rows = rows(reply)
        XCTAssertEqual(kinds(rows), ["prose3", "code", "prose1", "quote", "prose1"])
        // Named by their first block, so a run keeps its id as blocks join it.
        XCTAssertEqual(rows.map(\.id), ["m#t.0", "m#t.3", "m#t.4", "m#t.5", "m#t.6"])
        XCTAssertNotNil(rows.last?.timestamp, "the settled reply's time rides its last row")
    }

    func testTheStreamingTailKeepsItsOwnRowUntilTheReplySettles() {
        let text = "One.\n\nTwo.\n\nThree is still arriving"
        XCTAssertEqual(kinds(rows(text, status: .streaming)), ["prose2", "prose1 live"])
        XCTAssertEqual(kinds(rows(text)), ["prose3"])
        XCTAssertEqual(rows(text).first?.id, rows(text, status: .streaming).first?.id)
    }

    func testListsWithCodeKeepTheirOwnView() {
        let list = MDBlock.list(orderedStart: nil, items: [
            MDListItem(checked: nil, children: [.paragraph([InlineRun(text: "a", style: .plain)])]),
            MDListItem(checked: nil, children: [.codeBlock(language: nil, code: "x")]),
        ])
        XCTAssertFalse(TranscriptTextStyle.isProse(list))
        XCTAssertFalse(TranscriptTextStyle.isProse(.rule))
        XCTAssertFalse(TranscriptTextStyle.isProse(.table(header: [], rows: [], align: [])))
    }

    func testProseReadsAsOneTextWithMarkersAndHangingIndents() throws {
        let blocks = MarkdownParser.parse("""
        ## Steps

        Do this:

        1. First
        2. Second
           - nested
        - [x] done
        """).map(\.block)
        let text = TranscriptTextStyle.prose(blocks)
        XCTAssertEqual(text.string, "Steps\nDo this:\n1.\tFirst\n2.\tSecond\n\u{2022}\tnested\n\u{2611}\tdone")

        func style(at needle: String) throws -> NSParagraphStyle {
            let location = (text.string as NSString).range(of: needle).location
            XCTAssertNotEqual(location, NSNotFound, needle)
            return try XCTUnwrap(text.attribute(.paragraphStyle, at: location, effectiveRange: nil) as? NSParagraphStyle)
        }
        // The first paragraph sits at the top; the next block keeps the gap.
        XCTAssertEqual(try style(at: "Steps").paragraphSpacingBefore, 0)
        XCTAssertEqual(try style(at: "Steps").minimumLineHeight, MD.headingMetrics(2).line)
        XCTAssertEqual(try style(at: "Do this").paragraphSpacingBefore, MD.blockGap)
        // A list item: marker at the list's edge, wrapped lines on its text.
        let first = try style(at: "1.\tFirst")
        XCTAssertEqual(first.firstLineHeadIndent, 0)
        XCTAssertEqual(first.headIndent, TranscriptTextStyle.listIndent)
        XCTAssertEqual(try style(at: "2.\tSecond").paragraphSpacingBefore, TranscriptTextStyle.listItemGap)
        // Nested one level in.
        let nested = try style(at: "\u{2022}\tnested")
        XCTAssertEqual(nested.firstLineHeadIndent, TranscriptTextStyle.listIndent)
        XCTAssertEqual(nested.headIndent, 2 * TranscriptTextStyle.listIndent)
    }

    func testOneTextViewHoldsTheWholeRun() {
        // Copying across the run gives every paragraph and item, in order.
        let blocks = MarkdownParser.parse("First.\n\nSecond with `code`.\n\n- item").map(\.block)
        let view = UITextView()
        view.attributedText = TranscriptTextStyle.prose(blocks)
        view.selectedRange = NSRange(location: 0, length: view.attributedText.length)
        XCTAssertEqual(CommentPrompt.selectedText(view.text, range: view.selectedRange),
                       "First.\nSecond with code.\n\u{2022}\titem")
    }
}

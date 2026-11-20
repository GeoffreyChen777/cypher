import XCTest
import SwiftUI
import UIKit
@testable import Cypher

/// Transcript text is measured off screen and remembered per text and width:
/// laying every long reply out again each time the lazy transcript measured
/// it held the main thread for 10 s, and the system killed the app (watchdog
/// 0x8BADF00D on a scene update, iOS 0.4.0 (44)).
@MainActor
final class TranscriptTextMeasureTests: XCTestCase {
    /// A text view set up as `SelectableTranscriptText.makeUIView` sets one up.
    private func textView(_ text: NSAttributedString) -> TranscriptUITextView {
        let storage = NSTextStorage()
        let layout = TranscriptTextLayoutManager()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: .zero)
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        let view = TranscriptUITextView(frame: .zero, textContainer: container)
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.textContainerInset = .zero
        view.attributedText = text
        return view
    }

    private func prose(_ markdown: String) -> NSAttributedString {
        TranscriptTextStyle.prose(MarkdownParser.parse(markdown).map(\.block))
    }

    private var fixtures: [(String, NSAttributedString)] {
        [
            ("empty", TranscriptTextStyle.inline([])),
            ("one line", TranscriptTextStyle.inline([InlineRun(text: "Done.", style: .plain)])),
            (
                "long paragraph",
                TranscriptTextStyle.inline([
                    InlineRun(
                        text: String(repeating: "A paragraph that wraps across lines. ", count: 30), style: .plain)
                ])
            ),
            (
                "headings, lists, code, links",
                prose(
                    """
                    # Plan

                    First paragraph with `inline code`, **bold**, *italic* and [a link](https://example.com).

                    ## Steps

                    - one
                    - two, long enough to wrap onto a second line in a narrow column of text
                      - nested item
                    1. first
                    2. second

                    - [x] done
                    - [ ] todo
                    """)
            ),
            ("file chips", prose("See `apps/ios/Cypher/Transcript/TranscriptView.swift:42` and `file.py:1-3` here.")),
            (
                "math",
                prose(
                    #"""
                    Inline $x^2 + y_1$ in prose, then a display formula

                    $$\int_0^1 f(x)\,dx = \sum_{n=0}^{\infty} a_n \cdot \frac{1}{n!}$$

                    and prose after it.
                    """#)
            ),
            (
                "CJK and emoji",
                TranscriptTextStyle.inline([
                    InlineRun(
                        text: String(repeating: "中文排版测试，看看换行是否一致。👩🏽‍💻 emoji ", count: 12), style: .plain)
                ])
            ),
            ("trailing newline", TranscriptTextStyle.inline([InlineRun(text: "a line\n", style: .plain)])),
            (
                "code",
                TranscriptTextStyle.code(
                    (1...40).map { "let value\($0) = compute(\($0)) // a comment on line \($0)" }.joined(
                        separator: "\n"),
                    spans: [])
            ),
        ]
    }

    func testOffScreenHeightsMatchWhatTheTextViewLaysOut() {
        for (name, text) in fixtures {
            let view = textView(text)
            // A fresh view's first measurement leaves out a trailing
            // newline's empty line (36 for 44 at 22 a line); every later one,
            // and the view's own layout, has it. Compare with those.
            _ = view.sizeThatFits(CGSize(width: 100, height: CGFloat.greatestFiniteMagnitude))
            for width: CGFloat in [80, 160, 240, 343, 736, 4_000] {
                let expected = ceil(
                    view.sizeThatFits(CGSize(width: width, height: CGFloat.greatestFiniteMagnitude)).height)
                let measured = ceil(TranscriptTextMeasurer.shared.height(of: text, width: width))
                XCTAssertEqual(measured, expected, "\(name) at \(width)")
            }
        }
    }

    func testARowIsLaidOutOncePerTextAndWidth() {
        let coordinator = TranscriptSelectionCoordinator()
        let text = TranscriptTextStyle.inline([
            InlineRun(text: String(repeating: "A reply long enough to wrap. ", count: 20), style: .plain)
        ])
        let view = textView(NSAttributedString())
        coordinator.update(view, attributed: text, drafts: nil)
        let measurer = TranscriptTextMeasurer.shared
        let start = measurer.layouts

        let first = coordinator.size(of: view, proposedWidth: 343, wraps: true, hugsContent: false)
        for _ in 0..<50 {
            XCTAssertEqual(coordinator.size(of: view, proposedWidth: 343, wraps: true, hugsContent: false), first)
        }
        XCTAssertEqual(measurer.layouts - start, 1, "the same text at the same width is measured once")
        XCTAssertEqual(first.width, 343)
        XCTAssertEqual(
            first.height, ceil(view.sizeThatFits(CGSize(width: 343, height: CGFloat.greatestFiniteMagnitude)).height))

        // A re-render builds an equal text: the view keeps it, and its sizes.
        let rebuilt = TranscriptTextStyle.inline([
            InlineRun(text: String(repeating: "A reply long enough to wrap. ", count: 20), style: .plain)
        ])
        coordinator.update(view, attributed: rebuilt, drafts: nil)
        _ = coordinator.size(of: view, proposedWidth: 343, wraps: true, hugsContent: false)
        XCTAssertEqual(measurer.layouts - start, 1)

        // Another width is measured once too.
        let narrow = coordinator.size(of: view, proposedWidth: 200, wraps: true, hugsContent: false)
        _ = coordinator.size(of: view, proposedWidth: 200, wraps: true, hugsContent: false)
        XCTAssertEqual(measurer.layouts - start, 2)
        XCTAssertGreaterThan(narrow.height, first.height)

        // New text drops what was measured.
        coordinator.update(
            view, attributed: TranscriptTextStyle.inline([InlineRun(text: "Short.", style: .plain)]), drafts: nil)
        let short = coordinator.size(of: view, proposedWidth: 343, wraps: true, hugsContent: false)
        XCTAssertEqual(measurer.layouts - start, 3)
        XCTAssertEqual(short.height, MD.lineHeight)
    }

    func testASelectedBlockIsMeasuredByTheTextItShows() {
        let coordinator = TranscriptSelectionCoordinator()
        let view = textView(NSAttributedString())
        view.delegate = coordinator
        let short = TranscriptTextStyle.inline([InlineRun(text: "hello", style: .plain)])
        coordinator.update(view, attributed: short, drafts: nil)
        let before = coordinator.size(of: view, proposedWidth: 240, wraps: true, hugsContent: false)
        view.selectedRange = NSRange(location: 0, length: 5)
        let long = TranscriptTextStyle.inline([
            InlineRun(text: "hello " + String(repeating: "and a lot more streamed text ", count: 20), style: .plain)
        ])
        coordinator.update(view, attributed: long, drafts: nil)
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: 240, wraps: true, hugsContent: false), before)
        view.selectedRange = NSRange(location: 0, length: 0)
        coordinator.textViewDidChangeSelection(view)
        XCTAssertGreaterThan(
            coordinator.size(of: view, proposedWidth: 240, wraps: true, hugsContent: false).height, before.height)
    }

    func testUnwrappedAndHuggingTextTakeTheirOneLineWidth() {
        let coordinator = TranscriptSelectionCoordinator()
        let view = textView(NSAttributedString())
        let text = TranscriptTextStyle.inline([InlineRun(text: "A short cell", style: .plain)])
        coordinator.update(view, attributed: text, drafts: nil)
        let natural =
            ceil(
                text.boundingRect(
                    with: CGSize(width: 1_000_000, height: 1_000_000),
                    options: [.usesLineFragmentOrigin, .usesFontLeading], context: nil
                ).width) + 1
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: 40, wraps: false, hugsContent: true).width, natural)
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: 600, wraps: true, hugsContent: true).width, natural)
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: 40, wraps: true, hugsContent: true).width, 40)
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: nil, wraps: true, hugsContent: false).width, natural)
        XCTAssertEqual(coordinator.size(of: view, proposedWidth: 600, wraps: true, hugsContent: false).width, 600)
        XCTAssertEqual(
            coordinator.size(of: view, proposedWidth: 600, wraps: true, hugsContent: false).height, MD.lineHeight)
    }

    func testHostedTextIsSizedToShowAllItsLines() async throws {
        let attributed = prose(
            """
            # Heading

            \(String(repeating: "A paragraph with selectable text. ", count: 12))

            - one
            - two
            """)
        let view = SelectableTranscriptText(attributed: attributed)
        let hosted = try HostedWindow(view.frame(width: 240), frame: CGRect(x: 0, y: 0, width: 300, height: 900))
        defer { hosted.close() }
        var found: TranscriptUITextView?
        try await waitUntil("the transcript text view") {
            hosted.host.view.layoutIfNeeded()
            found = firstSubview(TranscriptUITextView.self, in: hosted.host.view)
            return found != nil
        }
        let text = try XCTUnwrap(found)
        XCTAssertEqual(text.frame.width, 240)
        XCTAssertEqual(
            text.frame.height,
            ceil(text.sizeThatFits(CGSize(width: 240, height: CGFloat.greatestFiniteMagnitude)).height))
    }
}

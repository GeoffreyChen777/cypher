import XCTest
import SwiftUI
import UIKit
@testable import Cypher

@MainActor
final class InlineSelectionTests: XCTestCase {
    private func perform(_ action: UIAction) {
        let button = UIButton(type: .system)
        button.addAction(action, for: .touchUpInside)
        button.sendActions(for: .touchUpInside)
    }

    func testNativeMenuCommentsOnlyTheSelectedUTF16Range() throws {
        let drafts = CommentDrafts()
        drafts.bind(to: "chat")
        let coordinator = TranscriptSelectionCoordinator()
        let textView = UITextView()
        let source = "Before 👩🏽‍💻 selected 中文 after"
        coordinator.update(textView, attributed: NSAttributedString(string: source), drafts: drafts)
        let range = (source as NSString).range(of: "👩🏽‍💻 selected 中文")
        let copy = UIAction(title: "Copy") { _ in }
        let menu = try XCTUnwrap(coordinator.textView(textView, editMenuForTextIn: range, suggestedActions: [copy]))
        XCTAssertEqual(menu.children.map(\.title), ["Comment", "Copy"])
        perform(try XCTUnwrap(menu.children.first as? UIAction))
        XCTAssertTrue(drafts.presented)
        XCTAssertEqual(drafts.editor?.text, "👩🏽‍💻 selected 中文")
        XCTAssertTrue(drafts.comments.isEmpty, "Opening Comment must not save or send anything")
    }

    func testEmptySelectionAndNonCommentableSurfaceDoNotOfferComment() {
        let coordinator = TranscriptSelectionCoordinator()
        let textView = UITextView()
        coordinator.update(textView, attributed: NSAttributedString(string: "text"), drafts: nil)
        XCTAssertNil(coordinator.commentAction(in: textView, range: NSRange(location: 0, length: 4)))
        let drafts = CommentDrafts()
        drafts.bind(to: "chat")
        coordinator.update(textView, attributed: NSAttributedString(string: "text"), drafts: drafts)
        XCTAssertNil(coordinator.commentAction(in: textView, range: NSRange(location: 0, length: 0)))
    }

    func testOldSelectionMenuCannotQuoteIntoAnotherChat() throws {
        let drafts = CommentDrafts()
        drafts.bind(to: "a")
        let coordinator = TranscriptSelectionCoordinator()
        let textView = UITextView()
        coordinator.update(textView, attributed: NSAttributedString(string: "private a"), drafts: drafts)
        let action = try XCTUnwrap(coordinator.commentAction(in: textView, range: NSRange(location: 0, length: 9)))
        drafts.bind(to: "b")
        perform(action)
        XCTAssertFalse(drafts.presented)
        XCTAssertNil(drafts.editor)
    }

    func testStreamingUpdatesFreezeSelectedBlockThenCatchUp() {
        let coordinator = TranscriptSelectionCoordinator()
        let textView = UITextView()
        textView.delegate = coordinator
        coordinator.update(textView, attributed: NSAttributedString(string: "hello"), drafts: nil)
        textView.selectedRange = NSRange(location: 0, length: 5)
        coordinator.update(textView, attributed: NSAttributedString(string: "hello world"), drafts: nil)
        XCTAssertEqual(textView.text, "hello")
        XCTAssertEqual(textView.selectedRange.length, 5)
        textView.selectedRange = NSRange(location: 0, length: 0)
        coordinator.textViewDidChangeSelection(textView)
        XCTAssertEqual(textView.text, "hello world")
    }

    func testNativeRichTextPreservesFontsLinksAndRoundedCodeMarker() throws {
        var code = InlineStyle.plain
        code.code = true
        var link = InlineStyle.plain
        link.link = "https://example.com"
        let attributed = TranscriptTextStyle.inline([
            InlineRun(text: "Text ", style: .plain),
            InlineRun(text: "code", style: code),
            InlineRun(text: " link", style: link),
        ])
        XCTAssertEqual(attributed.string, "Text code link")
        XCTAssertEqual((attributed.attribute(.font, at: 0, effectiveRange: nil) as? UIFont)?.pointSize, MD.textSize)
        XCTAssertNotNil(attributed.attribute(.cypherInlineCode, at: 5, effectiveRange: nil))
        XCTAssertEqual((attributed.attribute(.font, at: 5, effectiveRange: nil) as? UIFont)?.pointSize, MD.textSize - 1.5)
        XCTAssertEqual(attributed.attribute(.link, at: 10, effectiveRange: nil) as? URL, URL(string: "https://example.com"))
        XCTAssertEqual((attributed.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle)?.maximumLineHeight, MD.lineHeight)
    }

    func testCodeSelectionPreservesNewlinesAndSyntaxColors() {
        let source = "let x = 1\nprint(x)"
        let attributed = TranscriptTextStyle.code(source, spans: [[TokenSpan(range: 0..<3, cls: .keyword)]])
        XCTAssertEqual(attributed.string, source)
        XCTAssertEqual(attributed.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? UIColor,
                       UIColor(Theme.tokenKeyword))
        XCTAssertEqual((attributed.attribute(.font, at: 0, effectiveRange: nil) as? UIFont)?.pointSize, MD.codeTextSize)
        XCTAssertEqual(CommentPrompt.selectedText(source, range: NSRange(location: 8, length: 9)), "1\nprint(x")
    }

    func testNativeTextLaysOutInsideSwiftUIWithoutReplacingTheScrollView() async throws {
        let view = SelectableTranscriptText(attributed: TranscriptTextStyle.inline([
            InlineRun(text: String(repeating: "A paragraph with selectable text. ", count: 8), style: .plain),
        ]))
        let host = UIHostingController(rootView: view.frame(width: 240))
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let previousKeyWindow = scene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 300, height: 600)
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previousKeyWindow?.makeKey()
        }
        func findText(_ view: UIView) -> TranscriptUITextView? {
            if let text = view as? TranscriptUITextView { return text }
            for child in view.subviews {
                if let found = findText(child) { return found }
            }
            return nil
        }
        for _ in 0..<20 {
            host.view.layoutIfNeeded()
            if findText(host.view) != nil { break }
            try await Task.sleep(for: .milliseconds(20))
        }
        let text = try XCTUnwrap(findText(host.view))
        XCTAssertFalse(text.isEditable)
        XCTAssertTrue(text.isSelectable)
        XCTAssertFalse(text.isScrollEnabled, "The transcript owns vertical scrolling")
        XCTAssertEqual(text.textContainer.lineFragmentPadding, 0)
        XCTAssertTrue(text.layoutManager is TranscriptTextLayoutManager)
        let size = text.sizeThatFits(CGSize(width: 240, height: 100_000))
        XCTAssertGreaterThan(size.height, MD.lineHeight)
        XCTAssertLessThan(size.height, 600)
    }
}

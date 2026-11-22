import SwiftUI
import UIKit
import XCTest

@testable import Cypher

/// Inline code is set by the same rules as the desktop's: code type at the
/// one code scale, in the body ink on a neutral wash, a box that never
/// changes the line height; syntax colors stay in code blocks.
@MainActor
final class InlineCodeTests: XCTestCase {
    private let code = InlineStyle(code: true)

    private func runs() -> [InlineRun] {
        [
            InlineRun(text: "Run ", style: .plain),
            InlineRun(text: "cargo test", style: code),
            InlineRun(text: " again.", style: .plain),
        ]
    }

    private func layOut(
        _ text: NSAttributedString, width: CGFloat
    ) -> (NSTextStorage, NSLayoutManager, NSTextContainer) {
        let storage = NSTextStorage(attributedString: text)
        let layout = TranscriptTextLayoutManager()
        let container = NSTextContainer(size: CGSize(width: width, height: .greatestFiniteMagnitude))
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
        layout.ensureLayout(for: container)
        return (storage, layout, container)
    }

    /// `color`'s grey level and alpha in `style` (fails for a hue).
    private func white(_ color: UIColor, _ style: UIUserInterfaceStyle) -> (white: CGFloat, alpha: CGFloat) {
        var r: CGFloat = -1
        var g: CGFloat = -1
        var b: CGFloat = -1
        var a: CGFloat = -1
        XCTAssertTrue(
            color.resolvedColor(with: UITraitCollection(userInterfaceStyle: style)).getRed(
                &r, green: &g, blue: &b, alpha: &a))
        XCTAssertEqual(r, g, accuracy: 0.0001)
        XCTAssertEqual(g, b, accuracy: 0.0001)
        return (r, a)
    }

    func testOneCodeScaleForInlineCodeAndFileReferences() throws {
        XCTAssertEqual(Theme.codeScale, 0.86)
        XCTAssertEqual(FileReferenceChip.nameEm, Theme.codeScale)
        for size in [MD.textSize, MD.headingMetrics(1).size] {
            let text = TranscriptTextStyle.inline(runs(), size: size)
            let at = (text.string as NSString).range(of: "cargo").location
            let font = try XCTUnwrap(text.attribute(.font, at: at, effectiveRange: nil) as? UIFont)
            XCTAssertEqual(font.pointSize, size * Theme.codeScale, accuracy: 0.001)
            XCTAssertTrue(font.fontName.hasPrefix("GeistMono"), font.fontName)
        }
        // Code blocks keep their own size.
        let block = TranscriptTextStyle.code("let x = 1", spans: [])
        XCTAssertEqual((block.attribute(.font, at: 0, effectiveRange: nil) as? UIFont)?.pointSize, MD.codeTextSize)
    }

    func testInlineCodeIsBodyInkOnANeutralWash() throws {
        XCTAssertEqual(UIColor(Theme.inlineCodeText), UIColor(Theme.text))
        let text = TranscriptTextStyle.inline(runs())
        let at = (text.string as NSString).range(of: "cargo").location
        let ink = try XCTUnwrap(text.attribute(.foregroundColor, at: at, effectiveRange: nil) as? UIColor)
        let prose = try XCTUnwrap(text.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? UIColor)
        let wash = try XCTUnwrap(text.attribute(.cypherInlineCode, at: at, effectiveRange: nil) as? UIColor)
        for (style, level, alpha) in [
            (UIUserInterfaceStyle.light, CGFloat(0), Theme.inlineCodeWashLight), (.dark, 1, Theme.inlineCodeWashDark),
        ] {
            let traits = UITraitCollection(userInterfaceStyle: style)
            XCTAssertEqual(ink.resolvedColor(with: traits), prose.resolvedColor(with: traits))
            let paint = white(wash, style)
            XCTAssertEqual(paint.white, level, accuracy: 0.0001)
            XCTAssertEqual(paint.alpha, alpha, accuracy: 0.0001)
        }
        XCTAssertEqual(Theme.inlineCodeWashLight, 0.06)
        XCTAssertEqual(Theme.inlineCodeWashDark, 0.08)
        XCTAssertEqual(MD.inlineCodeRadius, 5)
    }

    func testSyntaxColorsStayInCodeBlocks() throws {
        let text = TranscriptTextStyle.inline([InlineRun(text: "let x = \"a\" + 1", style: code)])
        let body = UIColor(Theme.text)
        text.enumerateAttribute(.foregroundColor, in: NSRange(location: 0, length: text.length)) { value, _, _ in
            XCTAssertEqual(value as? UIColor, body)
        }
        let block = TranscriptTextStyle.code("let x = 1", spans: [[TokenSpan(range: 0..<3, cls: .keyword)]])
        XCTAssertEqual(
            block.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? UIColor, UIColor(Theme.tokenKeyword))
    }

    func testThinkingMutesInlineCodeWithItsProse() throws {
        let text = TranscriptTextStyle.muted(TranscriptTextStyle.inline(runs()))
        let at = (text.string as NSString).range(of: "cargo").location
        XCTAssertEqual(
            text.attribute(.foregroundColor, at: at, effectiveRange: nil) as? UIColor, UIColor(Theme.textMuted))
        XCTAssertNotNil(text.attribute(.cypherInlineCode, at: at, effectiveRange: nil))
    }

    func testRoomBesideTheCodeIsKernedAndCopiesAsWritten() {
        let text = TranscriptTextStyle.inline(runs())
        XCTAssertEqual(text.string, "Run cargo test again.")
        let string = text.string as NSString
        let codeRange = string.range(of: "cargo test")
        XCTAssertEqual(
            text.attribute(.kern, at: codeRange.location - 1, effectiveRange: nil) as? CGFloat, InlineCodeBox.padX)
        XCTAssertEqual(
            text.attribute(.kern, at: NSMaxRange(codeRange) - 1, effectiveRange: nil) as? CGFloat, InlineCodeBox.padX)
        XCTAssertNil(text.attribute(.kern, at: codeRange.location, effectiveRange: nil))
        XCTAssertNil(MathRenderer.plainText(text, range: NSRange(location: 0, length: text.length)))
    }

    func testTheBoxIsCenteredInsideItsLineAndKeepsTheLineHeight() throws {
        let text = TranscriptTextStyle.inline(runs())
        let (storage, layout, container) = layOut(text, width: 1_000)
        let codeRange = (text.string as NSString).range(of: "cargo test")
        let boxes = InlineCodeBox.boxes(layout, storage: storage, container: container, code: codeRange)
        XCTAssertEqual(boxes.count, 1)
        let box = try XCTUnwrap(boxes.first)
        let font = try XCTUnwrap(storage.attribute(.font, at: codeRange.location, effectiveRange: nil) as? UIFont)
        XCTAssertEqual(box.height, (font.pointSize * InlineCodeBox.heightEm).rounded())
        XCTAssertEqual(box.height, 15)
        let line = layout.lineFragmentRect(forGlyphAt: 0, effectiveRange: nil)
        XCTAssertEqual(line.height, MD.lineHeight, "inline code never changes the line height")
        XCTAssertGreaterThanOrEqual(box.minY, line.minY)
        XCTAssertLessThanOrEqual(box.maxY, line.maxY)
        let baseline = line.minY + layout.location(forGlyphAt: layout.glyphIndexForCharacter(at: codeRange.location)).y
        XCTAssertEqual(box.midY, baseline - font.capHeight / 2, accuracy: 0.01)
        // padX past the glyphs either side, inside the kerned room.
        let first = layout.location(forGlyphAt: layout.glyphIndexForCharacter(at: codeRange.location)).x
        XCTAssertEqual(first - box.minX, InlineCodeBox.padX, accuracy: 0.01)
        let advance = NSAttributedString(string: "x", attributes: [.font: font]).size().width
        XCTAssertEqual(box.width, CGFloat(codeRange.length) * advance + 2 * InlineCodeBox.padX, accuracy: 0.01)
        let after = layout.location(forGlyphAt: layout.glyphIndexForCharacter(at: NSMaxRange(codeRange))).x
        XCTAssertEqual(after, box.maxX, accuracy: 0.01, "the box ends where the prose resumes")
    }

    func testCodeWrappingAtTheViewEdgeStaysInsideIt() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "Then ", style: .plain),
            InlineRun(text: "some_rather_long_identifier_that::wraps_over::several_lines_of_text", style: code),
            InlineRun(text: " here.", style: .plain),
        ])
        let width: CGFloat = 160
        let (storage, layout, container) = layOut(text, width: width)
        let codeRange = (text.string as NSString).range(of: "some_rather")
        var whole = NSRange()
        _ = storage.attribute(
            .cypherInlineCode, at: codeRange.location, longestEffectiveRange: &whole,
            in: NSRange(location: 0, length: storage.length))
        let boxes = InlineCodeBox.boxes(layout, storage: storage, container: container, code: whole)
        XCTAssertGreaterThan(boxes.count, 1)
        for box in boxes {
            XCTAssertGreaterThanOrEqual(box.minX, 0)
            XCTAssertLessThanOrEqual(box.maxX, width)
            XCTAssertEqual(box.height, 15)
        }
        XCTAssertEqual(Set(boxes.map(\.minY)).count, boxes.count, "one box per line")
    }
}

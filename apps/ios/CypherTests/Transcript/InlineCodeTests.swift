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

    /// The desktop's rules (render.rs `INLINE_CODE_PAD_X`, `code_box_band`):
    /// 4 pt past the glyphs, and the box centered on the code text — its
    /// ascent and descent, which for Geist Mono is its cap height's middle
    /// to within half a point.
    func testTheBoxMatchesTheDesktopsPaddingAndCenter() throws {
        XCTAssertEqual(InlineCodeBox.padX, 4)
        let font = Theme.monoUI(MD.textSize * Theme.codeScale)
        let desktop = (font.ascender + font.descender) / 2
        XCTAssertEqual(font.capHeight / 2, desktop, accuracy: 0.5)
    }

    /// Inline code and a file reference share one box: the same height and
    /// radius, centered at the same height on one line.
    func testInlineCodeAndChipsShareOneBox() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "Run ", style: .plain),
            InlineRun(text: "cargo test", style: code),
            InlineRun(text: " and read ", style: .plain),
            InlineRun(text: "src/render.rs:12", style: code),
            InlineRun(text: ".", style: .plain),
        ])
        let (storage, layout, container) = layOut(text, width: 1_000)
        let codeRange = (text.string as NSString).range(of: "cargo test")
        let codeBox = try XCTUnwrap(
            InlineCodeBox.boxes(layout, storage: storage, container: container, code: codeRange).first)
        var chip = NSRange()
        let at = (text.string as NSString).range(of: "render").location
        _ = storage.attribute(
            .cypherFileChip, at: at, longestEffectiveRange: &chip, in: NSRange(location: 0, length: storage.length))
        let chipBox = try XCTUnwrap(
            FileReferenceChip.boxes(layout, storage: storage, container: container, chip: chip).first)
        XCTAssertEqual(codeBox.height, 15)
        XCTAssertEqual(chipBox.height, codeBox.height)
        XCTAssertEqual(chipBox.minY, codeBox.minY, accuracy: 0.01)
        XCTAssertEqual(FileReferenceChip.radius, InlineCodeBox.radius)
    }

    /// Mention chips in a sent prompt keep their violet look; only inline
    /// code went neutral (the desktop keeps its mention chips' colors too).
    func testMentionChipsInAPromptKeepTheirViolet() throws {
        let raw = "Look at " + Mentions.fileLink(path: "src/render.rs", isDir: false) + " please"
        let runs = Mentions.inlineRuns(raw)
        let mention = try XCTUnwrap(runs.first { $0.style.mention })
        XCTAssertTrue(mention.style.code)
        let text = TranscriptTextStyle.inline(runs)
        let at = (text.string as NSString).range(of: "render.rs").location
        XCTAssertNotEqual(at, NSNotFound)
        XCTAssertEqual(
            text.attribute(.foregroundColor, at: at, effectiveRange: nil) as? UIColor, UIColor(Theme.mentionText))
        XCTAssertEqual(
            text.attribute(.cypherMentionWash, at: at, effectiveRange: nil) as? UIColor, UIColor(Theme.mentionWash))
        // Not inline code, not a file chip, no kerned room; the old size.
        XCTAssertNil(text.attribute(.cypherInlineCode, at: at, effectiveRange: nil))
        XCTAssertNil(text.attribute(.cypherFileChip, at: at, effectiveRange: nil))
        XCTAssertNil(text.attribute(.kern, at: at - 1, effectiveRange: nil))
        let font = try XCTUnwrap(text.attribute(.font, at: at, effectiveRange: nil) as? UIFont)
        XCTAssertEqual(font.pointSize, MD.textSize - 1.5, accuracy: 0.001)
        XCTAssertEqual(MD.mentionRadius, 4.5)
        // The violet the chips always had, not the body ink.
        for style in [UIUserInterfaceStyle.light, .dark] {
            let traits = UITraitCollection(userInterfaceStyle: style)
            let violet = UIColor(Theme.mentionText).resolvedColor(with: traits)
            XCTAssertNotEqual(violet, UIColor(Theme.text).resolvedColor(with: traits))
            var r: CGFloat = 0
            var g: CGFloat = 0
            var b: CGFloat = 0
            var a: CGFloat = 0
            XCTAssertTrue(violet.getRed(&r, green: &g, blue: &b, alpha: &a))
            XCTAssertGreaterThan(b, g + 0.1, "violet")
        }
    }

    /// Lays out a reply's prose blocks from Markdown, `width` wide.
    private func prose(_ source: String, width: CGFloat = 354) -> (NSTextStorage, NSLayoutManager, NSTextContainer) {
        layOut(TranscriptTextStyle.prose(MarkdownParser.parse(source).map(\.block)), width: width)
    }

    /// Code that ends a paragraph keeps its box to its glyphs: the break
    /// after it is prose, not code (it used to carry the code's wash, and
    /// the box ran to the view's edge).
    func testCodeEndingAParagraphKeepsItsBoxToItsGlyphs() throws {
        let (storage, layout, container) = prose("- Build with `just ios`\n- `InlineCode.swift`\n\nNext.")
        let string = storage.string as NSString
        for code in ["just ios", "InlineCode.swift"] {
            let range = string.range(of: code)
            var whole = NSRange()
            _ = storage.attribute(
                .cypherInlineCode, at: range.location, longestEffectiveRange: &whole,
                in: NSRange(location: 0, length: storage.length))
            XCTAssertEqual(whole, range, "the break is not code")
            XCTAssertEqual(string.character(at: NSMaxRange(range)), 0x0A)
            let font = try XCTUnwrap(storage.attribute(.font, at: range.location, effectiveRange: nil) as? UIFont)
            let advance = NSAttributedString(string: "x", attributes: [.font: font]).size().width
            let box = try XCTUnwrap(
                InlineCodeBox.boxes(layout, storage: storage, container: container, code: whole).first)
            XCTAssertEqual(
                box.width, CGFloat(range.length) * advance + 2 * InlineCodeBox.padX, accuracy: 0.01, code)
        }
    }

    /// Every line sits on the prose baseline, with code or without, and a
    /// line of only code too (desktop `strut_shift`); the boxes sit at one
    /// height in their lines.
    func testALineOfOnlyCodeSitsOnTheProseBaseline() throws {
        let (storage, layout, container) = prose(
            """
            Plain prose.

            `only_code_here`

            Run `cargo test` now.

            See `src/a_rather_long_generated_preview_fixture_name_that_goes_on.swift:120-148` here.
            """)
        var baselines: [CGFloat] = []
        layout.enumerateLineFragments(forGlyphRange: NSRange(location: 0, length: layout.numberOfGlyphs)) {
            rect, _, _, glyphs, _ in
            // From the line's bottom: paragraph spacing sits above the text.
            baselines.append(rect.height - layout.location(forGlyphAt: glyphs.location).y)
        }
        XCTAssertEqual(baselines.count, 6, "the chip starts a line of its own and wraps")
        for baseline in baselines { XCTAssertEqual(baseline, baselines[0], accuracy: 0.01) }
        let string = storage.string as NSString
        let all = NSRange(location: 0, length: storage.length)
        var offsets: [CGFloat] = []
        for code in ["only_code_here", "cargo test"] {
            let range = string.range(of: code)
            let line = layout.lineFragmentRect(
                forGlyphAt: layout.glyphIndexForCharacter(at: range.location), effectiveRange: nil)
            let box = try XCTUnwrap(
                InlineCodeBox.boxes(layout, storage: storage, container: container, code: range).first)
            offsets.append(line.maxY - box.minY)
        }
        var chip = NSRange()
        _ = storage.attribute(
            .cypherFileChip, at: string.range(of: "a_rather").location, longestEffectiveRange: &chip, in: all)
        let chipBoxes = FileReferenceChip.boxes(layout, storage: storage, container: container, chip: chip)
        XCTAssertEqual(chipBoxes.count, 2)
        for box in chipBoxes {
            let glyph = layout.glyphIndex(for: CGPoint(x: box.midX, y: box.midY), in: container)
            offsets.append(layout.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil).maxY - box.minY)
        }
        XCTAssertEqual(offsets.count, 4)
        for offset in offsets { XCTAssertEqual(offset, offsets[0], accuracy: 0.01) }
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

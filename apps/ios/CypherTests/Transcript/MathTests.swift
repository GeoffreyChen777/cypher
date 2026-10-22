import XCTest
import UIKit
@testable import Cypher

/// LaTeX in chat Markdown: `$…$`/`\(…\)` inline and `$$…$$`/`\[…\]` display,
/// read the way the desktop reads them and typeset into the text.
@MainActor
final class MathTests: XCTestCase {
    private func math(_ tex: String, _ kind: MathKind) -> InlineRun {
        var style = InlineStyle.plain
        style.math = kind
        return InlineRun(text: tex, style: style)
    }

    private func prose(_ text: String) -> InlineRun {
        InlineRun(text: text, style: .plain)
    }

    private func blocks(_ source: String) -> [MDBlock] {
        MarkdownParser.parse(source).map(\.block)
    }

    // MARK: Parsing

    func testInlineFormulasBecomeTheirOwnRuns() {
        XCTAssertEqual(
            blocks("where $x_1$ and $y$ hold"),
            [.paragraph([prose("where "), math("x_1", .inline), prose(" and "), math("y", .inline), prose(" hold")])])
        // Adjacent formulas never merge into one.
        XCTAssertEqual(blocks("$a$$b$"), [.paragraph([math("a", .inline), math("b", .inline)])])
    }

    func testTeXSurvivesMarkdownUntouched() {
        // Rows (`\\`), subscripts and stars would all be Markdown to cmark.
        let tex = #"\begin{pmatrix} a_1 & *b* \\ c_2 & d \end{pmatrix}"#
        XCTAssertEqual(blocks("$$\(tex)$$"), [.paragraph([math(tex, .display)])])
    }

    func testDisplayFormulasStayRunsOfTheirParagraph() {
        XCTAssertEqual(
            blocks("so\n$$\nE = mc^2\n$$\nthus"),
            [.paragraph([prose("so "), math("\nE = mc^2\n", .display), prose(" thus")])])
    }

    func testTeXDelimitersReadAsMath() {
        XCTAssertEqual(
            blocks(#"a \(x\) b"#), [.paragraph([prose("a "), math("x", .inline), prose(" b")])])
        XCTAssertEqual(blocks("\\[\n\\frac{a}{b}\n\\]"), [.paragraph([math("\n\\frac{a}{b}\n", .display)])])
    }

    func testDollarsThatAreNotMathStayProse() {
        XCTAssertEqual(blocks("costs $5 and $10 today"), [.paragraph([prose("costs $5 and $10 today")])])
        // The nearest opener wins: the formula is `x`, not `5, $x`.
        XCTAssertEqual(
            blocks("price $5, $x$ ok"), [.paragraph([prose("price $5, "), math("x", .inline), prose(" ok")])])
        XCTAssertEqual(blocks(#"an escaped \$x$ stays"#), [.paragraph([prose("an escaped $x$ stays")])])
        XCTAssertEqual(blocks("a $ x $ b"), [.paragraph([prose("a $ x $ b")])])
    }

    func testCodeKeepsItsDollars() {
        var code = InlineStyle.plain
        code.code = true
        XCTAssertEqual(
            blocks("run `echo $HOME $x$` now"),
            [.paragraph([prose("run "), InlineRun(text: "echo $HOME $x$", style: code), prose(" now")])])
        XCTAssertEqual(blocks("```\n$x$ and \\(y\\)\n```"), [.codeBlock(language: nil, code: "$x$ and \\(y\\)")])
        // A link's destination is never math.
        if case .paragraph(let runs) = blocks("[see](https://x.dev/$a$b)").first {
            XCTAssertEqual(runs.first?.style.link, "https://x.dev/$a$b")
        } else {
            XCTFail("a paragraph")
        }
    }

    func testFormulasNeverCrossABlankLine() {
        XCTAssertEqual(blocks("$a\n\nb$"), [.paragraph([prose("$a")]), .paragraph([prose("b$")])])
    }

    func testMultiLineFormulasKeepLineNumbers() {
        // The incremental parser re-parses from a block's start line, so a
        // formula's placeholder must not shift the lines after it.
        let source = "intro\n\n$$\na\n\\\\\nb\n$$\n\n- after"
        let tops = MarkdownParser.parse(source)
        XCTAssertEqual(tops.map(\.startLine), [1, 3, 9])
        XCTAssertEqual(tops[1].block, .paragraph([math("\na\n\\\\\nb\n", .display)]))
    }

    func testFormulasInAQuoteLoseTheQuoteMarkers() {
        XCTAssertEqual(
            blocks("> $$\n> x\n> $$"), [.blockquote([.paragraph([math("\nx\n", .display)])])])
    }

    func testStreamingMatchesAFullParse() {
        let reply = """
            Roots of $ax^2 + bx + c$ are

            $$
            x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}
            $$

            and \\(\\Delta\\) decides. Prices: $5 and $10.

            - item \\[ y \\]
            """
        let parser = IncrementalMarkdownParser()
        var fed = ""
        for character in reply {
            fed.append(character)
            parser.setText(fed)
        }
        XCTAssertEqual(parser.blocks, MarkdownParser.parse(reply))
    }

    // MARK: Rendering

    private func attachments(_ text: NSAttributedString) -> [(source: String, display: Bool)] {
        var found: [(String, Bool)] = []
        text.enumerateAttribute(.cypherMathSource, in: NSRange(location: 0, length: text.length)) { value, range, _ in
            guard let source = value as? String else { return }
            let display = text.attribute(.cypherDisplayMath, at: range.location, effectiveRange: nil) != nil
            XCTAssertTrue(text.attribute(.attachment, at: range.location, effectiveRange: nil) is MathAttachment)
            found.append((source, display))
        }
        return found
    }

    func testInlineFormulasTypesetIntoTheLine() throws {
        let text = TranscriptTextStyle.inline([prose("so "), math(#"\frac{a}{b}"#, .inline), prose(" ok")])
        XCTAssertEqual(text.string, "so \u{FFFC} ok")
        XCTAssertEqual(attachments(text).map(\.source), [#"$\frac{a}{b}$"#])
        let attachment = try XCTUnwrap(text.attribute(.attachment, at: 3, effectiveRange: nil) as? MathAttachment)
        // On the baseline: some of it below (the fraction's denominator).
        XCTAssertLessThan(attachment.bounds.minY, 0)
        XCTAssertGreaterThan(attachment.bounds.maxY, 0)
        XCTAssertEqual(attachment.image?.renderingMode, .alwaysTemplate, "takes the text color")
        // A paragraph with a formula may grow past the fixed line height.
        let style = try XCTUnwrap(text.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle)
        XCTAssertEqual(style.maximumLineHeight, 0)
    }

    func testTeXThatDoesNotTypesetReadsAsCode() {
        let text = TranscriptTextStyle.inline([prose("so "), math(#"\undefinedmacro{x}"#, .inline)])
        XCTAssertEqual(text.string, #"so \undefinedmacro{x}"#)
        XCTAssertNotNil(text.attribute(.cypherInlineCode, at: 4, effectiveRange: nil))
        XCTAssertTrue(attachments(text).isEmpty)
    }

    func testKaTeXSpellingsSwiftMathLacksStillTypeset() {
        for tex in [#"\tfrac{1}{3}"#, #"\dfrac{1}{3}"#, #"\operatorname{tr} A"#, #"\boldsymbol{x}"#] {
            XCTAssertNotNil(MathRenderer.attachment(tex, kind: .inline, size: MD.textSize), tex)
        }
    }

    func testDisplayFormulasGetACenteredParagraph() throws {
        let text = TranscriptTextStyle.inline([
            prose("so "), math("x^2", .display), prose(" thus"),
        ])
        XCTAssertEqual(text.string, "so\n\u{FFFC}\nthus")
        XCTAssertEqual(attachments(text).map(\.display), [true])
        let style = { (at: Int) in text.attribute(.paragraphStyle, at: at, effectiveRange: nil) as? NSParagraphStyle }
        XCTAssertEqual(style(0)?.alignment, .natural)
        XCTAssertEqual(style(3)?.alignment, .center)
        XCTAssertEqual(style(3)?.paragraphSpacingBefore, MD.blockGap)
        XCTAssertEqual(style(3)?.paragraphSpacing, MD.blockGap)
        XCTAssertEqual(style(5)?.alignment, .natural)
    }

    func testProseJoinKeepsFormulasAndTheirParagraphs() {
        let text = TranscriptTextStyle.prose([
            .paragraph([prose("first "), math("a", .inline)]),
            .list(orderedStart: nil, items: [MDListItem(checked: nil, children: [.paragraph([math("b", .display)])])]),
        ])
        XCTAssertEqual(attachments(text).map(\.source), ["$a$", "$$b$$"])
        // The break after the first paragraph is not part of its formula.
        let breakIx = (text.string as NSString).range(of: "\n").location
        XCTAssertNil(text.attribute(.cypherMathSource, at: breakIx, effectiveRange: nil))
    }

    func testSelectionsCopyFormulasAsTheirSource() {
        let text = TranscriptTextStyle.inline([
            prose("a "), math("x", .inline), math("x", .inline), prose(" b"), math("y", .display),
        ])
        XCTAssertEqual(
            MathRenderer.plainText(text, range: NSRange(location: 0, length: text.length)),
            "a $x$$x$ b\n$$y$$")
        // Text without formulas leaves copying to UIKit.
        XCTAssertNil(MathRenderer.plainText(text, range: NSRange(location: 0, length: 1)))
    }
}

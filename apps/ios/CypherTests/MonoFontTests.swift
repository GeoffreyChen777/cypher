import CoreText
import XCTest
@testable import Cypher

/// Geist Mono 1.700's coding ligatures are one cell wide but ink two cells to
/// their left, so code text must shape with them off (see `Theme.monoUI`).
@MainActor
final class MonoFontTests: XCTestCase {
    private let samples = ["a === b", "a == b", "a !== b", "x => y"]

    func testBundledFontStillLigatesByDefault() throws {
        // The premise: without the feature settings the line collapses. If an
        // updated font stops doing this, the workaround can be revisited.
        let raw = try XCTUnwrap(UIFont(name: Theme.fontMonoName, size: 13))
        XCTAssertLessThan(glyphCount("a === b", raw), "a === b".count)
    }

    func testCodeTextKeepsOneGlyphPerCell() {
        let font = Theme.monoUI(13)
        XCTAssertEqual(font.fontName, Theme.fontMonoName)
        let cell = width(NSAttributedString(string: "a", attributes: [.font: font]))
        for sample in samples {
            let line = NSAttributedString(string: sample, attributes: [.font: font])
            XCTAssertEqual(glyphCount(sample, font), sample.count, sample)
            XCTAssertEqual(width(line), cell * CGFloat(sample.count), accuracy: 0.01, sample)
        }
    }

    func testCodeBlocksShapeWithoutLigatures() {
        let code = samples.joined(separator: "\n")
        let text = TranscriptTextStyle.code(code, spans: [])
        let lines = CTFrameGetLines(frame(text)) as! [CTLine]
        XCTAssertEqual(lines.count, samples.count)
        for (line, sample) in zip(lines, samples) {
            // A line's range includes its newline, which shapes a glyph too.
            XCTAssertEqual(CTLineGetGlyphCount(line), CTLineGetStringRange(line).length, sample)
        }
    }

    private func glyphCount(_ string: String, _ font: UIFont) -> Int {
        CTLineGetGlyphCount(CTLineCreateWithAttributedString(
            NSAttributedString(string: string, attributes: [.font: font])))
    }

    private func width(_ text: NSAttributedString) -> CGFloat {
        CGFloat(CTLineGetTypographicBounds(CTLineCreateWithAttributedString(text), nil, nil, nil))
    }

    private func frame(_ text: NSAttributedString) -> CTFrame {
        let setter = CTFramesetterCreateWithAttributedString(text)
        let path = CGPath(rect: CGRect(x: 0, y: 0, width: 1000, height: 1000), transform: nil)
        return CTFramesetterCreateFrame(setter, CFRange(location: 0, length: 0), path, nil)
    }
}

// Code block tokenizer: every line must terminate, whatever Unicode it holds.

import XCTest
@testable import Cypher

final class HighlightTests: XCTestCase {
    func testNonASCIINumericCharactersTerminate() {
        // `Character.isNumber` is true for these but `isHexDigit` is false; the
        // number scanner used to spin on them until allocation failed.
        let code = "let 一 = \"x\"\nx² + ½ + ① + ٣ + Ⅻ\n# 第一 步"
        for lang in [HighlightLanguage.rust, .python, .bash, .json, .markdown] {
            let lines = Highlighter.highlight(code: code, language: lang)
            XCTAssertEqual(lines.count, 3)
            for spans in lines {
                XCTAssertFalse(spans.contains { $0.range.isEmpty })
            }
        }
    }

    func testASCIINumbersStillHighlight() {
        var carry = LineCarry()
        let spans = Highlighter.tokenizeLine(Array("x = 0x1f + 42"), language: .rust, carry: &carry)
        XCTAssertEqual(spans.filter { $0.cls == .number }.map(\.range), [4..<8, 11..<13])
    }
}

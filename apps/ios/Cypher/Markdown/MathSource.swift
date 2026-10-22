// LaTeX formulas in chat Markdown — the parser half (the desktop's
// crates/ui/src/markdown/parser.rs reads them through pulldown-cmark's math
// extension; swift-markdown has none).
//
// cmark would mangle TeX before we saw it (`\\` rows collapse, `_` turns
// into emphasis, `\(` loses its backslash), so `rewrite` swaps each formula
// for an opaque placeholder first: private-use characters and digits, which
// cmark reads as plain text. The parser turns placeholders back into math
// runs. Line structure is kept — a multi-line formula's later lines become
// one filler character each — so block start lines still index the caller's
// source, which the incremental parser relies on.

import Foundation
import Markdown

enum MathKind: Hashable {
    /// `$…$` / `\(…\)`: set in the text style, in the line.
    case inline
    /// `$$…$$` / `\[…\]`: set in the display style, on a row of its own.
    case display
}

enum MathSource {
    /// Placeholder grammar: an opener (inline or display), the formula's
    /// index in decimal, the closer. Later lines of a multi-line formula
    /// hold one `filler` each.
    static let inlineOpen: Character = "\u{E000}"
    static let displayOpen: Character = "\u{E001}"
    static let close: Character = "\u{E002}"
    static let filler: Character = "\u{E003}"

    struct Rewrite: Equatable {
        var source: String
        /// TeX sources, indexed by the placeholders.
        var formulas: [String]
    }

    /// `source` with its formulas swapped for placeholders, or nil when it
    /// has none (the common case costs a byte scan, no parse).
    static func rewrite(_ source: String) -> Rewrite? {
        let bytes = Array(source.utf8)
        guard mayHoldMath(bytes) else { return nil }
        let spans = scan(bytes, skipping: literalRanges(in: source, bytes: bytes))
        guard !spans.isEmpty else { return nil }

        var out = [UInt8]()
        out.reserveCapacity(bytes.count)
        var formulas: [String] = []
        var at = 0
        for span in spans {
            out.append(contentsOf: bytes[at..<span.outer.lowerBound])
            let tex = texLines(bytes, span: span)
            let opener = span.kind == .inline ? inlineOpen : displayOpen
            out.append(contentsOf: Array("\(opener)\(formulas.count)\(close)".utf8))
            formulas.append(tex.text)
            for prefix in tex.continuationPrefixes {
                out.append(UInt8(ascii: "\n"))
                out.append(contentsOf: prefix)
                out.append(contentsOf: Array(String(filler).utf8))
            }
            at = span.outer.upperBound
        }
        out.append(contentsOf: bytes[at...])
        return Rewrite(source: String(decoding: out, as: UTF8.self), formulas: formulas)
    }

    /// Split cmark text that may hold placeholders into runs: prose in
    /// `style`, formulas as math runs (which drop emphasis and links — TeX
    /// sets them, not the prose fonts). A filler becomes a run of its own,
    /// for `dropFillers` to remove with the line break before it.
    static func runs(_ text: String, style: InlineStyle, formulas: [String]) -> [InlineRun] {
        guard text.contains(where: isPlaceholderCharacter) else {
            return [InlineRun(text: text, style: style)]
        }
        var runs: [InlineRun] = []
        var prose = ""
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            if character == inlineOpen || character == displayOpen,
                let closer = text[index...].firstIndex(of: close),
                let slot = Int(text[text.index(after: index)..<closer]),
                formulas.indices.contains(slot)
            {
                if !prose.isEmpty { runs.append(InlineRun(text: prose, style: style)) }
                prose = ""
                var math = InlineStyle.plain
                math.math = character == inlineOpen ? .inline : .display
                runs.append(InlineRun(text: formulas[slot], style: math))
                index = text.index(after: closer)
                continue
            }
            if character == filler {
                if !prose.isEmpty { runs.append(InlineRun(text: prose, style: style)) }
                prose = ""
                runs.append(InlineRun(text: String(filler), style: style))
            } else if !isPlaceholderCharacter(character) {
                prose.append(character)
            }
            index = text.index(after: index)
        }
        if !prose.isEmpty { runs.append(InlineRun(text: prose, style: style)) }
        return runs
    }

    /// Remove the filler runs a multi-line formula leaves, each with the soft
    /// break (a space) that led to its line — those lines were the formula's.
    static func dropFillers(_ runs: [InlineRun]) -> [InlineRun] {
        var out: [InlineRun] = []
        for run in runs {
            guard run.text == String(filler) else {
                out.append(run)
                continue
            }
            if out.last?.text == " " { out.removeLast() }
        }
        return out
    }

    /// Placeholders read back as Markdown-delimited TeX, for places that
    /// show text plainly (an image's alt text, an unknown block).
    static func plain(_ text: String, formulas: [String]) -> String {
        dropFillers(runs(text, style: .plain, formulas: formulas)).map { run in
            switch run.style.math {
            case .inline: return "$\(run.text)$"
            case .display: return "$$\(run.text)$$"
            case nil: return run.text
            }
        }.joined()
    }

    private static func isPlaceholderCharacter(_ character: Character) -> Bool {
        character == inlineOpen || character == displayOpen || character == close
            || character == filler
    }

    // MARK: Scanning

    struct Span: Equatable {
        /// Delimiters included.
        var outer: Range<Int>
        var inner: Range<Int>
        var kind: MathKind
    }

    private static let dollar = UInt8(ascii: "$")
    private static let backslash = UInt8(ascii: "\\")
    private static let newline = UInt8(ascii: "\n")

    private static func mayHoldMath(_ bytes: [UInt8]) -> Bool {
        for (i, byte) in bytes.enumerated() {
            if byte == dollar { return true }
            if byte == backslash, i + 1 < bytes.count,
                bytes[i + 1] == UInt8(ascii: "(") || bytes[i + 1] == UInt8(ascii: "[")
            {
                return true
            }
        }
        return false
    }

    /// Formulas in `bytes`, in order. The rules follow pulldown-cmark's
    /// math extension (what the desktop reads) closely enough to agree on
    /// what models write: an inline `$` opens before non-space and closes
    /// after non-space, pairing with the nearest opener (so "$5 and $10"
    /// stays prose); `$$` pairs anywhere; `\(…\)` and `\[…\]` pair like TeX. No formula
    /// crosses a blank line or a code span, and `\$` is a literal dollar.
    static func scan(_ bytes: [UInt8], skipping literals: [Range<Int>]) -> [Span] {
        var spans: [Span] = []
        var literalIx = 0
        /// The literal range holding `i`, if any (ranges are sorted).
        func literal(at i: Int) -> Range<Int>? {
            while literalIx < literals.count, literals[literalIx].upperBound <= i { literalIx += 1 }
            guard literalIx < literals.count, literals[literalIx].contains(i) else { return nil }
            return literals[literalIx]
        }
        /// Where a formula opened at `from` must stop: the next literal
        /// range or blank line.
        func limit(from: Int) -> Int {
            var end = bytes.count
            if let next = literals.first(where: { $0.lowerBound >= from }) { end = next.lowerBound }
            var j = from
            while j < end {
                if bytes[j] == newline {
                    var k = j + 1
                    while k < end,
                        bytes[k] == UInt8(ascii: " ") || bytes[k] == UInt8(ascii: "\t")
                            || bytes[k] == UInt8(ascii: ">")
                    {
                        k += 1
                    }
                    if k < end, bytes[k] == newline { return j }
                }
                j += 1
            }
            return end
        }
        func isSpace(_ i: Int) -> Bool {
            i < 0 || i >= bytes.count || bytes[i] == UInt8(ascii: " ") || bytes[i] == newline
                || bytes[i] == UInt8(ascii: "\t") || bytes[i] == UInt8(ascii: "\r")
        }

        var i = 0
        while i < bytes.count {
            if let range = literal(at: i) {
                i = range.upperBound
                continue
            }
            let byte = bytes[i]
            if byte == backslash {
                let next = i + 1 < bytes.count ? bytes[i + 1] : 0
                if next == UInt8(ascii: "(") || next == UInt8(ascii: "[") {
                    let closer = next == UInt8(ascii: "(") ? UInt8(ascii: ")") : UInt8(ascii: "]")
                    let end = limit(from: i + 2)
                    var j = i + 2
                    var found: Int?
                    while j + 1 < end {
                        if bytes[j] == backslash {
                            if bytes[j + 1] == closer {
                                found = j
                                break
                            }
                            j += 2
                        } else {
                            j += 1
                        }
                    }
                    if let found, hasContent(bytes, i + 2..<found) {
                        spans.append(
                            Span(
                                outer: i..<found + 2, inner: i + 2..<found,
                                kind: next == UInt8(ascii: "(") ? .inline : .display))
                        i = found + 2
                        continue
                    }
                }
                // An escape: the next character is literal (`\$`, `\\(`).
                i += 2
                continue
            }
            guard byte == dollar else {
                i += 1
                continue
            }
            var run = 1
            while i + run < bytes.count, bytes[i + run] == dollar { run += 1 }
            if run > 2 {
                i += run
                continue
            }
            let end = limit(from: i + run)
            var found: Int?
            if run == 2 {
                var j = i + 2
                while j + 1 < end {
                    if bytes[j] == backslash {
                        j += 2
                        continue
                    }
                    if bytes[j] == dollar, bytes[j + 1] == dollar {
                        found = j
                        break
                    }
                    j += 1
                }
            } else if !isSpace(i + 1) {
                // Dollars pair like brackets: the first one that can close
                // closes, and one that can only open takes over (so in
                // "costs $5, $x$" the formula is `x`).
                var j = i + 1
                while j < end {
                    if bytes[j] == backslash {
                        j += 2
                        continue
                    }
                    if bytes[j] == dollar {
                        if !isSpace(j - 1) { found = j }
                        if found != nil || !isSpace(j + 1) { break }
                    }
                    j += 1
                }
            }
            if let found, hasContent(bytes, i + run..<found) {
                spans.append(
                    Span(
                        outer: i..<found + run, inner: i + run..<found,
                        kind: run == 2 ? .display : .inline))
                i = found + run
                continue
            }
            i += run
        }
        return spans
    }

    private static func hasContent(_ bytes: [UInt8], _ range: Range<Int>) -> Bool {
        bytes[range].contains { !($0 == UInt8(ascii: " ") || $0 == newline || $0 == UInt8(ascii: "\t")) }
    }

    /// The formula's TeX, and the line prefix each of its later lines keeps
    /// in the rewritten source. A formula in a block quote loses its later
    /// lines' `>` markers from the TeX and keeps them in the source, so the
    /// quote stays one block.
    private static func texLines(_ bytes: [UInt8], span: Span)
        -> (text: String, continuationPrefixes: [[UInt8]])
    {
        let lineStart = bytes[..<span.outer.lowerBound].lastIndex(of: newline).map { $0 + 1 } ?? 0
        let quoted =
            bytes[lineStart..<span.outer.lowerBound]
            .first { $0 != UInt8(ascii: " ") && $0 != UInt8(ascii: "\t") } == UInt8(ascii: ">")
        let lines = bytes[span.inner].split(separator: newline, omittingEmptySubsequences: false)
        var tex: [String] = []
        var prefixes: [[UInt8]] = []
        for (n, line) in lines.enumerated() {
            guard n > 0 else {
                tex.append(String(decoding: line, as: UTF8.self))
                continue
            }
            let prefix = line.prefix {
                $0 == UInt8(ascii: " ") || $0 == UInt8(ascii: "\t") || (quoted && $0 == UInt8(ascii: ">"))
            }
            prefixes.append(Array(prefix))
            tex.append(String(decoding: line.dropFirst(prefix.count), as: UTF8.self))
        }
        return (tex.joined(separator: "\n"), prefixes)
    }

    // MARK: Literal ranges

    /// Byte ranges where a `$` is never math: code spans, code blocks, raw
    /// HTML and link destinations, found by cmark itself on the original
    /// source so they match what it renders.
    static func literalRanges(in source: String, bytes: [UInt8]) -> [Range<Int>] {
        var lineStarts = [0]
        for (i, byte) in bytes.enumerated() where byte == newline { lineStarts.append(i + 1) }
        func offset(_ location: SourceLocation) -> Int? {
            guard location.line >= 1, location.line <= lineStarts.count else { return nil }
            return min(bytes.count, lineStarts[location.line - 1] + location.column - 1)
        }
        func byteRange(_ range: SourceRange?) -> Range<Int>? {
            guard let range, let lower = offset(range.lowerBound), let upper = offset(range.upperBound),
                lower < upper
            else { return nil }
            return lower..<upper
        }
        var ranges: [Range<Int>] = []
        func walk(_ markup: Markup) {
            switch markup {
            case is CodeBlock, is InlineCode, is HTMLBlock, is InlineHTML:
                if let range = byteRange(markup.range) { ranges.append(range) }
                return
            case is Markdown.Link, is Markdown.Image:
                // The destination follows the link text: everything after
                // the last child is `](…)`.
                if let whole = byteRange(markup.range) {
                    let textEnd = markup.children.compactMap { byteRange($0.range)?.upperBound }.max()
                    ranges.append((textEnd ?? whole.lowerBound)..<whole.upperBound)
                }
            default:
                break
            }
            for child in markup.children { walk(child) }
        }
        walk(Document(parsing: source))
        ranges.sort { $0.lowerBound < $1.lowerBound }
        return ranges
    }
}

// Markdown block model — a port of crates/ui/src/markdown/parser.rs.
//
// The transcript renders one row per *top-level block*, so the model is
// block-first: a parsed document is a flat list of `TopBlock`s whose content
// hash doubles as the row-version key for the virtualizer. Inline content is a
// run model (adjacent same-style runs merged) rather than an AST, which keeps
// rendering a single pass over styled spans.

import Foundation
import Markdown

struct InlineStyle: Hashable {
    var bold = false
    var italic = false
    var code = false
    var strikethrough = false
    var link: String? = nil
    /// The run is a LaTeX formula (its text is the TeX source), not prose.
    var math: MathKind? = nil

    static let plain = InlineStyle()
}

struct InlineRun: Hashable {
    var text: String
    var style: InlineStyle
}

enum MDAlign: Hashable {
    case left, center, right, none
}

struct MDListItem: Hashable {
    /// Task-list checkbox state; nil for plain items.
    var checked: Bool?
    var children: [MDBlock]
}

indirect enum MDBlock: Hashable {
    case paragraph([InlineRun])
    case heading(level: Int, [InlineRun])
    /// `closed`: the fence has its closing line (indented blocks always do).
    /// A fence still streaming has none yet, so renderers that turn the code
    /// into something else (diagrams) wait for it.
    case codeBlock(language: String?, code: String, closed: Bool = true)
    /// A fence tagged `artifact` (```html artifact title="…"```): a page the
    /// model wrote to be viewed. `closed` is false while its fence streams.
    /// `patch`: tagged `artifact-patch`, `code` is SEARCH/REPLACE hunks that
    /// edit the latest version of the page `title` names (ArtifactPatch.swift).
    case artifact(kind: ArtifactKind, title: String?, code: String, closed: Bool, patch: Bool = false)
    case blockquote([MDBlock])
    case list(orderedStart: Int?, items: [MDListItem])
    case table(header: [[InlineRun]], rows: [[[InlineRun]]], align: [MDAlign])
    case rule
}

/// A top-level block plus the 1-based source line it starts on (the stable
/// re-parse anchor) and a content hash used as the row diff key.
struct TopBlock: Hashable {
    var startLine: Int
    var block: MDBlock

    /// FNV-1a-style stable content fingerprint (row version key).
    var fingerprint: UInt64 {
        var hasher = Hasher()
        block.hash(into: &hasher)
        return UInt64(bitPattern: Int64(hasher.finalize()))
    }
}

// MARK: - AST walk (swift-markdown → block model)

enum MarkdownParser {
    /// Parse a complete markdown source into top-level blocks.
    static func parse(_ source: String) -> [TopBlock] {
        // Formulas become placeholders cmark leaves alone (MathSource.swift);
        // the rewrite keeps every line, so start lines index `source`.
        let math = MathSource.rewrite(source)
        let parsed = math?.source ?? source
        let document = Document(parsing: parsed)
        let context = Context(
            formulas: math?.formulas ?? [],
            lines: parsed.split(separator: "\n", omittingEmptySubsequences: false))
        return document.children.compactMap { child in
            guard let block = convertBlock(child, context) else { return nil }
            let line = child.range?.lowerBound.line ?? 1
            return TopBlock(startLine: line, block: block)
        }
    }

    /// What every block conversion reads: the formulas the math rewrite took
    /// out, and the parsed source's lines (block ranges index them).
    private struct Context {
        var formulas: [String]
        var lines: [Substring]
    }

    private static func convertBlock(_ markup: Markup, _ context: Context) -> MDBlock? {
        let formulas = context.formulas
        switch markup {
        case let paragraph as Paragraph:
            return .paragraph(inlines(of: paragraph, formulas))
        case let heading as Heading:
            return .heading(level: heading.level, inlines(of: heading, formulas))
        case let code as CodeBlock:
            var body = code.code
            if body.hasSuffix("\n") { body.removeLast() }
            // cmark hands over the whole info string; its first word is the
            // language, as on the desktop.
            let info = code.language ?? ""
            let closed = code.range.map { fenceClosed($0, lines: context.lines) } ?? true
            if let (kind, title, patch) = ArtifactFence.info(info) {
                return .artifact(kind: kind, title: title, code: body, closed: closed, patch: patch)
            }
            let lang = info.split(whereSeparator: \.isWhitespace).first.map(String.init)
            return .codeBlock(language: lang, code: body, closed: closed)
        case let quote as BlockQuote:
            return .blockquote(quote.children.compactMap { convertBlock($0, context) })
        case let list as UnorderedList:
            return .list(orderedStart: nil, items: listItems(of: list, context))
        case let list as OrderedList:
            return .list(orderedStart: Int(list.startIndex), items: listItems(of: list, context))
        case let table as Markdown.Table:
            // `.cells`/`.rows` are lazy sequences — materialize eagerly.
            let header: [[InlineRun]] = table.head.cells.map { inlines(of: $0, formulas) }
            let rows: [[[InlineRun]]] = table.body.rows.map { row in
                row.cells.map { inlines(of: $0, formulas) } as [[InlineRun]]
            }
            let align: [MDAlign] = table.columnAlignments.map {
                switch $0 {
                case .left: return .left
                case .center: return .center
                case .right: return .right
                case nil: return .none
                }
            }
            return .table(header: header, rows: rows, align: align)
        case is ThematicBreak:
            return .rule
        case let html as HTMLBlock:
            // No HTML rendering — surface the raw source as a code block,
            // matching the desktop's plain-text fallback behavior.
            return .codeBlock(language: "html", code: html.rawHTML.trimmingCharacters(in: .newlines))
        default:
            // Unknown/unsupported block: flatten to a paragraph of its text.
            let text = MathSource.plain(markup.format(), formulas: formulas)
            guard !text.isEmpty else { return nil }
            return .paragraph([InlineRun(text: text, style: .plain)])
        }
    }

    private static func listItems(of list: Markup, _ context: Context) -> [MDListItem] {
        list.children.compactMap { child in
            guard let item = child as? ListItem else { return nil }
            let checked: Bool? = item.checkbox.map { $0 == .checked }
            return MDListItem(
                checked: checked, children: item.children.compactMap { convertBlock($0, context) })
        }
    }

    /// Whether a code block's source ends in a closing fence: a line of at
    /// least the opener's run of the opener's character (desktop parser.rs
    /// `fence_closed`). Indented blocks have no fence and count as closed.
    /// Columns are 1-based UTF-8 offsets; container markers (`>`, the
    /// indentation under a list item) are trimmed off the closing line.
    static func fenceClosed(_ range: SourceRange, lines: [Substring]) -> Bool {
        let first = range.lowerBound.line - 1
        let last = range.upperBound.line - 1
        guard first >= 0, first < lines.count else { return true }
        let openerLine = lines[first].utf8.dropFirst(max(0, range.lowerBound.column - 1))
        let opener = (String(openerLine) ?? "").drop(while: { $0 == " " || $0 == "\t" || $0 == ">" })
        guard let fence = opener.first, fence == "`" || fence == "~" else { return true }
        let run = opener.prefix(while: { $0 == fence }).count
        // The block's last non-blank line after the opener.
        var ix = min(last, lines.count - 1)
        while ix > first,
            lines[ix].allSatisfy({ $0 == " " || $0 == "\t" || $0 == ">" || $0 == "\r" })
        {
            ix -= 1
        }
        guard ix > first else { return false }
        let closer = lines[ix]
            .drop(while: { $0 == " " || $0 == "\t" || $0 == ">" })
            .trimmingCharacters(in: .whitespaces)
        return closer.count >= run && closer.allSatisfy { $0 == fence }
    }

    private static func inlines(of container: Markup, _ formulas: [String]) -> [InlineRun] {
        var runs: [InlineRun] = []
        for child in container.children {
            collectInline(child, style: .plain, formulas, into: &runs)
        }
        return mergeRuns(MathSource.dropFillers(runs))
    }

    private static func collectInline(
        _ markup: Markup, style: InlineStyle, _ formulas: [String], into runs: inout [InlineRun]
    ) {
        switch markup {
        case let text as Markdown.Text:
            runs.append(contentsOf: MathSource.runs(text.string, style: style, formulas: formulas))
        case let code as InlineCode:
            var s = style
            s.code = true
            runs.append(InlineRun(text: code.code, style: s))
        case let strong as Strong:
            var s = style
            s.bold = true
            strong.children.forEach { collectInline($0, style: s, formulas, into: &runs) }
        case let em as Emphasis:
            var s = style
            s.italic = true
            em.children.forEach { collectInline($0, style: s, formulas, into: &runs) }
        case let strike as Strikethrough:
            var s = style
            s.strikethrough = true
            strike.children.forEach { collectInline($0, style: s, formulas, into: &runs) }
        case let link as Markdown.Link:
            var s = style
            s.link = link.destination
            link.children.forEach { collectInline($0, style: s, formulas, into: &runs) }
        case let image as Markdown.Image:
            // Images render as their alt text (desktop parity: no inline images).
            let alt = MathSource.plain(
                image.children.compactMap { ($0 as? Markdown.Text)?.string }.joined(), formulas: formulas)
            runs.append(InlineRun(text: alt.isEmpty ? (image.source ?? "") : alt, style: style))
        case is SoftBreak:
            runs.append(InlineRun(text: " ", style: style))
        case is LineBreak:
            runs.append(InlineRun(text: "\n", style: style))
        case let html as InlineHTML:
            runs.append(InlineRun(text: html.rawHTML, style: style))
        default:
            for child in markup.children {
                collectInline(child, style: style, formulas, into: &runs)
            }
        }
    }

    /// Merge adjacent runs with identical style so rendering sees minimal spans.
    static func mergeRuns(_ runs: [InlineRun]) -> [InlineRun] {
        var merged: [InlineRun] = []
        for run in runs {
            if run.text.isEmpty { continue }
            // Two adjacent formulas stay two formulas.
            if var last = merged.last, last.style == run.style, run.style.math == nil {
                last.text += run.text
                merged[merged.count - 1] = last
            } else {
                merged.append(run)
            }
        }
        return merged
    }
}

// MARK: - Incremental streaming parser (parser.rs IncrementalParser port)

/// Re-parses only the streaming tail: on append, parsing restarts from the
/// start of the *second-to-last* top-level block (covers continuation merges),
/// so per-append cost is O(delta + tail), never O(document). Link-reference
/// definitions (`[label]: url`) break the locality assumption and force full
/// re-parses.
final class IncrementalMarkdownParser {
    private(set) var source: String = ""
    private(set) var blocks: [TopBlock] = []
    private var fullOnly = false

    func setText(_ text: String) {
        if text == source { return }
        if !fullOnly, text.hasPrefix(source), !source.isEmpty {
            append(text)
        } else {
            reset(text)
        }
    }

    private func reset(_ text: String) {
        source = text
        fullOnly = Self.hasLinkDefs(text)
        blocks = MarkdownParser.parse(text)
    }

    private func append(_ text: String) {
        let delta = String(text.dropFirst(source.count))
        source = text
        if Self.hasLinkDefs(delta) {
            fullOnly = true
            blocks = MarkdownParser.parse(text)
            return
        }
        guard blocks.count >= 2 else {
            blocks = MarkdownParser.parse(text)
            return
        }
        // Stable boundary: the start line of the second-to-last block.
        let boundaryLine = blocks[blocks.count - 2].startLine
        let stable = Array(blocks.prefix(blocks.count - 2))
        let tailSource = Self.suffix(of: text, fromLine: boundaryLine)
        let tailBlocks = MarkdownParser.parse(tailSource).map { top in
            TopBlock(startLine: top.startLine + boundaryLine - 1, block: top.block)
        }
        blocks = stable + tailBlocks
    }

    /// The substring starting at the given 1-based line.
    private static func suffix(of text: String, fromLine line: Int) -> String {
        guard line > 1 else { return text }
        var remaining = line - 1
        var index = text.startIndex
        while remaining > 0, let nl = text[index...].firstIndex(of: "\n") {
            index = text.index(after: nl)
            remaining -= 1
        }
        return String(text[index...])
    }

    // A regex literal is never mutated; `Regex` just isn't marked Sendable.
    nonisolated(unsafe) private static let linkDefPattern = /(?m)^\s{0,3}\[[^\]]+\]:/
    static func hasLinkDefs(_ text: String) -> Bool {
        text.contains(linkDefPattern)
    }
}

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
    /// The run is a mention chip in a sent prompt (`code` too): violet on its
    /// own wash, never a file reference chip.
    var mention = false

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
    /// An image (`![alt](src)`, or a paragraph that is only an image path):
    /// a block preview after the paragraph it was written in. `src` as
    /// written; `ImageSource` says where its bytes come from. `caption` is
    /// false when the text the image stood in kept its alt (desktop
    /// parser.rs `Block::Image`): the alt is said once.
    case image(src: String, alt: String, caption: Bool = true)
    /// A paragraph that is only a path to a csv/tsv, pdf, video or html
    /// file (`FilePreview.fileLine`): a table, first page, frame or page card.
    case filePreview(src: String, kind: FilePreview.Kind)
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
        return document.children.flatMap { child in
            let line = child.range?.lowerBound.line ?? 1
            return convertBlocks(child, context).map { TopBlock(startLine: line, block: $0) }
        }
    }

    /// What every block conversion reads: the formulas the math rewrite took
    /// out, and the parsed source's lines (block ranges index them); and
    /// where the images met in inline text wait for their block to end.
    private struct Context {
        var formulas: [String]
        var lines: [Substring]
        let images = ImageSink()
    }

    /// Images met in inline text, `(src, alt)`, until the block they stood
    /// in takes them.
    private final class ImageSink {
        var images: [(src: String, alt: String)] = []
    }

    /// A block as the blocks it renders as (desktop parser.rs
    /// `paragraph_blocks`): a paragraph, heading or table is followed by the
    /// images written in it — the sentence keeps their alt text and the
    /// images then carry no caption, so it is said once; a paragraph that
    /// said nothing but the images is dropped and they caption themselves —
    /// and a paragraph that only names an image file is that image, one that
    /// only names a previewable file that file's preview.
    private static func convertBlocks(_ markup: Markup, _ context: Context) -> [MDBlock] {
        if let paragraph = markup as? Paragraph, let source = lineSource(of: paragraph, context) {
            if let src = ImageSource.imageLine(source) { return [.image(src: src, alt: src)] }
            if let src = FilePreview.fileLine(source), let kind = FilePreview.kind(src) {
                return [.filePreview(src: src, kind: kind)]
            }
        }
        let taken = context.images.images.count
        let block = convertBlock(markup, context)
        let found = Array(context.images.images[taken...])
        context.images.images.removeSubrange(taken...)
        func images(caption: Bool) -> [MDBlock] {
            found.map { MDBlock.image(src: $0.src, alt: $0.alt, caption: caption) }
        }
        guard let block else { return images(caption: true) }
        guard !found.isEmpty else { return [block] }
        if case .paragraph(let runs) = block {
            let squeeze = { (text: String) in text.filter { !$0.isWhitespace } }
            if runs.map({ squeeze($0.text) }).joined() == found.map({ squeeze($0.alt) }).joined() {
                return images(caption: true)
            }
        }
        return [block] + images(caption: false)
    }

    /// A one-line paragraph's Markdown source, for `ImageSource.imageLine`
    /// and `FilePreview.fileLine`.
    private static func lineSource(of paragraph: Paragraph, _ context: Context) -> String? {
        guard let range = paragraph.range, range.lowerBound.line == range.upperBound.line,
            range.lowerBound.line >= 1, range.lowerBound.line - 1 < context.lines.count
        else { return nil }
        let line = context.lines[range.lowerBound.line - 1].utf8
        let source = String(decoding: line.dropFirst(max(0, range.lowerBound.column - 1)), as: UTF8.self)
        guard source.utf8.count <= 4096 else { return nil }
        return source
    }

    private static func convertBlock(_ markup: Markup, _ context: Context) -> MDBlock? {
        let formulas = context.formulas
        switch markup {
        case let paragraph as Paragraph:
            return .paragraph(inlines(of: paragraph, context))
        case let heading as Heading:
            return .heading(level: heading.level, inlines(of: heading, context))
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
            return .blockquote(quote.children.flatMap { convertBlocks($0, context) })
        case let list as UnorderedList:
            return .list(orderedStart: nil, items: listItems(of: list, context))
        case let list as OrderedList:
            return .list(orderedStart: Int(list.startIndex), items: listItems(of: list, context))
        case let table as Markdown.Table:
            // `.cells`/`.rows` are lazy sequences — materialize eagerly.
            let header: [[InlineRun]] = table.head.cells.map { inlines(of: $0, context) }
            let rows: [[[InlineRun]]] = table.body.rows.map { row in
                row.cells.map { inlines(of: $0, context) } as [[InlineRun]]
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
                checked: checked, children: item.children.flatMap { convertBlocks($0, context) })
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

    private static func inlines(of container: Markup, _ context: Context) -> [InlineRun] {
        var runs: [InlineRun] = []
        for child in container.children {
            collectInline(child, style: .plain, context.formulas, into: &runs, images: context.images)
        }
        return mergeRuns(MathSource.dropFillers(runs))
    }

    /// `images`: where the images met go, to follow their block.
    private static func collectInline(
        _ markup: Markup, style: InlineStyle, _ formulas: [String], into runs: inout [InlineRun],
        images: ImageSink
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
            strong.children.forEach { collectInline($0, style: s, formulas, into: &runs, images: images) }
        case let em as Emphasis:
            var s = style
            s.italic = true
            em.children.forEach { collectInline($0, style: s, formulas, into: &runs, images: images) }
        case let strike as Strikethrough:
            var s = style
            s.strikethrough = true
            strike.children.forEach { collectInline($0, style: s, formulas, into: &runs, images: images) }
        case let link as Markdown.Link:
            var s = style
            s.link = link.destination
            link.children.forEach { collectInline($0, style: s, formulas, into: &runs, images: images) }
        case let image as Markdown.Image:
            let alt = MathSource.plain(
                image.children.compactMap { ($0 as? Markdown.Text)?.string }.joined(), formulas: formulas)
            // The sentence keeps the alt text; an image the source names
            // follows its block.
            if let src = image.source, ImageSource.isImage(src) {
                images.images.append((src, alt.trimmingCharacters(in: .whitespacesAndNewlines)))
                if !alt.isEmpty { runs.append(InlineRun(text: alt, style: style)) }
            } else {
                runs.append(InlineRun(text: alt.isEmpty ? (image.source ?? "") : alt, style: style))
            }
        case is SoftBreak:
            runs.append(InlineRun(text: " ", style: style))
        case is LineBreak:
            runs.append(InlineRun(text: "\n", style: style))
        case let html as InlineHTML:
            runs.append(InlineRun(text: html.rawHTML, style: style))
        default:
            for child in markup.children {
                collectInline(child, style: style, formulas, into: &runs, images: images)
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
        // Blocks of one source block (a paragraph and its images) share a
        // start line: count source blocks, not blocks.
        var starts: [Int] = []
        for block in blocks where starts.last != block.startLine { starts.append(block.startLine) }
        guard starts.count >= 2 else {
            blocks = MarkdownParser.parse(text)
            return
        }
        // Stable boundary: the start line of the second-to-last source block.
        let boundaryLine = starts[starts.count - 2]
        let stable = Array(blocks.prefix { $0.startLine < boundaryLine })
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

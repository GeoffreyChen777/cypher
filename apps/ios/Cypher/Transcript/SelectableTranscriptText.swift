// Native selection in-place, sharing one text layout for painting, hit tests
// and selection handles. The outer SwiftUI transcript remains virtualized.
import SwiftUI
import UIKit

extension NSAttributedString.Key {
    static let cypherInlineCode = NSAttributedString.Key("cypher.inlineCode")
    /// Under a mention chip in a sent prompt: its wash color.
    static let cypherMentionWash = NSAttributedString.Key("cypher.mentionWash")
}

/// The transcript entry a piece of selectable text belongs to.
struct TranscriptEntryContext: Equatable {
    var entryId: String
    var role: MessageRole
    /// Complete (a user prompt the host has, a finished reply) — only
    /// settled entries can anchor a fork.
    var settled: Bool
}

/// What a selection's edit menu may offer beyond Comment; nil = not here
/// (the session can't fork right now, or this transcript is a side chat).
struct TranscriptSelectionActions {
    var fork: ((TranscriptEntryContext) -> Void)?
    var sideChat: ((TranscriptEntryContext, String) -> Void)?
}

extension EnvironmentValues {
    @Entry var transcriptEntry: TranscriptEntryContext? = nil
    @Entry var transcriptSelectionActions: TranscriptSelectionActions? = nil
}

@MainActor
enum TranscriptTextStyle {
    static func inline(
        _ runs: [InlineRun], size: CGFloat = MD.textSize,
        weight: UIFont.Weight = .regular, lineHeight: CGFloat = MD.lineHeight
    ) -> NSAttributedString {
        let result = NSMutableAttributedString(string: "")
        let proseAttributes: [NSAttributedString.Key: Any] = [
            .font: Theme.sansUI(size, weight: weight),
            .foregroundColor: UIColor(Theme.text),
        ]
        // A display formula ended the paragraph; the next text starts a new one.
        var breakPending = false
        // File references show as chips, labelled together so two files of
        // one name keep a folder apart.
        let references = runs.map { run in
            run.style.code && !run.style.mention && run.style.link == nil && run.style.math == nil
                ? FileReference.detect(run.text) : nil
        }
        let labels = FileReference.labels(references.compactMap { $0 })
        var chips = 0
        for (index, var run) in runs.enumerated() {
            if let reference = references[index] {
                if breakPending {
                    result.append(NSAttributedString(string: "\n", attributes: proseAttributes))
                    breakPending = false
                }
                // A thin space apart from the prose on either side, as
                // inline code sits on the desktop.
                if index > 0, !runs[index - 1].style.code {
                    result.append(FileReferenceChip.margin(proseAttributes))
                }
                result.append(
                    FileReferenceChip.string(
                        raw: run.text, reference: reference, label: labels[chips], size: size,
                        weight: FileReferenceChip.weight(base: weight, bold: run.style.bold), index: chips))
                if index + 1 < runs.count, !runs[index + 1].style.code {
                    result.append(FileReferenceChip.margin(proseAttributes))
                }
                chips += 1
                continue
            }
            if let kind = run.style.math {
                if let attachment = MathRenderer.attachment(run.text, kind: kind, size: size) {
                    let math = MathRenderer.string(attachment, tex: run.text, kind: kind, attributes: proseAttributes)
                    if kind == .display {
                        // Its own paragraph: the soft breaks around a `$$`
                        // on its own line are not text.
                        trimTrailingWhitespace(result)
                        if result.length > 0 {
                            result.append(NSAttributedString(string: "\n", attributes: proseAttributes))
                        }
                        breakPending = true
                    } else if breakPending {
                        result.append(NSAttributedString(string: "\n", attributes: proseAttributes))
                        breakPending = false
                    }
                    result.append(math)
                    continue
                }
                // TeX that does not typeset reads as code, so the source shows.
                run = InlineRun(
                    text: run.text.trimmingCharacters(in: .whitespacesAndNewlines), style: InlineStyle(code: true))
            }
            if breakPending {
                let rest = String(run.text.drop(while: \.isWhitespace))
                if rest.isEmpty { continue }
                run.text = rest
                result.append(NSAttributedString(string: "\n", attributes: proseAttributes))
                breakPending = false
            }
            if run.style.mention {
                // A mention chip keeps its look: violet code type a little
                // under the prose, on a violet wash (`paintMentions`).
                result.append(
                    NSAttributedString(
                        string: run.text,
                        attributes: [
                            .font: Theme.monoUI(size - 1.5),
                            .foregroundColor: UIColor(Theme.mentionText),
                            .cypherMentionWash: UIColor(Theme.mentionWash),
                        ]))
                continue
            }
            var font =
                run.style.code
                ? Theme.monoUI(size * Theme.codeScale)
                : Theme.sansUI(size, weight: run.style.bold ? .semibold : weight)
            if run.style.italic,
                let descriptor = font.fontDescriptor.withSymbolicTraits(
                    font.fontDescriptor.symbolicTraits.union(.traitItalic))
            {
                font = UIFont(descriptor: descriptor, size: font.pointSize)
            }
            var attributes: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: UIColor(run.style.code ? Theme.inlineCodeText : Theme.text),
            ]
            if run.style.code { attributes[.cypherInlineCode] = UIColor(Theme.inlineCodeWash) }
            if run.style.strikethrough { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
            if let link = run.style.link, let url = TranscriptFileLinks.url(forLink: link) {
                attributes[.link] = url
                attributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
            }
            let start = result.length
            result.append(NSAttributedString(string: run.text, attributes: attributes))
            if run.style.code {
                InlineCodeBox.addRoom(
                    result, code: NSRange(location: start, length: result.length - start),
                    afterProse: index > 0 && !runs[index - 1].style.code)
            }
        }
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = lineHeight
        paragraph.maximumLineHeight = lineHeight
        let full = NSRange(location: 0, length: result.length)
        result.addAttribute(.paragraphStyle, value: paragraph, range: full)
        applyMathParagraphs(result, in: full, base: paragraph)
        return result
    }

    private static func trimTrailingWhitespace(_ text: NSMutableAttributedString) {
        let string = text.string as NSString
        var end = string.length
        while end > 0, let scalar = Unicode.Scalar(string.character(at: end - 1)),
            CharacterSet.whitespacesAndNewlines.contains(scalar)
        {
            end -= 1
        }
        text.deleteCharacters(in: NSRange(location: end, length: string.length - end))
    }

    /// Re-style the paragraphs of `text[range]` (all given `base`) around its
    /// formulas: lines may grow past `base`'s fixed height to hold a tall
    /// formula, a display formula's paragraph is centered with the block gap
    /// around it, and prose after one hangs at the text edge like the
    /// paragraph's own wrapped lines. Text without formulas is untouched.
    static func applyMathParagraphs(
        _ text: NSMutableAttributedString, in range: NSRange, base: NSParagraphStyle
    ) {
        var hasMath = false
        text.enumerateAttribute(.cypherMathSource, in: range) { value, _, stop in
            if value != nil {
                hasMath = true
                stop.pointee = true
            }
        }
        guard hasMath else { return }
        let string = text.string as NSString
        var at = range.location
        var first = true
        while at < NSMaxRange(range) {
            let paragraph = NSIntersectionRange(
                string.paragraphRange(for: NSRange(location: at, length: 0)), range)
            guard paragraph.length > 0 else { break }
            var display = false
            text.enumerateAttribute(.cypherDisplayMath, in: paragraph) { value, _, stop in
                if value != nil {
                    display = true
                    stop.pointee = true
                }
            }
            let style = (base.mutableCopy() as? NSMutableParagraphStyle) ?? NSMutableParagraphStyle()
            style.maximumLineHeight = 0
            if !first {
                style.firstLineHeadIndent = base.headIndent
                style.paragraphSpacingBefore = 0
            }
            if display {
                style.alignment = .center
                style.tabStops = []
                if !first { style.paragraphSpacingBefore = MD.blockGap }
                if NSMaxRange(paragraph) < NSMaxRange(range) { style.paragraphSpacing = MD.blockGap }
            }
            text.addAttribute(.paragraphStyle, value: style, range: paragraph)
            at = NSMaxRange(paragraph)
            first = false
        }
    }

    /// Thinking's quieter tone: body text and inline code (which reads in
    /// the body ink) take the muted color; links and mentions keep theirs.
    static func muted(_ text: NSAttributedString) -> NSAttributedString {
        let result = NSMutableAttributedString(attributedString: text)
        var plain: [NSRange] = []
        result.enumerateAttributes(in: NSRange(location: 0, length: result.length)) { attributes, range, _ in
            if attributes[.link] == nil, attributes[.cypherMentionWash] == nil { plain.append(range) }
        }
        let color = UIColor(Theme.textMuted)
        for range in plain { result.addAttribute(.foregroundColor, value: color, range: range) }
        return result
    }

    /// Fade the streamed tail: `veil`'s segments are character offsets into
    /// `result`, and only text colors change, never the layout.
    static func applyVeil(_ veil: RowVeil, to result: NSMutableAttributedString) {
        let characters = Array(result.string)
        for segment in veil.segments(totalLength: characters.count) where segment.alpha < 1 {
            let lower = max(0, segment.range.lowerBound)
            let upper = min(characters.count, segment.range.upperBound)
            guard lower < upper else { continue }
            let start = String(characters[..<lower]).utf16.count
            let length = String(characters[lower..<upper]).utf16.count
            let range = NSRange(location: start, length: length)
            var colors: [(NSRange, UIColor)] = []
            result.enumerateAttribute(.foregroundColor, in: range) { value, part, _ in
                if let color = value as? UIColor {
                    colors.append((part, color.withAlphaComponent(color.cgColor.alpha * segment.alpha)))
                }
            }
            for (part, color) in colors { result.addAttribute(.foregroundColor, value: color, range: part) }
        }
    }

    // MARK: Prose (consecutive blocks as one selectable text)

    /// Whether a top-level block joins its neighbours in one selectable
    /// text: paragraphs, headings, and lists holding only those (nested
    /// lists included). Code blocks, tables, quotes and rules keep their own
    /// views, so a selection still stops at them.
    nonisolated static func isProse(_ block: MDBlock) -> Bool {
        switch block {
        case .paragraph, .heading:
            return true
        case .list(_, let items):
            return items.allSatisfy { $0.children.allSatisfy(isProse) }
        case .codeBlock, .artifact, .blockquote, .table, .rule, .image:
            return false
        }
    }

    /// Indent from a list's edge to its items' text (ListBlockView: an 18pt
    /// marker column and 8pt to the text).
    static let listIndent: CGFloat = 26
    /// Between a list's items, and between the blocks inside one item.
    static let listItemGap: CGFloat = 4

    /// `blocks` (all `isProse`) as one text, spaced and indented as their
    /// separate views were: the block gap between top-level blocks, list
    /// markers in the accent with wrapped lines hanging on the item's text.
    static func prose(_ blocks: [MDBlock]) -> NSAttributedString {
        let result = NSMutableAttributedString()
        for block in blocks {
            appendProse(block, to: result, indent: 0, spacingBefore: MD.blockGap)
        }
        return result
    }

    private static func appendProse(
        _ block: MDBlock, to result: NSMutableAttributedString,
        indent: CGFloat, spacingBefore: CGFloat, marker: String? = nil,
        markerColor: UIColor? = nil
    ) {
        switch block {
        case .paragraph(let runs):
            appendParagraph(
                inline(runs), lineHeight: MD.lineHeight, to: result, indent: indent,
                spacingBefore: spacingBefore, marker: marker, markerColor: markerColor)
        case .heading(let level, let runs):
            let m = MD.headingMetrics(level)
            appendParagraph(
                inline(runs, size: m.size, weight: .semibold, lineHeight: m.line),
                lineHeight: m.line, to: result, indent: indent,
                spacingBefore: spacingBefore, marker: marker, markerColor: markerColor)
        case .list(let start, let items):
            for (index, item) in items.enumerated() {
                let gap = index == 0 ? spacingBefore : listItemGap
                let itemMarker: String
                let color: UIColor
                if let checked = item.checked {
                    itemMarker = checked ? "\u{2611}" : "\u{2610}"
                    color = UIColor(checked ? Theme.accent.opacity(0.85) : Theme.textMuted)
                } else {
                    itemMarker = start.map { "\($0 + index)." } ?? "\u{2022}"
                    color = UIColor(Theme.accent.opacity(0.85))
                }
                guard let first = item.children.first else {
                    appendParagraph(
                        NSAttributedString(), lineHeight: MD.lineHeight, to: result, indent: indent,
                        spacingBefore: gap, marker: itemMarker, markerColor: color)
                    continue
                }
                // The marker rides the item's first line of text; an item that
                // opens with a nested list gets a line of its own for it.
                if case .list = first {
                    appendParagraph(
                        NSAttributedString(), lineHeight: MD.lineHeight, to: result, indent: indent,
                        spacingBefore: gap, marker: itemMarker, markerColor: color)
                    appendProse(first, to: result, indent: indent + listIndent, spacingBefore: listItemGap)
                } else {
                    appendProse(
                        first, to: result, indent: indent, spacingBefore: gap,
                        marker: itemMarker, markerColor: color)
                }
                for child in item.children.dropFirst() {
                    appendProse(child, to: result, indent: indent + listIndent, spacingBefore: listItemGap)
                }
            }
        case .codeBlock, .artifact, .blockquote, .table, .rule, .image:
            // Not prose: callers only pass `isProse` blocks.
            break
        }
    }

    /// One paragraph of `text`, after a line break when the text so far isn't
    /// empty. With a `marker` the paragraph is a list item's first: the
    /// marker at `indent`, the text from the list's text edge, and wrapped
    /// lines hanging there too.
    private static func appendParagraph(
        _ text: NSAttributedString, lineHeight: CGFloat,
        to result: NSMutableAttributedString, indent: CGFloat,
        spacingBefore: CGFloat, marker: String?, markerColor: UIColor?
    ) {
        let style = NSMutableParagraphStyle()
        style.minimumLineHeight = lineHeight
        style.maximumLineHeight = lineHeight
        style.paragraphSpacingBefore = result.length == 0 ? 0 : spacingBefore
        style.firstLineHeadIndent = indent
        style.headIndent = marker == nil ? indent : indent + listIndent
        if marker != nil {
            style.tabStops = [NSTextTab(textAlignment: .left, location: indent + listIndent)]
        }
        if result.length > 0 {
            // The break belongs to the paragraph before it (but is not part
            // of a formula, inline code, a chip, a mention or a link that
            // ends it: their boxes and copies would take the break in).
            var previous = result.attributes(at: result.length - 1, effectiveRange: nil)
            for key in [
                NSAttributedString.Key.attachment, .cypherMathSource, .cypherDisplayMath, .cypherInlineCode,
                .cypherMentionWash, .cypherFileChip, .cypherFileRef, .cypherFileGlyph, .link, .underlineStyle, .kern,
            ] {
                previous[key] = nil
            }
            result.append(NSAttributedString(string: "\n", attributes: previous))
        }
        let start = result.length
        if let marker {
            result.append(
                NSAttributedString(
                    string: "\(marker)\t",
                    attributes: [
                        .font: Theme.sansUI(MD.textSize, weight: .regular),
                        .foregroundColor: markerColor ?? UIColor(Theme.accent.opacity(0.85)),
                    ]))
        }
        result.append(text)
        let range = NSRange(location: start, length: result.length - start)
        result.addAttribute(.paragraphStyle, value: style, range: range)
        applyMathParagraphs(result, in: range, base: style)
    }

    static func code(
        _ source: String, spans: [[TokenSpan]],
        size: CGFloat = MD.codeTextSize
    ) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = MD.codeLineHeight
        paragraph.maximumLineHeight = MD.codeLineHeight
        let result = NSMutableAttributedString(
            string: source,
            attributes: [
                .font: Theme.monoUI(size),
                .foregroundColor: UIColor(Theme.text.opacity(0.9)),
                .paragraphStyle: paragraph,
            ])
        var offset = 0
        for (index, line) in source.components(separatedBy: "\n").enumerated() {
            let characters = Array(line)
            for span in index < spans.count ? spans[index] : [] {
                let lower = span.range.lowerBound
                let upper = min(span.range.upperBound, characters.count)
                guard lower >= 0, lower < upper else { continue }
                let range = NSRange(
                    location: offset + String(characters[..<lower]).utf16.count,
                    length: String(characters[lower..<upper]).utf16.count)
                let color: Color
                switch span.cls {
                case .keyword: color = Theme.tokenKeyword
                case .stringLit: color = Theme.tokenString
                case .number: color = Theme.tokenNumber
                case .comment: color = Theme.textFaint
                }
                result.addAttribute(.foregroundColor, value: UIColor(color), range: range)
            }
            offset += line.utf16.count + 1
        }
        return result
    }
}

/// Preserve the rounded inline-code wash and file reference chips without a
/// second text overlay.
final class TranscriptTextLayoutManager: NSLayoutManager {
    /// Image chips' thumbnails, by the chip's inline code.
    var thumbnails: [String: UIImage] = [:]

    override init() {
        super.init()
        delegate = FileReferenceChipLineBreaks.shared
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        delegate = FileReferenceChipLineBreaks.shared
    }

    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        if let storage = textStorage, let container = textContainers.first {
            let characters = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
            let thumbnails = thumbnails
            FileReferenceChip.paint(
                self, storage: storage, container: container, characters: characters, origin: origin,
                thumbnail: { thumbnails[$0] })
            InlineCodeBox.paint(self, storage: storage, container: container, characters: characters, origin: origin)
            paintMentions(storage: storage, container: container, characters: characters, origin: origin)
        }
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
    }

    /// A sent prompt's mention chips: their wash over the glyphs' line
    /// rects, as before inline code had its own box.
    private func paintMentions(
        storage: NSTextStorage, container: NSTextContainer, characters: NSRange, origin: CGPoint
    ) {
        storage.enumerateAttribute(.cypherMentionWash, in: characters) { value, range, _ in
            guard let color = value as? UIColor else { return }
            let glyphs = glyphRange(forCharacterRange: range, actualCharacterRange: nil)
            enumerateEnclosingRects(
                forGlyphRange: glyphs,
                withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                in: container
            ) { rect, _ in
                // The native text view's glyph bounds place this wash
                // slightly above the visible ink; nudge it down.
                let rect = rect.offsetBy(dx: origin.x, dy: origin.y + 2.5).insetBy(dx: -2, dy: 2)
                color.setFill()
                UIBezierPath(roundedRect: rect, cornerRadius: MD.mentionRadius).fill()
            }
        }
    }
}

/// Lays transcript text out off screen to measure it, set up as the text
/// views are (no line fragment padding, no insets), so a height matches what
/// a view of that width lays out. Measuring a view directly resizes its text
/// container and back, and TextKit throws the view's whole layout away twice
/// over for it.
@MainActor
final class TranscriptTextMeasurer {
    static let shared = TranscriptTextMeasurer()

    private let storage = NSTextStorage()
    private let layout = TranscriptTextLayoutManager()
    private let container = NSTextContainer(size: .zero)
    /// How many layouts it has run (tests).
    private(set) var layouts = 0

    init() {
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
    }

    /// The height `text` lays out to in `width`.
    func height(of text: NSAttributedString, width: CGFloat) -> CGFloat {
        layouts += 1
        container.size = CGSize(width: width, height: .greatestFiniteMagnitude)
        storage.setAttributedString(text)
        layout.ensureLayout(for: container)
        let height = layout.usedRect(for: container).height
        // Hold no reply's text (or formula images) between measurements.
        storage.setAttributedString(NSAttributedString())
        return height
    }
}

final class TranscriptUITextView: UITextView {
    var selectionEnded: (() -> Void)?

    /// Formulas copy as their TeX source; everything else as UIKit copies.
    override func copy(_ sender: Any?) {
        if let attributedText, let text = MathRenderer.plainText(attributedText, range: selectedRange) {
            UIPasteboard.general.string = text
            return
        }
        super.copy(sender)
    }

    override func resignFirstResponder() -> Bool {
        let result = super.resignFirstResponder()
        if result {
            selectedRange = NSRange(location: 0, length: 0)
            selectionEnded?()
        }
        return result
    }
}

struct SelectableTranscriptText: UIViewRepresentable {
    let attributed: NSAttributedString
    var wraps = true
    var hugsContent = false
    @Environment(\.commentDrafts) private var drafts
    @Environment(\.transcriptEntry) private var entry
    @Environment(\.transcriptSelectionActions) private var actions
    @Environment(\.openWorkspace) private var openWorkspace
    @Environment(\.fileReferenceContext) private var referenceContext
    @Environment(\.fileReferenceSnippet) private var referenceSnippet
    @Environment(\.openBotChat) private var openSession
    @Environment(\.transcriptImageFiles) private var imageFiles

    func makeUIView(context: Context) -> TranscriptUITextView {
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
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.dataDetectorTypes = []
        // Links carry their own look (underlined text; a chip's box, glyph,
        // name and muted lines), so the view adds none.
        view.linkTextAttributes = [:]
        view.tintColor = UIColor(Theme.accent)
        // Repaint custom inline-code backgrounds as well as glyphs on a live
        // appearance change. Do not replace text or clear an active selection.
        view.registerForTraitChanges([UITraitUserInterfaceStyle.self]) {
            (view: TranscriptUITextView, _: UITraitCollection) in
            view.layoutManager.invalidateDisplay(
                forCharacterRange: NSRange(location: 0, length: view.textStorage.length))
            view.setNeedsDisplay()
        }
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        view.selectionEnded = { [weak view, weak coordinator = context.coordinator] in
            if let view { coordinator?.applyLatest(to: view) }
        }
        context.coordinator.entry = entry
        context.coordinator.actions = actions
        context.coordinator.files = .init(
            open: openWorkspace, context: referenceContext, snippet: referenceSnippet)
        context.coordinator.openSession = openSession
        context.coordinator.imageFiles = imageFiles
        context.coordinator.update(view, attributed: attributed, drafts: drafts)
        return view
    }

    func updateUIView(_ view: TranscriptUITextView, context: Context) {
        context.coordinator.entry = entry
        context.coordinator.actions = actions
        context.coordinator.files = .init(
            open: openWorkspace, context: referenceContext, snippet: referenceSnippet)
        context.coordinator.openSession = openSession
        context.coordinator.imageFiles = imageFiles
        context.coordinator.update(view, attributed: attributed, drafts: drafts)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: TranscriptUITextView, context: Context) -> CGSize? {
        context.coordinator.size(of: uiView, proposedWidth: proposal.width, wraps: wraps, hugsContent: hugsContent)
    }

    func makeCoordinator() -> TranscriptSelectionCoordinator { TranscriptSelectionCoordinator() }
}

final class TranscriptSelectionCoordinator: NSObject, UITextViewDelegate {
    var entry: TranscriptEntryContext?
    var actions: TranscriptSelectionActions?
    var files = TranscriptFiles()
    /// Where a `cypher://session/<id>` link opens its session.
    var openSession: ((String) -> Void)?
    /// Where image chips' thumbnails are read.
    var imageFiles: TranscriptImageFiles?
    private var thumbnailLoads: Set<String> = []

    /// What file chips and file links reach: the session's Files sheet.
    struct TranscriptFiles {
        var open: ((WorkspaceDestination) -> Void)?
        var context: ((String) -> [String])?
        var snippet: ((WorkspaceReference) async throws -> FileReferenceSnippet)?
    }
    private var latest: NSAttributedString?
    private weak var drafts: CommentDrafts?
    private var applying = false
    /// The text the view shows, as UIKit holds it; nil until measured since
    /// it changed. An active selection freezes only this block on its old
    /// text, and the block is measured by what it shows, so the selection
    /// handles never drift.
    private var displayed: NSAttributedString?
    /// `displayed`'s width on one line, once something asked for it.
    private var naturalWidth: CGFloat?
    /// `displayed`'s height by layout width. The lazy transcript measures a
    /// row many times over (scrolling, re-anchoring at the bottom, a scene
    /// update going to the background), and laying out every long reply
    /// again each time held the main thread past the system's watchdog.
    private var heights: [CGFloat: CGFloat] = [:]

    /// The size of the text `view` shows for a proposed width: the offered
    /// width, or its one-line width where it does not wrap (or hugs content
    /// narrower than offered). Remembered until the text changes.
    func size(of view: UITextView, proposedWidth: CGFloat?, wraps: Bool, hugsContent: Bool) -> CGSize {
        let displayed = displayed ?? NSAttributedString(attributedString: view.textStorage)
        self.displayed = displayed
        let offered = proposedWidth.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
        let width: CGFloat
        if wraps, !hugsContent, let offered {
            width = max(1, offered)
        } else {
            let natural = natural(of: displayed)
            let offered = offered ?? natural
            width = max(1, wraps ? min(offered, natural) : natural)
        }
        if let height = heights[width] { return CGSize(width: width, height: height) }
        let height = ceil(TranscriptTextMeasurer.shared.height(of: displayed, width: width))
        // A rotation or split view brings a few widths; never a lot of them.
        if heights.count >= 8 { heights.removeAll() }
        heights[width] = height
        return CGSize(width: width, height: height)
    }

    private func natural(of displayed: NSAttributedString) -> CGFloat {
        if let naturalWidth { return naturalWidth }
        let width =
            ceil(
                displayed.boundingRect(
                    with: CGSize(width: 1_000_000, height: 1_000_000),
                    options: [.usesLineFragmentOrigin, .usesFontLeading],
                    context: nil
                ).width) + 1
        naturalWidth = width
        return width
    }

    func update(_ view: UITextView, attributed: NSAttributedString, drafts: CommentDrafts?) {
        latest = attributed
        self.drafts = drafts
        applyLatest(to: view)
    }

    func applyLatest(to view: UITextView) {
        guard !applying, view.selectedRange.length == 0, let latest,
            !(view.attributedText?.isEqual(to: latest) ?? false)
        else { return }
        applying = true
        view.attributedText = latest
        displayed = nil
        naturalWidth = nil
        heights.removeAll()
        view.selectedRange = NSRange(location: 0, length: 0)
        view.invalidateIntrinsicContentSize()
        applying = false
        loadThumbnails(in: view)
    }

    /// Give each chip naming an image file its thumbnail, once loaded; the
    /// chip shows its file glyph until then (and if it can't be read).
    private func loadThumbnails(in view: UITextView) {
        guard let files = imageFiles, let layout = view.layoutManager as? TranscriptTextLayoutManager else { return }
        let storage = view.textStorage
        var raws: [String] = []
        storage.enumerateAttribute(.cypherFileRef, in: NSRange(location: 0, length: storage.length)) { value, _, _ in
            if let raw = value as? String, !raw.isEmpty, !raws.contains(raw), FileReferenceChip.namesImage(raw) {
                raws.append(raw)
            }
        }
        for raw in raws where layout.thumbnails[raw] == nil && !thumbnailLoads.contains(raw) {
            let path = FileReference.detect(raw)?.path ?? raw
            if let hit = ImageLoader.shared.cached(path, files: files, size: .thumbnail)?.bitmap {
                layout.thumbnails[raw] = hit
                continue
            }
            thumbnailLoads.insert(raw)
            Task { @MainActor [weak view, weak layout] in
                let result = await ImageLoader.shared.load(path, files: files, size: .thumbnail)
                guard case .success(let image) = result, let bitmap = image.bitmap, let layout, let view else { return }
                layout.thumbnails[raw] = bitmap
                layout.invalidateDisplay(forCharacterRange: NSRange(location: 0, length: view.textStorage.length))
                view.setNeedsDisplay()
            }
        }
    }

    func textViewDidChangeSelection(_ textView: UITextView) {
        if textView.selectedRange.length == 0 { applyLatest(to: textView) }
    }

    /// The selected text, formulas as their TeX source.
    private func quote(in textView: UITextView, range: NSRange) -> String {
        textView.attributedText.flatMap { MathRenderer.plainText($0, range: range) }
            ?? CommentPrompt.selectedText(textView.text ?? "", range: range)
    }

    func commentAction(in textView: UITextView, range: NSRange) -> UIAction? {
        guard let drafts, drafts.owner != nil else { return nil }
        let quote = quote(in: textView, range: range)
        guard !CommentPrompt.normalize(quote).isEmpty else { return nil }
        let generation = drafts.generation
        return UIAction(title: "Comment", image: UIImage(systemName: "text.bubble")) { [weak drafts, weak textView] _ in
            guard let drafts, drafts.generation == generation else { return }
            textView?.selectedRange = NSRange(location: 0, length: 0)
            textView?.resignFirstResponder()
            drafts.begin(quote: quote)
        }
    }

    /// Fork (desktop: "Fork before this message" on a prompt, "after this
    /// response" on a reply). A prompt's fork hands its text back to edit.
    func forkAction(in textView: UITextView) -> UIAction? {
        guard let entry, entry.settled, entry.role != .system, let fork = actions?.fork else { return nil }
        let title = entry.role == .user ? "Edit in Fork" : "Fork from Here"
        return UIAction(title: title, image: UIImage(systemName: "arrow.triangle.branch")) { [weak textView] _ in
            textView?.selectedRange = NSRange(location: 0, length: 0)
            textView?.resignFirstResponder()
            fork(entry)
        }
    }

    /// A temporary side chat about the selected text.
    func sideChatAction(in textView: UITextView, range: NSRange) -> UIAction? {
        guard let entry, let sideChat = actions?.sideChat else { return nil }
        let quote = quote(in: textView, range: range)
        guard !CommentPrompt.normalize(quote).isEmpty else { return nil }
        return UIAction(title: "Side Chat", image: UIImage(systemName: "bubble.left.and.bubble.right")) {
            [weak textView] _ in
            textView?.selectedRange = NSRange(location: 0, length: 0)
            textView?.resignFirstResponder()
            sideChat(entry, quote)
        }
    }

    /// The file a chip or a file link opens; nil for any other link.
    private func reference(_ textItem: UITextItem) -> WorkspaceReference? {
        guard case .link(let url) = textItem.content else { return nil }
        let context = entry.flatMap { entry in files.context?(entry.entryId) } ?? []
        return TranscriptFileLinks.reference(url, context: context)
    }

    /// A tap on a chip or a file link opens the file in Files; other links
    /// open as before. With no Files to open (a side chat) a chip does
    /// nothing, and a file link — no URL the system can open — neither.
    func textView(
        _ textView: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction
    ) -> UIAction? {
        // A session link opens in-app, never through the system.
        if case .link(let url) = textItem.content, SessionLink.isSessionLink(url) {
            guard let id = SessionLink.chatId(url), let openSession else { return nil }
            return UIAction { _ in openSession(id) }
        }
        guard let reference = reference(textItem) else { return defaultAction }
        guard let open = files.open else { return nil }
        return UIAction { _ in open(.reference(reference)) }
    }

    /// A long press on a chip or a file link: its cited lines as they are
    /// in the checkout now, with Open in Files and Copy.
    func textView(
        _ textView: UITextView, menuConfigurationFor textItem: UITextItem, defaultMenu: UIMenu
    ) -> UITextItem.MenuConfiguration? {
        guard let reference = reference(textItem), case .link(let url) = textItem.content else {
            return UITextItem.MenuConfiguration(menu: defaultMenu)
        }
        var actions: [UIMenuElement] = []
        if let open = files.open {
            actions.append(
                UIAction(title: "Open in Files", image: UIImage(systemName: "folder")) { _ in
                    open(.reference(reference))
                })
        }
        let written = TranscriptFileLinks.written(url) ?? reference.path
        actions.append(
            UIAction(title: "Copy", image: UIImage(systemName: "doc.on.doc")) { _ in
                UIPasteboard.general.string = written
            })
        let menu = UIMenu(children: actions)
        guard let snippet = files.snippet else { return UITextItem.MenuConfiguration(menu: menu) }
        let card: UIView = UIHostingController(
            rootView: FileReferencePreviewCard(reference: reference, load: snippet)
        ).view
        card.frame = CGRect(
            x: 0, y: 0, width: FileReferencePreviewCard.width,
            height: FileReferencePreviewCard.height(for: reference))
        card.backgroundColor = UIColor(Theme.sheetPanel)
        return UITextItem.MenuConfiguration(preview: .view(card), menu: menu)
    }

    func textView(
        _ textView: UITextView, editMenuForTextIn range: NSRange,
        suggestedActions: [UIMenuElement]
    ) -> UIMenu? {
        let ours: [UIMenuElement] = [
            commentAction(in: textView, range: range),
            sideChatAction(in: textView, range: range),
            forkAction(in: textView),
        ].compactMap { $0 }
        return UIMenu(children: ours + suggestedActions)
    }
}

/// A tap inside native selected text must not trigger the transcript's
/// blanket keyboard-dismiss action, or it would destroy the selection.
@MainActor
enum TranscriptKeyboardDismissal {
    static weak var responder: UIResponder?
    static func dismiss(at point: CGPoint) {
        responder = nil
        UIApplication.shared.sendAction(#selector(UIResponder.cypherCaptureResponder), to: nil, from: nil, for: nil)
        if let view = responder as? TranscriptUITextView, let window = view.window,
            view.bounds.contains(view.convert(point, from: window))
        {
            responder = nil
            return
        }
        responder?.resignFirstResponder()
        responder = nil
    }
}

extension UIResponder {
    @objc func cypherCaptureResponder() { TranscriptKeyboardDismissal.responder = self }
}

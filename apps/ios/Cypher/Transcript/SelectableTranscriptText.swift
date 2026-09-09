// Native selection in-place, sharing one text layout for painting, hit tests
// and selection handles. The outer SwiftUI transcript remains virtualized.
import SwiftUI
import UIKit

extension NSAttributedString.Key {
    static let cypherInlineCode = NSAttributedString.Key("cypher.inlineCode")
}

@MainActor
enum TranscriptTextStyle {
    static func inline(_ runs: [InlineRun], size: CGFloat = MD.textSize,
                       weight: UIFont.Weight = .regular, lineHeight: CGFloat = MD.lineHeight,
                       veil: RowVeil? = nil) -> NSAttributedString {
        let result = NSMutableAttributedString(string: "")
        for run in runs {
            var font = run.style.code ? Theme.monoUI(size - 1.5)
                : Theme.sansUI(size, weight: run.style.bold ? .semibold : weight)
            if run.style.italic,
               let descriptor = font.fontDescriptor.withSymbolicTraits(font.fontDescriptor.symbolicTraits.union(.traitItalic)) {
                font = UIFont(descriptor: descriptor, size: font.pointSize)
            }
            var attributes: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: UIColor(run.style.code ? Theme.inlineCodeText : Theme.text),
            ]
            if run.style.code { attributes[.cypherInlineCode] = UIColor(Theme.inlineCodeWash) }
            if run.style.strikethrough { attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
            if let link = run.style.link, let url = URL(string: link) {
                attributes[.link] = url
                attributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
            }
            result.append(NSAttributedString(string: run.text, attributes: attributes))
        }
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = lineHeight
        paragraph.maximumLineHeight = lineHeight
        result.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: result.length))
        if let veil {
            let characters = Array(result.string)
            for segment in veil.segments(totalLength: characters.count) where segment.alpha < 1 {
                let lower = max(0, segment.range.lowerBound), upper = min(characters.count, segment.range.upperBound)
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
        return result
    }

    static func code(_ source: String, spans: [[TokenSpan]]) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = MD.codeLineHeight
        paragraph.maximumLineHeight = MD.codeLineHeight
        let result = NSMutableAttributedString(string: source, attributes: [
            .font: Theme.monoUI(MD.codeTextSize),
            .foregroundColor: UIColor(Theme.text.opacity(0.9)),
            .paragraphStyle: paragraph,
        ])
        var offset = 0
        for (index, line) in source.components(separatedBy: "\n").enumerated() {
            let characters = Array(line)
            for span in index < spans.count ? spans[index] : [] {
                let lower = span.range.lowerBound, upper = min(span.range.upperBound, characters.count)
                guard lower >= 0, lower < upper else { continue }
                let range = NSRange(location: offset + String(characters[..<lower]).utf16.count,
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

/// Preserve the rounded inline-code wash without a second text overlay.
final class TranscriptTextLayoutManager: NSLayoutManager {
    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        if let storage = textStorage, let container = textContainers.first {
            let characters = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
            storage.enumerateAttribute(.cypherInlineCode, in: characters) { value, range, _ in
                guard let color = value as? UIColor else { return }
                let glyphs = self.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
                self.enumerateEnclosingRects(forGlyphRange: glyphs,
                    withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                    in: container) { rect, _ in
                    // The native text view's glyph bounds place this wash
                    // slightly above the visible ink. Unlike the SwiftUI
                    // renderer, this path is used by the actual transcript
                    // (including selectable Markdown), so keep the correction
                    // here as well.
                    let rect = rect.offsetBy(dx: origin.x, dy: origin.y + 2.5)
                        .insetBy(dx: -2, dy: 2)
                    color.setFill()
                    UIBezierPath(roundedRect: rect, cornerRadius: MD.inlineCodeRadius).fill()
                }
            }
        }
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
    }
}

final class TranscriptUITextView: UITextView {
    var selectionEnded: (() -> Void)?
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
        view.linkTextAttributes = [.foregroundColor: UIColor(Theme.text),
                                   .underlineStyle: NSUnderlineStyle.single.rawValue]
        view.tintColor = UIColor(Theme.accent)
        // Repaint custom inline-code backgrounds as well as glyphs on a live
        // appearance change. Do not replace text or clear an active selection.
        view.registerForTraitChanges([UITraitUserInterfaceStyle.self]) { (view: TranscriptUITextView, _: UITraitCollection) in
            view.layoutManager.invalidateDisplay(forCharacterRange: NSRange(location: 0, length: view.textStorage.length))
            view.setNeedsDisplay()
        }
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        view.selectionEnded = { [weak view, weak coordinator = context.coordinator] in
            if let view { coordinator?.applyLatest(to: view) }
        }
        context.coordinator.update(view, attributed: attributed, drafts: drafts)
        return view
    }

    func updateUIView(_ view: TranscriptUITextView, context: Context) {
        context.coordinator.update(view, attributed: attributed, drafts: drafts)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: TranscriptUITextView, context: Context) -> CGSize? {
        // An active selection freezes only this text block, not the session.
        // Measure what is actually displayed so its handles never drift.
        let displayed = uiView.attributedText ?? attributed
        let natural = ceil(displayed.boundingRect(with: CGSize(width: 1_000_000, height: 1_000_000),
                                                  options: [.usesLineFragmentOrigin, .usesFontLeading],
                                                  context: nil).width) + 1
        let offered = proposal.width.flatMap { $0.isFinite && $0 > 0 ? $0 : nil } ?? natural
        let width = max(1, wraps ? (hugsContent ? min(offered, natural) : offered) : natural)
        let measured = uiView.sizeThatFits(CGSize(width: width, height: CGFloat.greatestFiniteMagnitude))
        return CGSize(width: width, height: ceil(measured.height))
    }

    func makeCoordinator() -> TranscriptSelectionCoordinator { TranscriptSelectionCoordinator() }
}

final class TranscriptSelectionCoordinator: NSObject, UITextViewDelegate {
    private var latest: NSAttributedString?
    private weak var drafts: CommentDrafts?
    private var applying = false

    func update(_ view: UITextView, attributed: NSAttributedString, drafts: CommentDrafts?) {
        latest = attributed
        self.drafts = drafts
        applyLatest(to: view)
    }

    func applyLatest(to view: UITextView) {
        guard !applying, view.selectedRange.length == 0, let latest,
              !(view.attributedText?.isEqual(to: latest) ?? false) else { return }
        applying = true
        view.attributedText = latest
        view.selectedRange = NSRange(location: 0, length: 0)
        view.invalidateIntrinsicContentSize()
        applying = false
    }

    func textViewDidChangeSelection(_ textView: UITextView) {
        if textView.selectedRange.length == 0 { applyLatest(to: textView) }
    }

    func commentAction(in textView: UITextView, range: NSRange) -> UIAction? {
        guard let drafts, drafts.owner != nil else { return nil }
        let quote = CommentPrompt.selectedText(textView.text ?? "", range: range)
        guard !CommentPrompt.normalize(quote).isEmpty else { return nil }
        let generation = drafts.generation
        return UIAction(title: "Comment", image: UIImage(systemName: "text.bubble")) { [weak drafts, weak textView] _ in
            guard let drafts, drafts.generation == generation else { return }
            textView?.selectedRange = NSRange(location: 0, length: 0)
            textView?.resignFirstResponder()
            drafts.begin(quote: quote)
        }
    }

    func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                  suggestedActions: [UIMenuElement]) -> UIMenu? {
        guard let comment = commentAction(in: textView, range: range) else {
            return UIMenu(children: suggestedActions)
        }
        return UIMenu(children: [comment] + suggestedActions)
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
           view.bounds.contains(view.convert(point, from: window)) {
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

import SwiftUI
import UIKit

/// Focus belongs to our editor, not SwiftUI's private TextField backing view
/// or the keyboard (which may belong to a picker/search field).
@MainActor @Observable
final class ComposerFocus {
    var isFocused = false
    @ObservationIgnored private var editor: UUID?

    func attach(_ id: UUID) { editor = id }
    func owns(_ id: UUID) -> Bool { editor == id }
    func changed(_ focused: Bool, from id: UUID) {
        guard owns(id) else { return }
        isFocused = focused
    }
    func detach(_ id: UUID) {
        if owns(id) { editor = nil }
    }
}

/// The `@` completion's link between a composer and its native editor: the
/// editor publishes the `@query` under the caret, and a picked row asks it
/// to splice the mention's link in place of that token.
@MainActor @Observable
final class MentionEditor {
    private(set) var token: MentionToken?
    @ObservationIgnored fileprivate var splice: ((MentionToken, String) -> Void)?

    func publish(_ token: MentionToken?) {
        if self.token != token { self.token = token }
    }

    /// Replace the current token with `link` (raw mention markup).
    func accept(link: String) {
        guard let token, let splice else { return }
        splice(token, link)
    }
}

extension NSAttributedString.Key {
    /// A mention chip's raw markup. One object per chip, so two adjacent
    /// chips stay two runs.
    static let cypherMention = NSAttributedString.Key("cypher.mention")
}

/// A chip's identity in the editor's text storage: its markup, and the
/// text it shows (anything else means an edit damaged it).
final class MentionChipValue: NSObject {
    let raw: String
    let display: String
    init(raw: String, display: String) {
        self.raw = raw
        self.display = display
    }
}

/// The composer's chips read as inline code: the label in mono over a
/// rounded wash (composer.rs paints the same wash under its chips).
final class ComposerLayoutManager: NSLayoutManager {
    override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
        if let storage = textStorage, let container = textContainers.first {
            let characters = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
            storage.enumerateAttribute(.cypherInlineCode, in: characters) { value, range, _ in
                guard let color = value as? UIColor else { return }
                let glyphs = self.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
                self.enumerateEnclosingRects(forGlyphRange: glyphs,
                    withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                    in: container) { rect, _ in
                    let rect = rect.offsetBy(dx: origin.x, dy: origin.y).insetBy(dx: 0, dy: 1)
                    color.setFill()
                    UIBezierPath(roundedRect: rect, cornerRadius: MD.inlineCodeRadius).fill()
                }
            }
        }
        super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
    }
}

/// Own the UITextView and its delegate outright. SwiftUI's multiline
/// TextField can put its accessibility ID on a wrapper rather than the view
/// that sends editing notifications, and FocusState has not been reliable
/// inside the session's safeAreaBar on physical devices.
///
/// `text` is the RAW draft: mention links stay strict Markdown there, and
/// the editor shows each as an `@name` chip that edits as one unit.
struct ComposerTextInput: UIViewRepresentable {
    @Binding var text: String
    let focus: ComposerFocus
    let editorID: String
    let enabled: Bool
    var placeholder = "Message"
    /// Bumped when the draft is replaced wholesale (a picked slash command):
    /// the caret moves to the end instead of keeping its old offset.
    var caretToEnd = 0
    /// `@` completion; nil leaves chips display-only.
    var mentions: MentionEditor? = nil

    static let fontSize: CGFloat = 16

    func makeCoordinator() -> Coordinator { Coordinator(text: $text, focus: focus) }

    func makeUIView(context: Context) -> UITextView {
        // TextKit 1, for the chips' rounded wash.
        let storage = NSTextStorage()
        let layout = ComposerLayoutManager()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: CGSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        container.widthTracksTextView = true
        layout.addTextContainer(container)
        let view = UITextView(frame: .zero, textContainer: container)
        view.backgroundColor = .clear
        view.font = Theme.sansUI(Self.fontSize)
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.isScrollEnabled = true
        view.alwaysBounceVertical = false
        view.bounces = false
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.typingAttributes = Coordinator.baseAttributes
        view.delegate = context.coordinator
        context.coordinator.attach()
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.text = $text
        coordinator.mentions = mentions
        mentions?.splice = { [weak coordinator, weak view] token, link in
            guard let coordinator, let view else { return }
            coordinator.splice(view, token: token, link: link)
        }
        view.accessibilityIdentifier = editorID
        view.accessibilityLabel = placeholder
        view.tintColor = UIColor(Theme.text)
        view.isEditable = enabled
        // Never replace native marked text during Chinese/Japanese input.
        // Accepted sends retire the entire editor via editorRevision instead.
        if coordinator.raw(of: view) != text, view.markedTextRange == nil {
            let selection = view.selectedRange
            coordinator.render(text, in: view)
            let length = view.textStorage.length
            let start = min(selection.location, length)
            view.selectedRange = NSRange(location: start, length: min(selection.length, length - start))
            view.invalidateIntrinsicContentSize()
        }
        if coordinator.caretToEnd != caretToEnd {
            coordinator.caretToEnd = caretToEnd
            if view.markedTextRange == nil {
                view.selectedRange = NSRange(location: view.textStorage.length, length: 0)
            }
        }
        coordinator.reconcileFocus(view, enabled: enabled)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        guard let width = proposal.width, width.isFinite, width > 0 else { return nil }
        let line = uiView.font?.lineHeight ?? 20
        let measured = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: min(ceil(line * 7), max(ceil(line), ceil(measured.height))))
    }

    static func dismantleUIView(_ view: UITextView, coordinator: Coordinator) {
        coordinator.detach()
        coordinator.mentions?.publish(nil)
        view.delegate = nil
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var text: Binding<String>
        let focus: ComposerFocus
        let id = UUID()
        var caretToEnd = 0
        var mentions: MentionEditor?
        private var active = false
        private var request = 0
        /// Set while the coordinator itself moves the caret, so the
        /// selection callback doesn't snap it again.
        private var adjustingSelection = false

        static var baseAttributes: [NSAttributedString.Key: Any] {
            [.font: Theme.sansUI(ComposerTextInput.fontSize), .foregroundColor: UIColor(Theme.text)]
        }

        init(text: Binding<String>, focus: ComposerFocus) {
            self.text = text
            self.focus = focus
        }

        func attach() {
            active = true
            focus.attach(id)
        }

        func detach() {
            active = false
            request += 1
            focus.detach(id)
        }

        // MARK: Chips

        /// The chips in `storage`, in order.
        private func chips(in storage: NSAttributedString) -> [(range: NSRange, raw: String)] {
            var found: [(NSRange, String)] = []
            storage.enumerateAttribute(.cypherMention, in: NSRange(location: 0, length: storage.length)) { value, range, _ in
                if let chip = value as? MentionChipValue { found.append((range, chip.raw)) }
            }
            return found
        }

        /// The raw draft a stretch of the editor stands for: chips back to
        /// their links, everything else as typed.
        private func raw(of storage: NSAttributedString, upTo end: Int? = nil) -> String {
            let end = end ?? storage.length
            let source = storage.string as NSString
            var out = ""
            var at = 0
            for chip in chips(in: storage) where chip.range.location < end {
                out += source.substring(with: NSRange(location: at, length: chip.range.location - at))
                out += chip.raw
                at = chip.range.location + chip.range.length
            }
            if at < end { out += source.substring(with: NSRange(location: at, length: end - at)) }
            return out
        }

        func raw(of view: UITextView) -> String { raw(of: view.textStorage) }

        /// Show `raw`: plain runs as typed, each mention link as its chip.
        func render(_ raw: String, in view: UITextView) {
            let projection = Mentions.project(raw)
            let base = Self.baseAttributes
            let result = NSMutableAttributedString(string: projection.display, attributes: base)
            var chipAttributes = base
            chipAttributes[.font] = Theme.monoUI(ComposerTextInput.fontSize - 1)
            chipAttributes[.foregroundColor] = UIColor(Theme.inlineCodeText)
            chipAttributes[.cypherInlineCode] = UIColor(Theme.inlineCodeWash)
            for chip in projection.chips {
                let source = (raw as NSString).substring(with: chip.link.range)
                var attributes = chipAttributes
                attributes[.cypherMention] = MentionChipValue(raw: source, display: chip.text)
                result.setAttributes(attributes, range: chip.display)
            }
            view.attributedText = result
            view.typingAttributes = base
        }

        /// Where the caret lands after a re-render: the same raw offset, or
        /// just past a chip it fell inside.
        private func displayOffset(forRaw offset: Int, in raw: String) -> Int {
            var shift = 0
            for chip in Mentions.project(raw).chips {
                let start = chip.link.range.location, end = start + chip.link.range.length
                if offset <= start { break }
                if offset < end { return chip.display.location + chip.display.length }
                shift += chip.display.length - chip.link.range.length
            }
            return offset + shift
        }

        /// Replace `range` (already widened to whole chips) with plain text,
        /// re-render, park the caret after it and publish.
        private func replace(_ view: UITextView, range: NSRange, with replacement: String,
                             caretAfter extra: Int = 0) {
            let storage = view.textStorage
            let prefix = raw(of: storage, upTo: range.location)
            // Tell the keyboard the text moved under it: without this a
            // pending autocorrection survives the splice and swallows the
            // next delete.
            view.inputDelegate?.selectionWillChange(view)
            view.inputDelegate?.textWillChange(view)
            storage.replaceCharacters(in: range, with: NSAttributedString(string: replacement,
                                                                          attributes: Self.baseAttributes))
            let raw = raw(of: storage)
            render(raw, in: view)
            let caretRaw = (prefix + replacement).utf16.count + extra
            adjustingSelection = true
            view.selectedRange = NSRange(location: min(displayOffset(forRaw: caretRaw, in: raw), view.textStorage.length),
                                         length: 0)
            adjustingSelection = false
            view.inputDelegate?.textDidChange(view)
            view.inputDelegate?.selectionDidChange(view)
            publish(view)
        }

        /// Swap the menu's token for a picked link (composer.rs
        /// `insert_raw`): a space follows unless one already does, and the
        /// caret parks past it.
        func splice(_ view: UITextView, token: MentionToken, link: String) {
            guard view.markedTextRange == nil,
                  token.range.location + token.range.length <= view.textStorage.length,
                  Mentions.token(in: masked(view), caret: view.selectedRange.location) == token else { return }
            let source = view.textStorage.string as NSString
            let end = token.range.location + token.range.length
            let next = end < source.length ? source.substring(with: NSRange(location: end, length: 1)) : ""
            let separated = next == " " || next == "\t"
            replace(view, range: token.range, with: separated ? link : link + " ", caretAfter: separated ? 1 : 0)
        }

        /// Remove every chip an edit cut into, whole. The delegate widens
        /// keyboard edits up front, but not every path asks it first
        /// (programmatic `deleteBackward`, some autocorrect replacements),
        /// so the edited text is the authority. Returns whether it changed.
        private func repairChips(_ view: UITextView) -> Bool {
            let storage = view.textStorage
            var order: [ObjectIdentifier] = []
            var groups: [ObjectIdentifier: (chip: MentionChipValue, ranges: [NSRange])] = [:]
            storage.enumerateAttribute(.cypherMention, in: NSRange(location: 0, length: storage.length)) { value, range, _ in
                guard let chip = value as? MentionChipValue else { return }
                let key = ObjectIdentifier(chip)
                if groups[key] == nil { order.append(key) }
                groups[key, default: (chip, [])].ranges.append(range)
            }
            let source = storage.string as NSString
            let damaged = order.compactMap { groups[$0] }.filter { group in
                group.ranges.count != 1 || source.substring(with: group.ranges[0]) != group.chip.display
            }.flatMap(\.ranges).sorted { $0.location > $1.location }
            guard !damaged.isEmpty else { return false }
            let caret = view.selectedRange.location
            let shift = damaged.filter { $0.location < caret }
                .reduce(0) { $0 + min($1.length, caret - $1.location) }
            view.inputDelegate?.selectionWillChange(view)
            view.inputDelegate?.textWillChange(view)
            for range in damaged { storage.replaceCharacters(in: range, with: "") }
            adjustingSelection = true
            view.selectedRange = NSRange(location: min(max(caret - shift, 0), storage.length), length: 0)
            adjustingSelection = false
            view.inputDelegate?.textDidChange(view)
            view.inputDelegate?.selectionDidChange(view)
            return true
        }

        /// The display text with every chip character masked, so `@name`
        /// inside a chip never reads as a token.
        private func masked(_ view: UITextView) -> String {
            let storage = view.textStorage
            let chips = chips(in: storage)
            guard !chips.isEmpty else { return storage.string }
            let masked = NSMutableString(string: storage.string)
            for chip in chips.reversed() {
                masked.replaceCharacters(in: chip.range,
                                         with: String(repeating: "\u{FFFC}", count: chip.range.length))
            }
            return masked as String
        }

        private func publishToken(_ view: UITextView) {
            guard let mentions else { return }
            let selection = view.selectedRange
            guard view.isFirstResponder, selection.length == 0, view.markedTextRange == nil else {
                mentions.publish(nil)
                return
            }
            mentions.publish(Mentions.token(in: masked(view), caret: selection.location))
        }

        private func publish(_ view: UITextView) {
            text.wrappedValue = raw(of: view.textStorage)
            view.typingAttributes = Self.baseAttributes
            view.invalidateIntrinsicContentSize()
            publishToken(view)
        }

        // MARK: Delegate

        func textViewDidBeginEditing(_ textView: UITextView) {
            guard active else { return }
            request += 1
            focus.changed(true, from: id)
            publishToken(textView)
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            guard active else { return }
            request += 1
            focus.changed(false, from: id)
            mentions?.publish(nil)
        }

        /// A chip edits as one unit: an edit touching one widens to the
        /// whole chip, and typing never lands inside one.
        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange,
                      replacementText replacement: String) -> Bool {
            guard textView.markedTextRange == nil else { return true }
            let touched = chips(in: textView.textStorage).filter { chip in
                let end = chip.range.location + chip.range.length
                return range.length > 0
                    ? range.location < end && chip.range.location < range.location + range.length
                    : chip.range.location < range.location && range.location < end
            }
            guard !touched.isEmpty else { return true }
            var widened = range
            for chip in touched { widened = NSUnionRange(widened, chip.range) }
            if range.length == 0 {
                // An insertion inside a chip lands after it.
                widened = NSRange(location: widened.location + widened.length, length: 0)
            }
            replace(textView, range: widened, with: replacement)
            return false
        }

        func textViewDidChangeSelection(_ textView: UITextView) {
            guard active else { return }
            textView.typingAttributes = Self.baseAttributes
            if !adjustingSelection, textView.markedTextRange == nil {
                // Never park the caret inside a chip.
                let selection = textView.selectedRange
                for chip in chips(in: textView.textStorage) {
                    let end = chip.range.location + chip.range.length
                    if selection.length == 0, chip.range.location < selection.location, selection.location < end {
                        adjustingSelection = true
                        let nearer = selection.location - chip.range.location < end - selection.location
                            ? chip.range.location : end
                        textView.selectedRange = NSRange(location: nearer, length: 0)
                        adjustingSelection = false
                        break
                    }
                }
            }
            publishToken(textView)
        }

        func textViewDidChange(_ textView: UITextView) {
            guard active, focus.owns(id) else { return }
            // Also reconcile on input, including IME updates. A first
            // keystroke must not wait for a character-count threshold.
            focus.changed(textView.isFirstResponder, from: id)
            if textView.markedTextRange == nil { _ = repairChips(textView) }
            publish(textView)
        }

        func reconcileFocus(_ view: UITextView, enabled: Bool) {
            request += 1
            let ticket = request
            let wanted = focus.isFocused && enabled
            guard wanted != view.isFirstResponder else { return }
            // Avoid publishing focus changes from inside updateUIView, and
            // never let an old scheduled request steal focus back after blur.
            DispatchQueue.main.async { [weak self, weak view] in
                guard let self, let view, self.active, self.focus.owns(self.id),
                      self.request == ticket, view.window != nil,
                      (self.focus.isFocused && view.isEditable) == wanted else { return }
                if wanted { view.becomeFirstResponder() }
                else { view.resignFirstResponder() }
            }
        }
    }
}

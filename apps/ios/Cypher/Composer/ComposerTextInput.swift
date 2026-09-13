import SwiftUI

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

/// Own the UITextView and its delegate outright. SwiftUI's multiline
/// TextField can put its accessibility ID on a wrapper rather than the view
/// that sends editing notifications, and FocusState has not been reliable
/// inside the session's safeAreaBar on physical devices.
struct ComposerTextInput: UIViewRepresentable {
    @Binding var text: String
    let focus: ComposerFocus
    let editorID: String
    let enabled: Bool
    var placeholder = "Message"

    func makeCoordinator() -> Coordinator { Coordinator(text: $text, focus: focus) }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.backgroundColor = .clear
        view.font = Theme.sansUI(16)
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.isScrollEnabled = true
        view.alwaysBounceVertical = false
        view.bounces = false
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        context.coordinator.attach()
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.text = $text
        view.accessibilityIdentifier = editorID
        view.accessibilityLabel = placeholder
        view.textColor = UIColor(Theme.text)
        view.tintColor = UIColor(Theme.text)
        view.isEditable = enabled
        // Never replace native marked text during Chinese/Japanese input.
        // Accepted sends retire the entire editor via editorRevision instead.
        if view.text != text, view.markedTextRange == nil {
            let selection = view.selectedRange
            view.text = text
            let length = (text as NSString).length
            let start = min(selection.location, length)
            view.selectedRange = NSRange(location: start, length: min(selection.length, length - start))
            view.invalidateIntrinsicContentSize()
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
        view.delegate = nil
    }

    @MainActor
    final class Coordinator: NSObject, UITextViewDelegate {
        var text: Binding<String>
        let focus: ComposerFocus
        let id = UUID()
        private var active = false
        private var request = 0

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

        func textViewDidBeginEditing(_ textView: UITextView) {
            guard active else { return }
            request += 1
            focus.changed(true, from: id)
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            guard active else { return }
            request += 1
            focus.changed(false, from: id)
        }

        func textViewDidChange(_ textView: UITextView) {
            guard active, focus.owns(id) else { return }
            // Also reconcile on input, including IME updates. A first
            // keystroke must not wait for a character-count threshold.
            focus.changed(textView.isFirstResponder, from: id)
            text.wrappedValue = textView.text
            textView.invalidateIntrinsicContentSize()
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

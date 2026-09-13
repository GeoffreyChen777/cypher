import XCTest
import SwiftUI
@testable import Cypher

@MainActor
final class ComposerTextInputTests: XCTestCase {
    func testOnlyTheCurrentEditorCanChangeFocus() {
        let focus = ComposerFocus()
        let old = UUID(), current = UUID(), search = UUID()
        focus.attach(old)
        focus.changed(true, from: old)
        XCTAssertTrue(focus.isFocused)
        focus.attach(current)
        focus.changed(false, from: old)
        focus.detach(old)
        XCTAssertTrue(focus.isFocused, "Retiring an editor must not blur its replacement")
        focus.changed(false, from: current)
        focus.changed(true, from: search)
        XCTAssertFalse(focus.isFocused, "A picker/search keyboard is not composer focus")
        focus.changed(true, from: current)
        XCTAssertTrue(focus.isFocused)
    }

    func testDelegateFocusDoesNotDependOnAccessibilityOrTextLength() {
        let draft = ComposerDraft(), focus = ComposerFocus()
        let coordinator = ComposerTextInput.Coordinator(text: draft.binding, focus: focus)
        coordinator.attach()
        let view = UITextView()
        XCTAssertNil(view.accessibilityIdentifier)
        coordinator.textViewDidBeginEditing(view)
        XCTAssertTrue(focus.isFocused)
        XCTAssertTrue(draft.text.isEmpty)
        coordinator.textViewDidEndEditing(view)
        XCTAssertFalse(focus.isFocused)
    }

    func testRetiredDelegateCannotRestoreSentTextOrBlurNewInput() {
        let draft = ComposerDraft(), focus = ComposerFocus()
        let old = ComposerTextInput.Coordinator(text: draft.binding, focus: focus)
        old.attach()
        let view = UITextView()
        view.text = "sent"
        old.textViewDidChange(view)
        XCTAssertEqual(draft.text, "sent")
        draft.clearAfterSend()
        old.detach()
        let current = ComposerTextInput.Coordinator(text: draft.binding, focus: focus)
        current.attach()
        current.textViewDidBeginEditing(view)
        draft.binding.wrappedValue = "next"
        old.textViewDidChange(view)
        old.textViewDidEndEditing(view)
        XCTAssertEqual(draft.text, "next")
        XCTAssertTrue(focus.isFocused)
    }

    func testNativeMarkedTextIsNotReplacedByAViewUpdate() async throws {
        let draft = ComposerDraft()
        let focus = ComposerFocus()
        let root = ComposerTextInput(text: draft.binding, focus: focus, editorID: "ime", enabled: true)
            .frame(width: 300, height: 100)
        let host = UIHostingController(rootView: root)
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let previous = scene.windows.first(where: \.isKeyWindow)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            window.rootViewController = nil
            previous?.makeKey()
        }
        func editor(_ parent: UIView) -> UITextView? {
            (parent as? UITextView) ?? parent.subviews.lazy.compactMap { editor($0) }.first
        }
        host.view.layoutIfNeeded()
        let view = try XCTUnwrap(editor(host.view))
        XCTAssertTrue(view.becomeFirstResponder())
        view.setMarkedText("拼", selectedRange: NSRange(location: 1, length: 0))
        XCTAssertNotNil(view.markedTextRange)
        // A parent render with stale text must not reset the active IME/caret.
        host.rootView = ComposerTextInput(text: .constant("stale"), focus: focus, editorID: "ime", enabled: true)
            .frame(width: 300, height: 100)
        try await Task.sleep(for: .milliseconds(100))
        host.view.layoutIfNeeded()
        XCTAssertTrue(editor(host.view) === view)
        XCTAssertEqual(view.text, "拼")
        XCTAssertNotNil(view.markedTextRange)
        view.unmarkText()
    }
}

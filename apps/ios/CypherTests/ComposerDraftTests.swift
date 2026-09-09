import XCTest
import SwiftUI
@testable import Cypher

@MainActor
final class ComposerDraftTests: XCTestCase {
    func testSuccessfulSendClearsAndRejectsLateEditorWrites() {
        let draft = ComposerDraft()
        let oldEditor = draft.binding
        oldEditor.wrappedValue = "测试 steer"
        draft.clearAfterSend()
        XCTAssertEqual(draft.text, "")
        XCTAssertEqual(draft.revision, 1)
        oldEditor.wrappedValue = "测试 steer"
        XCTAssertEqual(draft.text, "", "Late text/IME commits must not restore a sent prompt")
        let nextEditor = draft.binding
        nextEditor.wrappedValue = "下一条消息"
        oldEditor.wrappedValue = ""
        XCTAssertEqual(draft.text, "下一条消息", "No deferred clear may erase the next draft")
        draft.clearAfterSend()
        nextEditor.wrappedValue = "下一条消息"
        XCTAssertEqual(draft.text, "")
    }

    func testNoAcceptedSendPreservesDraftAndEditor() {
        let draft = ComposerDraft()
        let editor = draft.binding
        editor.wrappedValue = "keep on queue failure"
        XCTAssertEqual(draft.revision, 0)
        XCTAssertEqual(draft.text, "keep on queue failure")
        editor.wrappedValue += " — retry"
        XCTAssertEqual(draft.text, "keep on queue failure — retry")
    }

    func testEmptyComposerExpandsOnNativeFocusAndCollapsesOnBlur() async throws {
        let draft = ComposerDraft()
        var height: CGFloat = 0
        let host = UIHostingController(rootView: FocusComposerFixture(draft: draft) { height = $0 })
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
        func editor(_ view: UIView) -> UIView? {
            if view is UITextField || view is UITextView { return view }
            return view.subviews.lazy.compactMap { editor($0) }.first
        }
        func settle(_ message: String, _ condition: () -> Bool) async throws {
            for _ in 0..<75 {
                host.view.layoutIfNeeded()
                if condition() { return }
                try await Task.sleep(for: .milliseconds(20))
            }
            XCTFail("\(message); height=\(height)")
        }
        try await settle("initial collapsed editor") { editor(host.view) != nil && height > 0 && height < 100 }
        let field = try XCTUnwrap(editor(host.view))
        // Accessibility is for automation, never a prerequisite for observing
        // focus. SwiftUI's private backing views need not inherit this ID.
        field.accessibilityIdentifier = nil
        XCTAssertTrue(field.becomeFirstResponder())
        try await settle("empty focused composer must show toolbar") { height > 100 }
        XCTAssertTrue(field.isFirstResponder, "Expansion must preserve editor identity and focus")
        host.rootView = FocusComposerFixture(draft: draft) { height = $0 }
        try await Task.sleep(for: .milliseconds(100))
        host.view.layoutIfNeeded()
        XCTAssertTrue(editor(host.view) === field, "A parent/session refresh must preserve the editor")
        XCTAssertTrue(field.isFirstResponder)
        XCTAssertGreaterThan(height, 100)
        draft.binding.wrappedValue = "短消息"
        try await settle("short focused draft stays expanded") { height > 100 }
        let native = try XCTUnwrap(field as? UITextView)
        draft.binding.wrappedValue = Array(repeating: "line", count: 20).joined(separator: "\n")
        let cap = ceil(try XCTUnwrap(native.font).lineHeight * 7)
        try await settle("long drafts cap at seven lines and scroll") {
            abs(native.bounds.height - cap) < 1 && native.contentSize.height > native.bounds.height
        }
        draft.binding.wrappedValue = "短消息"
        field.resignFirstResponder()
        try await settle("short blurred draft collapses") { height < 100 }
    }

    func testLiveComposerClearsNativeTextWithoutRunStateTransition() async throws {
        let draft = ComposerDraft()
        draft.text = "测试 steer message"
        let host = UIHostingController(rootView: LiveComposerFixture(draft: draft))
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
        func editors(_ view: UIView) -> [UIView] {
            if view is UITextView || view is UITextField { return [view] }
            return view.subviews.flatMap { editors($0) }
        }
        func text(_ view: UIView) -> String {
            (view as? UITextView)?.text ?? (view as? UITextField)?.text ?? ""
        }
        func settle(_ condition: () -> Bool) async throws {
            for _ in 0..<50 {
                host.view.layoutIfNeeded()
                if condition() { return }
                try await Task.sleep(for: .milliseconds(20))
            }
            XCTFail("Composer update did not settle")
        }
        try await settle { editors(host.view).contains { text($0) == draft.text } }
        let old = try XCTUnwrap(editors(host.view).first { text($0) == draft.text })
        old.becomeFirstResponder()
        try await settle { old.isFirstResponder }
        // Let SwiftUI receive the native focus update before retiring it.
        try await Task.sleep(for: .milliseconds(50))
        draft.clearAfterSend()
        try await settle {
            let current = editors(host.view)
            return !current.isEmpty && current.allSatisfy { text($0).isEmpty && $0 !== old }
        }
        try await settle { editors(host.view).contains { $0.isFirstResponder } }
        draft.binding.wrappedValue = "next prompt"
        try await settle { editors(host.view).contains { text($0) == "next prompt" } }
    }
}

private struct FocusComposerFixture: View {
    let draft: ComposerDraft
    let heightChanged: (CGFloat) -> Void
    var body: some View {
        ScrollView { Text("Synced transcript").frame(maxWidth: .infinity, minHeight: 500) }
        .safeAreaBar(edge: .bottom, spacing: 0) {
            ComposerShell(
                draft: draft.binding,
                editorRevision: draft.revision,
                sendEnabled: true,
                showStop: false,
                onSend: { draft.clearAfterSend() }
            ) { Text("Pi · High") }
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { heightChanged($0) }
        }
        .frame(width: 360)
        .frame(maxHeight: .infinity, alignment: .bottom)
    }
}

private struct LiveComposerFixture: View {
    let draft: ComposerDraft
    var body: some View {
        ComposerShell(
            draft: draft.binding,
            editorRevision: draft.revision,
            sendEnabled: true,
            showStop: true,
            onSend: { draft.clearAfterSend() }
        ) { Text("Pi") }
    }
}

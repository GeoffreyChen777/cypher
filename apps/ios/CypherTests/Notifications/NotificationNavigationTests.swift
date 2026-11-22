import XCTest
import SwiftUI
@testable import Cypher

/// Regressions from two phone reports: a crash after a notification opened a
/// session (the tap was swallowed while Home was covered, then replayed on
/// the way back in the middle of the pop), and long composer drafts drawing
/// over the model chips / send button. Both drive the REAL views (HomeView /
/// SessionView) against the in-memory demo, in a full-size window — Home
/// pinned to compact width, the phone's stack, on any simulator.
@MainActor
final class NotificationNavigationTests: XCTestCase {
    private func settle(
        _ what: String, file: StaticString = #filePath, line: UInt = #line,
        _ condition: @MainActor () -> Bool
    ) async throws {
        try await waitUntil(what, file: file, line: line, condition)
    }

    private func views<T: UIView>(of type: T.Type, in root: UIView) -> [T] {
        var out: [T] = []
        if let v = root as? T { out.append(v) }
        for child in root.subviews { out += views(of: type, in: child) }
        return out
    }

    // MARK: 1. Long draft overlapping the toolbar

    func testLongDraftStaysAboveTheComposerToolbar() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let driver = ReproDriver()
        driver.path = [.space("space-cypher"), .chat("chat-veil")]
        let hosted = try HostedWindow(ReproStack(driver: driver, model: model))
        defer { hosted.close() }
        let window = hosted.window
        let hostVC = hosted.host
        try await settle("chat pushed") {
            (navigationController(in: hostVC)?.viewControllers.count ?? 0) == 3
                && navigationController(in: hostVC)?.transitionCoordinator == nil
        }
        let store = try XCTUnwrap(model.sessionStore(for: XCTUnwrap(model.chat(id: "chat-veil"))))
        try await settle("transcript hydrated") { !store.entries.isEmpty }
        try await Task.sleep(for: .milliseconds(1200))
        func findEditor() -> UITextView? {
            views(of: UITextView.self, in: window).first {
                $0.accessibilityIdentifier?.hasPrefix("composer-editor-") == true
            }
        }
        try await settle("composer editor present") { findEditor() != nil }
        let editor = try XCTUnwrap(findEditor())

        // Focus like a real tap (keyboard up, expanded layout), then type a
        // long draft the way the keyboard does: native text + delegate.
        XCTAssertTrue(editor.becomeFirstResponder())
        try await Task.sleep(for: .milliseconds(800))
        for text in [
            ((1...18).map { "Line \($0) of a long message that keeps going for a while." }.joined(separator: "\n")),
            (String(repeating: "这是一段很长的中文输入，用来测试输入框在文字很多的时候是否会和下面的模型选择以及发送按钮重叠。", count: 6)),
            ((1...40).map { "第\($0)行 line \($0)" }.joined(separator: "\n")),
        ] {
            editor.text = text
            editor.delegate?.textViewDidChange?(editor)
            try await Task.sleep(for: .milliseconds(600))
            window.layoutIfNeeded()
            try await Task.sleep(for: .milliseconds(400))
        }

        let editorFrame = editor.convert(editor.bounds, to: window)
        let lineHeight = editor.font?.lineHeight ?? 20
        // The chips rail is the short horizontal UIScrollView under the editor.
        let chips = views(of: UIScrollView.self, in: window).filter { view in
            view !== editor && !(view is UITextView)
                && view.bounds.height < 60 && view.convert(view.bounds, to: window).minY >= editorFrame.minY
        }
        let chipsFrame = chips.first.map { $0.convert($0.bounds, to: window) }
        XCTAssertLessThanOrEqual(
            editorFrame.height, ceil(lineHeight * 7) + 1,
            "the editor must cap at seven lines and scroll inside")
        if let chipsFrame {
            XCTAssertLessThanOrEqual(
                editorFrame.maxY, chipsFrame.minY + 0.5,
                "the draft must not draw over the model chips")
        } else {
            XCTFail("chips rail not found")
        }
    }

    // MARK: 2. Notification taps

    func testNotificationTapsOpenSessionsWithoutCrashing() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let scope = String(repeating: "a", count: 64)
        let controller = model.notifications
        controller.readRegistration = { nil }
        controller.writeRegistration = { _ in true }
        controller.authorization = { .authorized }
        controller.registerWithOS = {}
        controller.clearDelivered = {}
        controller.setBadge = { _ in }
        controller.perform = { request in
            let url = request.url!
            var body: [String: Any] = ["ok": true, "scope": scope]
            if url.path.hasSuffix("/settings") {
                let prefs = try JSONSerialization.jsonObject(with: JSONEncoder().encode(NotificationPreferences()))
                body = ["scope": scope, "available": true, "settings": prefs]
            } else if url.path.hasSuffix("/register") {
                body = ["scope": scope, "bindingId": String(repeating: "c", count: 64), "lease": "l"]
            }
            return (
                try JSONSerialization.data(withJSONObject: body),
                HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            )
        }
        controller.bind(
            AppConfig(
                edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
                userId: "u", orgId: "org", deviceId: "phone", deviceName: "Test",
                devBearer: "u@org"))
        defer { controller.disconnect() }
        try await settle("scope bound") { controller.scope == scope }

        let hosted = try HostedWindow(HomeView().environment(model).environment(\.horizontalSizeClass, .compact))
        defer { hosted.close() }
        let hostVC = hosted.host
        try await settle("home mounted") { navigationController(in: hostVC) != nil }
        let nav = try XCTUnwrap(navigationController(in: hostVC))
        try await Task.sleep(for: .milliseconds(300))

        func tap(_ chat: String, project: String = "space-cypher", kind: String = "completed") {
            let payload = PushPayload(
                eventId: UUID().uuidString, scope: scope, chatId: chat,
                projectId: project, kind: kind)
            // The delegate's exact sequence: deliver the tap, then refresh.
            controller.receive(payload, tapped: true)
            Task { await controller.refresh() }
        }
        func topChat() -> String? {
            nav.topViewController?.title ?? nav.navigationBar.topItem?.title
        }

        func expect(_ what: String, depth: Int, line: UInt = #line) async throws {
            // The tap is consumed before the path moves, so a stack already
            // at `depth` (a pop followed by a push) is not mistaken for done.
            try await waitUntil(what, line: line) {
                controller.pendingNavigation == nil && nav.viewControllers.count == depth
                    && nav.transitionCoordinator == nil
            }
        }
        // Cold-start shape: nothing pushed yet.
        tap("chat-tabs")
        try await expect("first open", depth: 2)
        // Another chat while one is on screen (push). This is the reported
        // case: Home is covered, and the tap must still open the session
        // right away rather than replay later on the way back.
        tap("chat-veil")
        try await expect("second open", depth: 3)
        XCTAssertNil(controller.pendingNavigation, "consumed on navigation")
        // A child of the visible chat.
        tap("demo-child-planner")
        try await expect("child open", depth: 4)
        // Back to an ancestor (pop-to-existing).
        tap("chat-tabs")
        try await expect("pop to first", depth: 2)
        // Two taps in quick succession, the second landing mid-transition:
        // the second waits for the first push to finish, then lands. The
        // second is sent once the first push is on the stack, not after a
        // fixed pause: a tap is held back while an earlier transition
        // settles, and a second one arriving meanwhile replaces it.
        tap("chat-oklch")
        try await waitUntil("the first of two rapid taps to push") { nav.viewControllers.count == 3 }
        tap("chat-picker")
        try await expect("rapid taps", depth: 4)
        XCTAssertEqual(nav.topViewController?.navigationItem.title, "Model picker catalog sync")
        // A user pop with a tap arriving during the animation.
        nav.popViewController(animated: true)
        tap("chat-deploy", project: "space-edge")
        try await expect("tap during pop", depth: 4)
        XCTAssertEqual(nav.topViewController?.navigationItem.title, "Wrangler deploy hygiene")
        // The same chat that is already visible: nothing is pushed again.
        let before = nav.viewControllers.count
        tap("chat-deploy", project: "space-edge")
        try await expect("a tap on the visible chat", depth: before)
        // An input request notification.
        tap("chat-veil", kind: "input")
        try await settle("input open") {
            controller.pendingNavigation == nil && nav.transitionCoordinator == nil
        }
        XCTAssertNil(controller.navigationError)
    }
}

extension NotificationNavigationTests {
    /// Cold launch from a notification: the tap is pending BEFORE the home
    /// screen exists, so the navigation lands on the stack's first frame.
    func testNotificationPendingBeforeHomeMountsOpensTheSession() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let scope = String(repeating: "a", count: 64)
        let controller = model.notifications
        controller.readRegistration = { nil }
        controller.writeRegistration = { _ in true }
        controller.authorization = { .authorized }
        controller.registerWithOS = {}
        controller.clearDelivered = {}
        controller.setBadge = { _ in }
        controller.perform = { request in
            let url = request.url!
            var body: [String: Any] = ["ok": true, "scope": scope]
            if url.path.hasSuffix("/settings") {
                let prefs = try JSONSerialization.jsonObject(with: JSONEncoder().encode(NotificationPreferences()))
                body = ["scope": scope, "available": true, "settings": prefs]
            } else if url.path.hasSuffix("/register") {
                body = ["scope": scope, "bindingId": String(repeating: "c", count: 64), "lease": "l"]
            }
            return (
                try JSONSerialization.data(withJSONObject: body),
                HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            )
        }
        // The delegate sees the tap before any controller/config exists.
        let payload = PushPayload(
            eventId: UUID().uuidString, scope: scope, chatId: "chat-veil",
            projectId: "space-cypher", kind: "completed")
        controller.receive(payload, tapped: true)
        controller.bind(
            AppConfig(
                edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
                userId: "u", orgId: "org", deviceId: "phone", deviceName: "Test",
                devBearer: "u@org"))
        defer { controller.disconnect() }
        try await settle("the deferred tap to complete on bind") { controller.pendingNavigation != nil }
        XCTAssertEqual(controller.pendingNavigation?.chatId, "chat-veil", "deferred tap completes on bind/refresh")
        let hosted = try HostedWindow(HomeView().environment(model).environment(\.horizontalSizeClass, .compact))
        defer { hosted.close() }
        let hostVC = hosted.host
        try await settle("home mounted") { navigationController(in: hostVC) != nil }
        let nav = try XCTUnwrap(navigationController(in: hostVC))
        try await settle("the pending tap to open its session") {
            controller.pendingNavigation == nil && nav.viewControllers.count == 2
                && nav.transitionCoordinator == nil
        }
    }
}

extension NotificationNavigationTests {
    /// A workspace notice opens the workspace (its chat, `Route.bot`), its
    /// Needs you item's board page over it, and the chat counts as viewed
    /// there, as a session's does; a gone item leaves it on the chat.
    func testWorkspaceNoticeTapsOpenTheWorkspaceAndItsItem() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let scope = String(repeating: "a", count: 64)
        let controller = model.notifications
        controller.readRegistration = { nil }
        controller.writeRegistration = { _ in true }
        controller.authorization = { .authorized }
        controller.registerWithOS = {}
        controller.clearDelivered = {}
        controller.setBadge = { _ in }
        controller.perform = { request in
            let url = request.url!
            var body: [String: Any] = ["ok": true, "scope": scope]
            if url.path.hasSuffix("/settings") {
                let prefs = try JSONSerialization.jsonObject(with: JSONEncoder().encode(NotificationPreferences()))
                body = ["scope": scope, "available": true, "settings": prefs]
            } else if url.path.hasSuffix("/register") {
                body = ["scope": scope, "bindingId": String(repeating: "c", count: 64), "lease": "l"]
            }
            return (
                try JSONSerialization.data(withJSONObject: body),
                HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!
            )
        }
        controller.bind(
            AppConfig(
                edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
                userId: "u", orgId: "org", deviceId: "phone", deviceName: "Test",
                devBearer: "u@org"))
        defer { controller.disconnect() }
        try await settle("scope bound") { controller.scope == scope }

        let hosted = try HostedWindow(HomeView().environment(model).environment(\.horizontalSizeClass, .compact))
        defer { hosted.close() }
        let hostVC = hosted.host
        try await settle("home mounted") { navigationController(in: hostVC) != nil }
        let nav = try XCTUnwrap(navigationController(in: hostVC))
        try await Task.sleep(for: .milliseconds(300))

        func tap(_ item: PushItem?) {
            controller.receive(
                PushPayload(
                    eventId: UUID().uuidString, scope: scope, chatId: "chat-atlas", projectId: DemoDataset.atlasId,
                    kind: "input", workspaceId: DemoDataset.atlasId, item: item),
                tapped: true)
        }
        func presented() -> UIViewController? {
            var top = hostVC.presentedViewController
            while let next = top?.presentedViewController { top = next }
            return top
        }
        func settled(_ depth: Int) -> Bool {
            controller.pendingNavigation == nil && nav.viewControllers.count == depth
                && nav.transitionCoordinator == nil
        }

        // The workspace alone: its chat, reported as the one on screen.
        tap(nil)
        try await settle("the workspace opens") { settled(2) }
        XCTAssertEqual(nav.topViewController?.navigationItem.title, "Atlas")
        XCTAssertEqual(controller.currentChat, "chat-atlas", "the workspace's chat is reported as viewed")
        XCTAssertNil(controller.pendingBoard)
        XCTAssertNil(controller.navigationError)

        // A plan: the same screen (not pushed again), its board at the plan.
        tap(.plan("work-rounding"))
        try await settle("the plan's board page opens") {
            settled(2) && controller.pendingBoard == nil && presented() != nil
        }
        XCTAssertEqual(controller.currentChat, "chat-atlas")
        presented()?.dismiss(animated: false)
        try await settle("the board closes") { presented() == nil }

        // A gone item: the workspace's chat, no board, no error.
        tap(.approval("approval-gone"))
        try await settle("a gone item stays on the chat") { settled(2) }
        try await Task.sleep(for: .milliseconds(900))
        XCTAssertNil(presented(), "no board for an item that is gone")
        XCTAssertNil(controller.navigationError)

        // A session notice still opens its session, and leaving the
        // workspace reports that session instead.
        controller.receive(
            PushPayload(
                eventId: UUID().uuidString, scope: scope, chatId: "chat-veil", projectId: "space-cypher",
                kind: "completed"),
            tapped: true)
        try await settle("the session opens over the workspace") { settled(3) }
        XCTAssertEqual(controller.currentChat, "chat-veil")
        nav.popViewController(animated: false)
        try await settle("back on the workspace") { nav.viewControllers.count == 2 && nav.transitionCoordinator == nil }
        try await settle("the workspace's chat is viewed again") { controller.currentChat == "chat-atlas" }
        XCTAssertNil(controller.navigationError)
    }
}

@MainActor
private final class ReproDriver: ObservableObject {
    @Published var path: [Route] = []
}

private struct ReproStack: View {
    @ObservedObject var driver: ReproDriver
    let model: AppModel
    var body: some View {
        NavigationStack(path: $driver.path) {
            Text("Projects")
                .navigationDestination(for: Route.self) { route in
                    switch route {
                    case .space(let id): SpaceView(spaceId: id, path: $driver.path)
                    case .chat(let id): SessionView(chatId: id, path: $driver.path).id(id)
                    case .newSession(let id): NewSessionView(spaceId: id, path: $driver.path)
                    case .quickChat(let id):
                        NewSessionView(spaceId: "", path: $driver.path, quickDeviceId: id)
                    case .bot(let id): BotView(botId: id, path: $driver.path)
                    }
                }
        }
        .environment(model)
    }
}

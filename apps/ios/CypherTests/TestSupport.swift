import SwiftUI
import XCTest
@testable import Cypher

enum TestSupport {
    /// The repository checkout: the nearest ancestor of this file holding
    /// Cargo.toml. Simulator tests read repo fixtures straight from the host,
    /// so no test depends on its own folder depth.
    static func repoRoot(file: String = #filePath) throws -> URL {
        var dir = URL(fileURLWithPath: file).deletingLastPathComponent()
        while dir.path != "/" {
            if FileManager.default.fileExists(atPath: dir.appendingPathComponent("Cargo.toml").path) { return dir }
            dir = dir.deletingLastPathComponent()
        }
        throw CocoaError(.fileNoSuchFile, userInfo: [NSFilePathErrorKey: file])
    }

    /// apps/ios, for project-level files such as export options.
    static func iosRoot() throws -> URL {
        try repoRoot().appendingPathComponent("apps/ios")
    }
}

/// `TranscriptRowBuilder.rows` with fresh parse caches, as a first render
/// builds them.
func buildRows(_ entries: [MessageEntry], pending: [PendingSend] = []) -> [TranscriptRow] {
    var parsers: [String: IncrementalMarkdownParser] = [:]
    var completed: [String: CompletedParse] = [:]
    return TranscriptRowBuilder.rows(entries: entries, pendingSends: pending,
                                     parsers: &parsers, completed: &completed)
}

extension MessageEntry {
    /// A transcript entry for tests: complete, from device "d", created at 1
    /// unless stated.
    static func fixture(_ id: String = "m", role: MessageRole = .assistant, parts: [MessagePart],
                        createdAt: Int64 = 1, deviceId: String = "d",
                        status: MessageStatus? = .complete, isSteer: Bool = false) -> MessageEntry {
        MessageEntry(id: id, role: role, parts: parts, createdAt: createdAt, deviceId: deviceId,
                     status: status, isSteer: isSteer)
    }
}

/// A key window in the app's scene hosting `rootView`; `close()` hides it
/// and gives key status back to the previous key window.
@MainActor
final class HostedWindow<Root: View> {
    let window: UIWindow
    let host: UIHostingController<Root>
    private let previous: UIWindow?

    init(_ rootView: Root, frame: CGRect? = nil) throws {
        host = UIHostingController(rootView: rootView)
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        previous = scene.windows.first(where: \.isKeyWindow)
        window = UIWindow(windowScene: scene)
        if let frame { window.frame = frame }
        window.rootViewController = host
        window.makeKeyAndVisible()
    }

    func close() {
        window.isHidden = true
        window.rootViewController = nil
        previous?.makeKey()
    }
}

/// The first navigation controller at or under `controller`.
@MainActor
func navigationController(in controller: UIViewController) -> UINavigationController? {
    if let nav = controller as? UINavigationController { return nav }
    for child in controller.children {
        if let nav = navigationController(in: child) { return nav }
    }
    return nil
}

/// The first view of `type` at or under `view`, depth-first.
@MainActor
func firstSubview<T: UIView>(_ type: T.Type, in view: UIView) -> T? {
    if let match = view as? T { return match }
    return view.subviews.lazy.compactMap { firstSubview(type, in: $0) }.first
}

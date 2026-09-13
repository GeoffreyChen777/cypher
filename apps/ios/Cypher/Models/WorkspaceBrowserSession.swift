import Foundation

enum WorkspaceBrowserError: LocalizedError {
    case unavailable, contextChanged, invalidPath, missingDirectory
    var errorDescription: String? {
        switch self {
        case .unavailable: "Connect to this session's device to browse its files."
        case .contextChanged: "The account, device or checkout changed. Reopen Files / Changes."
        case .invalidPath: "This path is not available in the workspace browser."
        case .missingDirectory: "This session has no working directory. Open a session in a project or worktree to browse files."
        }
    }
}

/// One sheet owns one device/checkout/account context. Nothing falls back to
/// the phone's filesystem, a different device, or another account's cache.
@MainActor
final class WorkspaceBrowserSession {
    let chat: Chat
    private let model: AppModel
    private let workspace: WorkspaceStore?
    private let demo: DemoDataset?
    private var isDemo: Bool { demo != nil }

    init(model: AppModel, chat: Chat) {
        self.model = model
        self.chat = chat
        workspace = model.workspace
        demo = model.demo
    }

    private func check() throws {
        guard !Task.isCancelled else { throw CancellationError() }
        guard model.workspace === workspace, model.demo === demo,
              let current = model.chat(id: chat.id), current.deviceId == chat.deviceId,
              current.cwd == chat.cwd else { throw WorkspaceBrowserError.contextChanged }
        guard let cwd = chat.cwd, !cwd.isEmpty else { throw WorkspaceBrowserError.missingDirectory }
    }

    private func call<T: Decodable>(_ method: String, path: String? = nil) async throws -> T {
        try check()
        guard let workspace else { throw WorkspaceBrowserError.unavailable }
        var params: [String: Any] = ["chatId": chat.id, "cwd": chat.cwd ?? ""]
        if let path {
            guard WorkspaceFilePath.valid(path) else { throw WorkspaceBrowserError.invalidPath }
            params["path"] = path
        }
        let result: T = try await workspace.workspaceBrowserCall(deviceId: chat.deviceId,
                                                                 method: method, params: params)
        try check()
        return result
    }

    func directory(_ path: String) async throws -> WorkspaceDirectory {
        try check()
        if isDemo {
            guard ["", "Sources"].contains(path) else {
                throw WorkspaceBrowserError.invalidPath
            }
            guard WorkspaceFilePath.valid(path) else { throw WorkspaceBrowserError.invalidPath }
            let entries: [WorkspaceFileEntry] = path.isEmpty
                ? [.init(name: "Sources", isDir: true), .init(name: "README.md", isDir: false)]
                : path == "Sources" ? [.init(name: "Example.swift", isDir: false),
                                       .init(name: "ReaderExample.swift", isDir: false)] : []
            return WorkspaceDirectory(entries: entries, truncated: false)
        }
        return try await call("ListWorkspaceFiles", path: path)
    }

    func file(_ path: String) async throws -> WorkspaceFileContent {
        try check()
        guard WorkspaceFilePath.valid(path, allowRoot: false) else { throw WorkspaceBrowserError.invalidPath }
        if isDemo {
            guard ["README.md", "Sources/Example.swift", "Sources/ReaderExample.swift"].contains(path) else {
                throw WorkspaceBrowserError.invalidPath
            }
            let text = path == "README.md" ? "# Demo workspace\n\nRead-only file browsing.\n"
                : path == "Sources/ReaderExample.swift" ? Self.readerExample : Self.demoSource(new: true)
            return WorkspaceFileContent(text: text, bytes: UInt64(text.utf8.count), binary: false, truncated: false)
        }
        return try await call("ReadWorkspaceFile", path: path)
    }

    func changes() async throws -> WorkspaceChanges {
        try check()
        if isDemo {
            return WorkspaceChanges(checkoutId: "demo", deviceId: chat.deviceId, cwd: chat.cwd ?? "",
                patch: Self.demoPatch,
                files: [.init(path: "Sources/Example.swift", status: "modified", additions: 2, deletions: 2, binary: false),
                        .init(path: "README.md", status: "modified", additions: 1, deletions: 1, binary: false)],
                additions: 3, deletions: 3, truncated: false, checksum: "demo", updatedAt: "Demo snapshot")
        }
        let result: WorkspaceChanges = try await call("GetCheckoutDiff")
        guard result.deviceId == chat.deviceId else { throw WorkspaceBrowserError.contextChanged }
        return result
    }

    func diffSources(snapshot: WorkspaceChanges, path: String) async throws -> WorkspaceDiffSources {
        try check()
        guard snapshot.deviceId == chat.deviceId, !snapshot.truncated,
              snapshot.files.contains(where: { $0.path == path && !$0.binary }),
              WorkspaceFilePath.valid(path, allowRoot: false) else {
            throw WorkspaceDiffContextError.unavailable
        }
        if isDemo {
            let old = path == "README.md" ? "# Demo workspace\n\nFile browsing.\n" : Self.demoSource(new: false)
            let new = path == "README.md" ? "# Demo workspace\n\nRead-only file browsing.\n" : Self.demoSource(new: true)
            return try WorkspaceDiffSources(diffChecksum: "demo", oldText: old, newText: new,
                binary: false, truncated: false, stale: false).validated(checksum: snapshot.checksum)
        }
        guard let workspace else { throw WorkspaceBrowserError.unavailable }
        let result: WorkspaceDiffSources = try await workspace.workspaceBrowserCall(
            deviceId: chat.deviceId, method: "GetCheckoutFileDiffText", params: [
                "checkoutId": snapshot.checkoutId, "cwd": snapshot.cwd, "chatId": chat.id,
                "mode": "working", "path": path, "diffChecksum": snapshot.checksum,
            ])
        try check()
        return try result.validated(checksum: snapshot.checksum)
    }

    static func demoSource(new: Bool) -> String {
        var lines = (1...60).map { "let value\($0) = \($0)" }
        lines[0] = "import Foundation"
        lines[19] = new ? "let message = \"Hello from Cypher\"" : "let message = \"Hello\""
        lines[44] = new ? "let enabled = true" : "let enabled = false"
        return lines.joined(separator: "\n") + "\n"
    }

    static let readerExample = #"""
    import Foundation

    // A small workspace summary.
    struct WorkspaceSummary: Codable {
        let name: String
        let fileCount: Int
        let updatedAt: Date

        var isEmpty: Bool {
            fileCount == 0
        }

        var detail: String {
            "\(name) · \(fileCount) files"
        }
    }

    let caption = "Read source with the same quiet colors as Changes. Long lines keep their indentation, or wrap to fit when you choose Wrap lines."

    enum PreviewMode: String {
        case code
        case plainText
    }

    func summary(for names: [String]) -> String {
        let visible = names
            .filter { !$0.hasPrefix(".") }
            .sorted()

        guard !visible.isEmpty else {
            return "No files"
        }

        return visible.joined(separator: ", ")
    }

    // Unicode remains intact: 你好 · café · 🌿
    let limit = 256 * 1024
    let mode: PreviewMode = .code
    """# + "\n"

    private static var demoPatch: String {
        let old = demoSource(new: false).split(separator: "\n").map(String.init)
        let new = demoSource(new: true).split(separator: "\n").map(String.init)
        var patch = "diff --git a/Sources/Example.swift b/Sources/Example.swift\n--- a/Sources/Example.swift\n+++ b/Sources/Example.swift\n"
        for start in [17, 42] {
            patch += "@@ -\(start),7 +\(start),7 @@\n"
            for index in start - 1..<start + 6 {
                patch += old[index] == new[index] ? " \(old[index])\n" : "-\(old[index])\n+\(new[index])\n"
            }
        }
        return patch + "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1,3 +1,3 @@\n # Demo workspace\n \n-File browsing.\n+Read-only file browsing.\n"
    }

    static func errorMessage(_ error: Error) -> String {
        if case RelayError.rpc(let message) = error,
           message.localizedCaseInsensitiveContains("unknown method"),
           message.contains("Workspace") {
            return "Update this session's remote Cypher engine to use Files. No terminal commands were used as a fallback."
        }
        return error.localizedDescription
    }
}

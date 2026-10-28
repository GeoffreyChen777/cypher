// Data source for one Files / Changes sheet: host RPCs (or the demo dataset)
// bound to a single chat's device, checkout and account.

import Foundation
import UIKit

enum WorkspaceBrowserError: LocalizedError {
    case unavailable, contextChanged, invalidPath, missingDirectory, tooLarge, changedWhileReading
    var errorDescription: String? {
        switch self {
        case .unavailable: "Connect to this session's device to browse its files."
        case .contextChanged: "The account, device or checkout changed. Reopen Files / Changes."
        case .invalidPath: "This path is not available in the workspace browser."
        case .missingDirectory:
            "This session has no working directory. Open a session in a project or worktree to browse files."
        case .tooLarge: "This file is too large to preview on iPhone."
        case .changedWhileReading: "The file changed while it was loading. Refresh to try again."
        }
    }
}

/// Which diff a Changes document shows: the checkout's working tree, or a
/// recorded round's (its start and end trees, read in the `trees` mode).
enum WorkspaceChangesScope: Hashable {
    case working
    case round(TurnChanges)
}

/// Where a file reference or link landed in the checkout.
enum WorkspaceResolution: Equatable {
    /// A file, relative to the checkout root.
    case file(String)
    case directory(String)
    /// Several files end in the reference; the reader picks.
    case ambiguous([String])
    /// Nothing at the path, and no file ending in it.
    case missing(String)
    /// The path lies outside the session's checkout (absolute).
    case outside(String)
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
            current.cwd == chat.cwd
        else { throw WorkspaceBrowserError.contextChanged }
        guard let cwd = chat.cwd, !cwd.isEmpty else { throw WorkspaceBrowserError.missingDirectory }
    }

    private func call<T: Decodable & Sendable>(
        _ method: String, path: String? = nil, extra: [String: Any] = [:]
    ) async throws -> T {
        try check()
        guard let workspace else { throw WorkspaceBrowserError.unavailable }
        var params: [String: Any] = ["chatId": chat.id, "cwd": chat.cwd ?? ""]
        params.merge(extra) { _, new in new }
        if let path {
            guard WorkspaceFilePath.valid(path) else { throw WorkspaceBrowserError.invalidPath }
            params["path"] = path
        }
        let result: T = try await workspace.workspaceBrowserCall(
            deviceId: chat.deviceId,
            method: method, params: params)
        try check()
        return result
    }

    func directory(_ path: String) async throws -> WorkspaceDirectory {
        try check()
        if isDemo {
            guard WorkspaceFilePath.valid(path), let entries = Self.demoTree[path] else {
                throw WorkspaceBrowserError.invalidPath
            }
            return WorkspaceDirectory(entries: entries, truncated: false)
        }
        return try await call("ListWorkspaceFiles", path: path)
    }

    func file(_ path: String) async throws -> WorkspaceFileContent {
        try check()
        guard WorkspaceFilePath.valid(path, allowRoot: false) else { throw WorkspaceBrowserError.invalidPath }
        if isDemo {
            if let data = Self.demoBytes(path) {
                return WorkspaceFileContent(text: nil, bytes: UInt64(data.count), binary: true, truncated: false)
            }
            guard let text = Self.demoText(path) else { throw WorkspaceBrowserError.invalidPath }
            return WorkspaceFileContent(text: text, bytes: UInt64(text.utf8.count), binary: false, truncated: false)
        }
        return try await call("ReadWorkspaceFile", path: path)
    }

    func changes(_ scope: WorkspaceChangesScope = .working) async throws -> WorkspaceChanges {
        try check()
        if isDemo {
            return WorkspaceChanges(
                checkoutId: "demo", deviceId: chat.deviceId, cwd: chat.cwd ?? "",
                patch: Self.demoPatch,
                files: [
                    .init(path: "Sources/Example.swift", status: "modified", additions: 2, deletions: 2, binary: false),
                    .init(path: "README.md", status: "modified", additions: 1, deletions: 1, binary: false),
                ],
                additions: 3, deletions: 3, truncated: false, checksum: "demo", updatedAt: "Demo snapshot")
        }
        let result: WorkspaceChanges = try await call("GetCheckoutDiff", extra: Self.modeParams(scope))
        guard result.deviceId == chat.deviceId else { throw WorkspaceBrowserError.contextChanged }
        return result
    }

    /// A round's diff reads back from its tree pair; the host passes only
    /// full object ids on to git.
    private static func modeParams(_ scope: WorkspaceChangesScope) -> [String: Any] {
        switch scope {
        case .working: ["mode": "working"]
        case .round(let changes): ["mode": "trees", "baseTree": changes.base, "targetTree": changes.head]
        }
    }

    func diffSources(
        snapshot: WorkspaceChanges, path: String, scope: WorkspaceChangesScope = .working
    ) async throws -> WorkspaceDiffSources {
        try check()
        guard snapshot.deviceId == chat.deviceId, !snapshot.truncated,
            snapshot.files.contains(where: { $0.path == path && !$0.binary }),
            WorkspaceFilePath.valid(path, allowRoot: false)
        else {
            throw WorkspaceDiffContextError.unavailable
        }
        if isDemo {
            let old = path == "README.md" ? "# Demo workspace\n\nFile browsing.\n" : Self.demoSource(new: false)
            let new =
                path == "README.md" ? "# Demo workspace\n\nRead-only file browsing.\n" : Self.demoSource(new: true)
            return try WorkspaceDiffSources(
                diffChecksum: "demo", oldText: old, newText: new,
                binary: false, truncated: false, stale: false
            ).validated(checksum: snapshot.checksum)
        }
        guard let workspace else { throw WorkspaceBrowserError.unavailable }
        var params: [String: Any] = [
            "checkoutId": snapshot.checkoutId, "cwd": snapshot.cwd, "chatId": chat.id,
            "path": path, "diffChecksum": snapshot.checksum,
        ]
        params.merge(Self.modeParams(scope)) { _, new in new }
        let result: WorkspaceDiffSources = try await workspace.workspaceBrowserCall(
            deviceId: chat.deviceId, method: "GetCheckoutFileDiffText", params: params)
        try check()
        return try result.validated(checksum: snapshot.checksum)
    }

    /// A file's raw bytes (image previews), read a slice at a time through
    /// `ReadWorkspaceFileChunk`. Refused past `limit`, and when the file is
    /// rewritten while it loads.
    func fileBytes(_ path: String, limit: UInt64 = 24 * 1024 * 1024) async throws -> Data {
        try check()
        guard WorkspaceFilePath.valid(path, allowRoot: false) else { throw WorkspaceBrowserError.invalidPath }
        if isDemo {
            guard let data = Self.demoBytes(path) else { throw WorkspaceBrowserError.invalidPath }
            return data
        }
        struct Chunk: Decodable, Sendable {
            var data: String
            var nextOffset: UInt64
            var bytes: UInt64
            var done: Bool
            var modifiedMs: Int64?
        }
        var data = Data()
        var offset: UInt64 = 0
        var modified: Int64?
        while true {
            let chunk: Chunk = try await call("ReadWorkspaceFileChunk", path: path, extra: ["offset": offset])
            guard chunk.bytes <= limit else { throw WorkspaceBrowserError.tooLarge }
            if offset > 0, chunk.modifiedMs != modified { throw WorkspaceBrowserError.changedWhileReading }
            modified = chunk.modifiedMs
            guard let slice = Data(base64Encoded: chunk.data) else { throw WorkspaceBrowserError.unavailable }
            data.append(slice)
            if chunk.done { return data }
            guard chunk.nextOffset > offset else { throw WorkspaceBrowserError.unavailable }
            offset = chunk.nextOffset
        }
    }

    /// Where a reference or link a reply wrote lands in this checkout
    /// (code_ref.rs `resolve`): the path is read at the checkout root first,
    /// then found by the file search among paths ending in it. A bare name
    /// is first read as the one longer path of that name the reply cites.
    func resolve(_ reference: WorkspaceReference) async throws -> WorkspaceResolution {
        try check()
        let written = reference.expandedPath
        let relative: String
        switch FileLink.resolve(written, cwd: chat.cwd ?? "") {
        case .success(let path): relative = path
        case .failure(let outside): return .outside(outside.path)
        }
        if relative.isEmpty { return .directory("") }
        guard WorkspaceFilePath.valid(relative, allowRoot: false) else { return .missing(relative) }
        if reference.dir {
            _ = try await directory(relative)
            return .directory(relative)
        }
        do {
            _ = try await file(relative)
            return .file(relative)
        } catch let error as RelayError {
            // The device itself is out of reach: a search would fail the same.
            if case .rpc = error {} else { throw error }
        } catch is CancellationError {
            throw CancellationError()
        } catch WorkspaceBrowserError.contextChanged {
            throw WorkspaceBrowserError.contextChanged
        } catch {}
        // A folder written without its trailing slash.
        if (try? await directory(relative)) != nil { return .directory(relative) }
        // An absolute path inside the checkout means exactly that file.
        if written.hasPrefix("/") { return .missing(relative) }
        var wanted = Substring(written)
        while wanted.hasPrefix("./") || wanted.hasPrefix("../") {
            wanted = wanted.drop { $0 == "." }.dropFirst()
        }
        let matches =
            isDemo
            ? Self.demoTree.flatMap { dir, entries in
                entries.map { FileSearchMatch(path: dir.isEmpty ? $0.name : "\(dir)/\($0.name)", isDir: $0.isDir) }
            }
            : try await model.searchFiles(
                MentionScope.Files(deviceId: chat.deviceId, chatId: chat.id), query: String(wanted))
        try check()
        var found: [String] = []
        for hit in matches where !hit.isDir && (hit.path == wanted || hit.path.hasSuffix("/" + wanted)) {
            if hit.path != relative, !found.contains(hit.path) { found.append(hit.path) }
        }
        switch found.count {
        case 0: return .missing(relative)
        case 1: return .file(found[0])
        default: return .ambiguous(found)
        }
    }

    // MARK: Demo checkout

    private static let demoTree: [String: [WorkspaceFileEntry]] = [
        "": [
            .init(name: "Sources", isDir: true), .init(name: "notes", isDir: true),
            .init(name: "README.md", isDir: false),
        ],
        "Sources": [.init(name: "Example.swift", isDir: false), .init(name: "ReaderExample.swift", isDir: false)],
        "notes": [
            .init(name: "guide.md", isDir: false), .init(name: "page.html", isDir: false),
            .init(name: "diagram.svg", isDir: false), .init(name: "icon.png", isDir: false),
        ],
    ]

    private static func demoText(_ path: String) -> String? {
        switch path {
        case "README.md": "# Demo workspace\n\nRead-only file browsing.\n"
        case "Sources/Example.swift": demoSource(new: true)
        case "Sources/ReaderExample.swift": readerExample
        case "notes/guide.md":
            """
            # Reading files on iPhone

            Markdown opens **rendered**, with a Source view a tap away.

            - HTML and SVG open as pages
            - Images show fitted, with their size
            - Everything else opens as code

            ```swift
            let message = "Hello from Cypher"
            ```

            """
        case "notes/page.html":
            """
            <!doctype html>
            <html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
            <title>Demo page</title>
            <style>body{font:16px -apple-system,sans-serif;margin:24px}h1{color:#6366f1}</style></head>
            <body><h1>Demo page</h1><p>An HTML file from the checkout, opened as a page.</p></body></html>

            """
        case "notes/diagram.svg":
            """
            <svg xmlns="http://www.w3.org/2000/svg" width="240" height="120" viewBox="0 0 240 120">
              <rect x="8" y="8" width="96" height="48" rx="10" fill="#6366f1"/>
              <rect x="136" y="64" width="96" height="48" rx="10" fill="#16a34a"/>
              <path d="M104 32 C 140 32, 140 88, 136 88" stroke="#888" stroke-width="3" fill="none"/>
            </svg>

            """
        default: nil
        }
    }

    /// The demo checkout's one image: a small generated PNG.
    static func demoBytes(_ path: String) -> Data? {
        guard path == "notes/icon.png" else { return nil }
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 96, height: 96), format: format)
        return renderer.pngData { _ in
            UIColor.systemIndigo.setFill()
            UIBezierPath(roundedRect: CGRect(x: 0, y: 0, width: 96, height: 96), cornerRadius: 22).fill()
            UIColor.white.setFill()
            UIBezierPath(ovalIn: CGRect(x: 28, y: 28, width: 40, height: 40)).fill()
        }
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
        var patch =
            "diff --git a/Sources/Example.swift b/Sources/Example.swift\n--- a/Sources/Example.swift\n+++ b/Sources/Example.swift\n"
        for start in [17, 42] {
            patch += "@@ -\(start),7 +\(start),7 @@\n"
            for index in start - 1..<start + 6 {
                patch += old[index] == new[index] ? " \(old[index])\n" : "-\(old[index])\n+\(new[index])\n"
            }
        }
        return patch
            + "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1,3 +1,3 @@\n # Demo workspace\n \n-File browsing.\n+Read-only file browsing.\n"
    }

    /// Git prunes the unreferenced trees a round is recorded against
    /// (`git gc`, two weeks by default; changes/render.rs `is_missing_object`).
    static func isMissingSnapshot(_ error: Error) -> Bool {
        guard case RelayError.rpc(let message) = error else { return false }
        return ["bad object", "invalid object", "not a tree object", "unknown revision"]
            .contains { message.contains($0) }
    }

    static func errorMessage(_ error: Error, scope: WorkspaceChangesScope = .working) -> String {
        if case .round = scope, isMissingSnapshot(error) {
            return "This turn's snapshot is no longer in the repository."
        }
        if case .round = scope, case RelayError.rpc(let message) = error,
            message.localizedCaseInsensitiveContains("unknown method")
                || message.contains("baseTree")
        {
            return "This session's device is running an older Cypher. Update it to view turn diffs."
        }
        if case RelayError.rpc(let message) = error,
            message.localizedCaseInsensitiveContains("unknown method"),
            message.contains("Workspace")
        {
            return
                "Update this session's remote Cypher engine to use Files. No terminal commands were used as a fallback."
        }
        return error.localizedDescription
    }
}

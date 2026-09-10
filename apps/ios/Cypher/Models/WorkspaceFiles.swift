import Foundation

struct WorkspaceFileEntry: Decodable, Hashable, Identifiable {
    var name: String
    var isDir: Bool
    var id: String { name }
}

struct WorkspaceDirectory: Decodable {
    var entries: [WorkspaceFileEntry]
    var truncated: Bool
}

struct WorkspaceFileContent: Decodable {
    var text: String?
    var bytes: UInt64
    var binary: Bool
    var truncated: Bool
}

struct WorkspaceChange: Decodable, Hashable, Identifiable {
    var path: String
    var oldPath: String?
    var status: String
    var additions: Int
    var deletions: Int
    var binary: Bool
    var id: String { path }
}

struct WorkspaceChanges: Decodable {
    var checkoutId: String
    var deviceId: String
    var cwd: String
    var patch: String
    var files: [WorkspaceChange]
    var additions: Int
    var deletions: Int
    var truncated: Bool
    var checksum: String
    var updatedAt: String

    /// Match complete headers against the snapshot's known paths, including
    /// Git's C/octal quoting. Never guess by basename, split on spaces, or
    /// pretend a missing/truncated section is an empty diff.
    func patch(for file: WorkspaceChange) -> String? {
        guard files.contains(file) else { return nil }
        return patches()[file.path]
    }

    /// Index the snapshot once for the continuous changes document.
    func patches() -> [String: String] {
        func candidates(_ entry: WorkspaceChange) -> Set<String> {
            Set(Self.gitNames("a/" + (entry.oldPath ?? entry.path)).flatMap { old in
                Self.gitNames("b/" + entry.path).map { "diff --git \(old) \($0)" }
            })
        }
        var owners: [String: Set<String>] = [:]
        for file in files {
            for header in candidates(file) { owners[header, default: []].insert(file.path) }
        }
        var result: [String: String] = [:]
        var duplicates = Set<String>()
        var currentPath: String?
        var current = ""
        func save() {
            guard let currentPath else { return }
            if result[currentPath] != nil { duplicates.insert(currentPath) }
            result[currentPath] = current
        }
        for line in patch.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.hasPrefix("diff --git ") {
                save()
                let paths = owners[String(line)]
                currentPath = paths?.count == 1 ? paths?.first : nil
                current = String(line)
            } else if currentPath != nil {
                current += "\n" + line
            }
        }
        save()
        for path in duplicates { result.removeValue(forKey: path) }
        return result
    }

    private static func gitNames(_ name: String) -> [String] {
        func quoted(escapeUnicode: Bool) -> String {
            var bytes: [UInt8] = [34]
            for byte in name.utf8 {
                let escaped: String?
                switch byte {
                case 7: escaped = "\\a"
                case 8: escaped = "\\b"
                case 9: escaped = "\\t"
                case 10: escaped = "\\n"
                case 11: escaped = "\\v"
                case 12: escaped = "\\f"
                case 13: escaped = "\\r"
                case 34: escaped = "\\\""
                case 92: escaped = "\\\\"
                case 0..<32, 127: escaped = String(format: "\\%03o", Int(byte))
                case 128...255 where escapeUnicode: escaped = String(format: "\\%03o", Int(byte))
                default: escaped = nil
                }
                if let escaped { bytes += escaped.utf8 } else { bytes.append(byte) }
            }
            bytes.append(34)
            return String(decoding: bytes, as: UTF8.self)
        }
        return [name, quoted(escapeUnicode: true), quoted(escapeUnicode: false)]
    }
}

struct WorkspaceDiffEntry: Equatable {
    let path: String
    let patch: String?
    let additions: Int
    let deletions: Int
    let binary: Bool
    var arguments: [String: Any] {
        ["path": path, "patch": patch ?? "", "additions": additions,
         "deletions": deletions, "binary": binary]
    }
}

struct WorkspaceDiffSources: Decodable {
    let diffChecksum: String
    let oldText: String?
    let newText: String?
    let binary: Bool
    let truncated: Bool
    let stale: Bool

    func validated(checksum: String) throws -> Self {
        guard !stale, diffChecksum == checksum else { throw WorkspaceDiffContextError.stale }
        guard !binary, !truncated,
              (oldText?.utf8.count ?? 0) + (newText?.utf8.count ?? 0) <= 512 * 1024,
              [oldText, newText].compactMap({ $0 }).allSatisfy({ $0.split(separator: "\n", omittingEmptySubsequences: false).count <= 10_000 }),
              oldText != nil || newText != nil else { throw WorkspaceDiffContextError.unavailable }
        return self
    }
}

enum WorkspaceDiffContextError: Error {
    case stale, unavailable
}

enum WorkspaceFilePath {
    static func valid(_ path: String, allowRoot: Bool = true) -> Bool {
        if path.isEmpty { return allowRoot }
        return path.utf8.count <= 4096 && !path.contains("\0")
            && path.split(separator: "/", omittingEmptySubsequences: false)
                .allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." && $0.lowercased() != ".git" }
    }
    static func child(_ name: String, in directory: String) -> String? {
        guard !name.contains("/") else { return nil }
        let result = directory.isEmpty ? name : directory + "/" + name
        return valid(result, allowRoot: false) ? result : nil
    }
}

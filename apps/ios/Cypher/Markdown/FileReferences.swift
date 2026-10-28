// File references and file links in chat Markdown — ports of the desktop's
// crates/ui/src/markdown/code_ref.rs and crates/ui/src/files/links.rs.
//
// A *reference* is inline code an agent cites code with (`src/a.rs:12-20`):
// the transcript shows it as a short chip instead of the long path. A *link*
// is a Markdown link whose destination is a file (`src/a.rs`, `a.rs#L12`,
// `file:///…`, `cypher-file:`). Both open the file in the session's Files
// sheet, at its lines.

import Foundation

/// One file reference in inline code.
struct FileReference: Hashable {
    /// As written, without its line suffix: relative (to the checkout or to
    /// some folder in it) or absolute.
    var path: String
    /// The cited lines, 1-based and inclusive; nil cites the whole file.
    var lines: ClosedRange<Int>?

    /// The path's last segment.
    var fileName: String {
        path.split(separator: "/", omittingEmptySubsequences: false).last.map(String.init) ?? path
    }

    /// The folder part of the path ("" for a bare name).
    var dir: String {
        guard let slash = path.lastIndex(of: "/") else { return "" }
        return slash == path.startIndex ? "/" : String(path[..<slash])
    }

    /// `:12` or `:12-20`; empty for a whole file.
    var lineSuffix: String {
        guard let lines else { return "" }
        return lines.count > 1 ? ":\(lines.lowerBound)-\(lines.upperBound)" : ":\(lines.lowerBound)"
    }

    /// The reference a span of inline code holds, if it is one. Deliberately
    /// narrow — inline code is mostly identifiers, commands and values, and a
    /// false positive would hide what the agent wrote. A span is a reference
    /// when it is one path-shaped token that names a file and either cites
    /// lines (`file.py:128-131`, `a.rs#L7`) or has a folder (`core/x.py`).
    static func detect(_ code: String) -> FileReference? {
        let text = code.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty, text.utf8.count <= 512, !text.contains("://"),
            text.unicodeScalars.allSatisfy(isPathCharacter)
        else { return nil }
        let (path, lines) = splitLines(text)
        guard !path.isEmpty, !path.hasSuffix("/"), !path.contains(":"), !path.contains("#"),
            !path.hasPrefix("-"), !path.contains("//")
        else { return nil }
        let name = path.split(separator: "/", omittingEmptySubsequences: false).last.map(String.init) ?? path
        guard namesFile(name), lines != nil || path.contains("/") else { return nil }
        // `./` and `~/` prefixes are fine; a path of only dots is not a file.
        if path.split(separator: "/", omittingEmptySubsequences: false)
            .allSatisfy({ $0.isEmpty || $0.allSatisfy { $0 == "." || $0 == "~" } })
        {
            return nil
        }
        return FileReference(path: path, lines: lines)
    }

    /// What each chip of one text shows, in order: the file name, or
    /// `folder/name` where two references share a name but not a path
    /// (`a/mod.rs` and `b/mod.rs` must not read the same), then the lines.
    static func labels(_ refs: [FileReference]) -> [String] {
        refs.map { ref in
            let name = ref.fileName
            let clashes = refs.contains { $0.fileName == name && $0.path != ref.path && !$0.dir.isEmpty }
            let parent = ref.dir.split(separator: "/").last.map(String.init) ?? ""
            let shown = clashes && !parent.isEmpty ? "\(parent)/\(name)" : name
            return shown + ref.lineSuffix
        }
    }

    /// The longer path this reference most likely abbreviates: the one other
    /// path among `context` that ends in it, when exactly one does
    /// (`file.py` beside `utils/processors/file.py`).
    func expanded(with context: [String]) -> String? {
        let tail = "/" + (path.hasPrefix("./") ? String(path.dropFirst(2)) : path)
        let longer = Set(context.filter { $0.hasSuffix(tail) })
        return longer.count == 1 ? longer.first : nil
    }

    private static func isPathCharacter(_ c: Unicode.Scalar) -> Bool {
        CharacterSet.alphanumerics.contains(c) || "_-./~@+:#".unicodeScalars.contains(c)
    }

    /// A trailing `:12`, `:12-20`, `:12:5` (column dropped), `#L12`,
    /// `#L12-L20` or `#L12-20` read as the cited lines.
    private static func splitLines(_ text: String) -> (String, ClosedRange<Int>?) {
        if let hash = text.range(of: "#L", options: .backwards),
            let lines = parseRange(String(text[hash.upperBound...]), fragment: true)
        {
            return (String(text[..<hash.lowerBound]), lines)
        }
        guard let colon = text.lastIndex(of: ":") else { return (text, nil) }
        let head = String(text[..<colon])
        let last = String(text[text.index(after: colon)...])
        // `a.rs:12:5` — a column after the line.
        if let inner = head.lastIndex(of: ":") {
            let line = String(head[head.index(after: inner)...])
            if isDigits(line), isDigits(last), let n = Int(line), n > 0 {
                return (String(head[..<inner]), n...n)
            }
        }
        if let lines = parseRange(last, fragment: false) { return (head, lines) }
        return (text, nil)
    }

    /// `12` or `12-20` (with `L` before the end too in a `#L` fragment). A
    /// reversed range reads as its first line.
    private static func parseRange(_ text: String, fragment: Bool) -> ClosedRange<Int>? {
        let parts = text.split(separator: "-", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        guard isDigits(parts[0]), let first = Int(parts[0]), first > 0 else { return nil }
        guard parts.count == 2 else { return first...first }
        var last = parts[1]
        if fragment, last.hasPrefix("L") { last.removeFirst() }
        guard isDigits(last) else { return nil }
        guard let end = Int(last), end >= first else { return first...first }
        return first...end
    }

    private static func isDigits(_ text: String) -> Bool {
        !text.isEmpty && text.utf8.count <= 9 && text.utf8.allSatisfy { $0 >= 48 && $0 <= 57 }
    }

    /// `name.ext` with a short extension that has a letter in it (`v1.2` and
    /// `3.14` do not), a dotfile, or a build file without an extension.
    private static func namesFile(_ name: String) -> Bool {
        let bare: Set<String> = [
            "Makefile", "makefile", "GNUmakefile", "Dockerfile", "Containerfile", "Justfile",
            "justfile", "Gemfile", "Rakefile", "Procfile", "Vagrantfile", "Brewfile", "BUILD",
            "WORKSPACE", "LICENSE", "README", "CODEOWNERS",
        ]
        if bare.contains(name) { return true }
        guard let dot = name.lastIndex(of: ".") else { return false }
        let stem = name[..<dot]
        let ext = name[name.index(after: dot)...]
        let alphanumeric = { (c: Character) in c.isASCII && (c.isLetter || c.isNumber) }
        if stem.isEmpty {
            // A dotfile (`.env`, `.gitignore`).
            return ext.count >= 2 && ext.allSatisfy { alphanumeric($0) || $0 == "_" }
        }
        return (1...12).contains(ext.count) && ext.allSatisfy(alphanumeric)
            && ext.contains { $0.isASCII && $0.isLetter } && !stem.hasSuffix(".")
    }
}

// MARK: - Links

/// A file a chat link points at.
struct FileLink: Hashable {
    /// As written, percent-decoded: absolute, or relative to the checkout.
    var path: String
    /// The cited lines, from a `:12` / `:12-20` suffix or a `#L12` fragment.
    var lines: ClosedRange<Int>?
    /// Written with a trailing slash: a folder.
    var dir = false

    /// The file `url` points at, or nil when it is not a file link (another
    /// scheme — `https:`, `mailto:`, `cypher-session:` — keeps opening
    /// through the system).
    static func parse(_ url: String) -> FileLink? {
        let url = url.trimmingCharacters(in: .whitespaces)
        var rest: Substring
        if url.hasPrefix("cypher-file:") {
            rest = url.dropFirst("cypher-file:".count)
        } else if url.hasPrefix("zeron-file:") {
            rest = url.dropFirst("zeron-file:".count)
        } else if url.lowercased().hasPrefix("file://") {
            rest = url.dropFirst(7)
            if rest.hasPrefix("localhost") { rest = rest.dropFirst("localhost".count) }
        } else if hasScheme(url) {
            return nil
        } else {
            rest = Substring(url)
        }
        var fragment: Substring?
        if let hash = rest.firstIndex(of: "#") {
            fragment = rest[rest.index(after: hash)...]
            rest = rest[..<hash]
        }
        if let query = rest.firstIndex(of: "?") { rest = rest[..<query] }
        var path = String(rest).removingPercentEncoding ?? String(rest)
        var lines = fragment.flatMap { fragmentLines(String($0)) }
        if lines == nil, let (head, at) = splitLineSuffix(path) {
            path = head
            lines = at
        }
        let dir = path.hasSuffix("/")
        while path.hasSuffix("/") { path.removeLast() }
        guard !path.isEmpty else { return nil }
        return FileLink(path: path, lines: lines, dir: dir)
    }

    /// Whether `url` starts with a URL scheme (`https:`, `mailto:`) — not a
    /// drive letter, and not a `name.rs:12` line suffix.
    private static func hasScheme(_ url: String) -> Bool {
        guard let colon = url.firstIndex(of: ":") else { return false }
        let scheme = url[..<colon]
        let after = url[url.index(after: colon)...].split(separator: ":", omittingEmptySubsequences: false).first
        return scheme.count >= 2 && scheme.first?.isLetter == true
            && scheme.allSatisfy { ($0.isASCII && ($0.isLetter || $0.isNumber)) || "+-.".contains($0) }
            && lineRange(String(after ?? "")) == nil
    }

    /// The lines of a `L12`, `L12-L20`, `L12-20` or `L12C3` fragment.
    private static func fragmentLines(_ fragment: String) -> ClosedRange<Int>? {
        func leadingNumber(_ text: Substring) -> Int? {
            guard let first = text.first, first == "L" || first == "l" else { return nil }
            return Int(text.dropFirst().prefix { $0.isASCII && $0.isNumber }).flatMap { $0 > 0 ? $0 : nil }
        }
        guard let first = leadingNumber(Substring(fragment)) else { return nil }
        var last = first
        if let dash = fragment.firstIndex(of: "-") {
            let to = fragment[fragment.index(after: dash)...]
            if let end = leadingNumber(to) ?? Int(to), end >= first { last = end }
        }
        return first...last
    }

    /// `a.rs:12`, `a.rs:12:5` or `a.rs:12-20` → (`a.rs`, 12…12 or 12…20).
    private static func splitLineSuffix(_ path: String) -> (String, ClosedRange<Int>)? {
        guard let colon = path.lastIndex(of: ":"),
            let lastLines = lineRange(String(path[path.index(after: colon)...]))
        else { return nil }
        var head = String(path[..<colon])
        var lines = lastLines
        if let inner = head.lastIndex(of: ":"), inner != head.startIndex {
            let previous = String(head[head.index(after: inner)...])
            if !previous.isEmpty, previous.allSatisfy({ $0.isASCII && $0.isNumber }), let line = Int(previous) {
                head = String(head[..<inner])
                lines = line...line
            }
        }
        guard !head.isEmpty, lines.lowerBound > 0 else { return nil }
        return (head, lines)
    }

    /// `12` → 12…12; `12-20` → 12…20. A reversed range keeps its start.
    private static func lineRange(_ text: String) -> ClosedRange<Int>? {
        let parts = text.split(separator: "-", maxSplits: 1, omittingEmptySubsequences: false).map(String.init)
        let digits = { (s: String) in !s.isEmpty && s.allSatisfy { $0.isASCII && $0.isNumber } }
        guard digits(parts[0]), let first = Int(parts[0]) else { return nil }
        guard parts.count == 2 else { return first...first }
        guard digits(parts[1]) else { return nil }
        guard let last = Int(parts[1]), last >= first else { return first...first }
        return first...last
    }

    /// `path` inside the checkout at `cwd`, relative to it ("" is the root),
    /// or the absolute path when it lies outside (`.failure`).
    static func resolve(_ path: String, cwd: String) -> Result<String, OutsideCheckout> {
        let root = normalize(cwd)
        let full = path.hasPrefix("/") ? normalize(path) : normalize("\(root)/\(path)")
        if full == root { return .success("") }
        if root == "/" { return .success(String(full.dropFirst())) }
        if full.hasPrefix(root + "/") { return .success(String(full.dropFirst(root.count + 1))) }
        return .failure(OutsideCheckout(path: full))
    }

    struct OutsideCheckout: Error, Equatable {
        var path: String
    }

    /// An absolute path with `.` and `..` folded away and no trailing slash.
    private static func normalize(_ path: String) -> String {
        var parts: [Substring] = []
        for part in path.split(separator: "/", omittingEmptySubsequences: true) {
            switch part {
            case ".": continue
            case "..": _ = parts.popLast()
            default: parts.append(part)
            }
        }
        return "/" + parts.joined(separator: "/")
    }
}

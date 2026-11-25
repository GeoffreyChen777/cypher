// Where a chat image's bytes come from — a port of the desktop's
// crates/ui/src/markdown/image_source.rs. Both apps run the same table,
// protocol/vectors/image-sources-v1.json (ImageSourceTests).
//
// - `https://` URLs load from the network (no cookies, no credentials);
//   plain `http://` is refused as insecure. A URL whose path names a known
//   non-image file (`.pdf`) is not an image; one without an extension is
//   tried, and its bytes decide.
// - `data:image/…;base64,` URIs carry their bytes; another media type is not
//   an image, and a data URI that is not base64 is unsupported.
// - Anything else is a path on the session's host — absolute, `~`, relative,
//   `file://` or `cypher-file:`, percent-decoded, with a `#L12`/`:12` suffix
//   or a `?query` dropped — and must name a png, jpg/jpeg, gif, webp or svg
//   file (any case). It is read through the engine, so only inside one of
//   the chat's roots: a relative path is looked up from the chat's folder
//   first, then from each root in order; an absolute one only in the
//   innermost root holding it. Each try is a root and the path inside it.
// - Every source is capped at `maxBytes`.

import Foundation

enum ImageSource {
    /// The most bytes one image may be (fetched, read or decoded from a data
    /// URI); a larger one shows an error instead.
    static let maxBytes: UInt64 = 20 * 1024 * 1024

    /// The image formats the chat shows.
    enum Format: String, Hashable, Sendable {
        case png, jpeg, gif, webp, svg

        /// By file extension, any case (`PNG`, `jpeg`).
        init?(extension ext: some StringProtocol) {
            switch ext.lowercased() {
            case "png": self = .png
            case "jpg", "jpeg": self = .jpeg
            case "gif": self = .gif
            case "webp": self = .webp
            case "svg": self = .svg
            default: return nil
            }
        }

        /// By media type (`image/png`; parameters ignored).
        init?(mime: some StringProtocol) {
            let bare = mime.split(separator: ";", omittingEmptySubsequences: false).first ?? ""
            switch bare.trimmingCharacters(in: .whitespaces).lowercased() {
            case "image/png": self = .png
            case "image/jpeg", "image/jpg": self = .jpeg
            case "image/gif": self = .gif
            case "image/webp": self = .webp
            case "image/svg+xml": self = .svg
            default: return nil
            }
        }

        /// By a path's (or URL path's) extension.
        init?(path: some StringProtocol) {
            guard let ext = ImageSource.fileExtension(path) else { return nil }
            self.init(extension: ext)
        }

        /// From the bytes themselves (magic numbers; SVG by its root element).
        init?(sniffing data: Data) {
            let bytes = [UInt8](data.prefix(1024))
            func starts(_ prefix: [UInt8]) -> Bool { bytes.starts(with: prefix) }
            if starts([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
                self = .png
            } else if starts([0xFF, 0xD8, 0xFF]) {
                self = .jpeg
            } else if starts(Array("GIF87a".utf8)) || starts(Array("GIF89a".utf8)) {
                self = .gif
            } else if bytes.count >= 12, bytes[0..<4].elementsEqual("RIFF".utf8),
                bytes[8..<12].elementsEqual("WEBP".utf8)
            {
                self = .webp
            } else {
                var head = String(decoding: bytes, as: UTF8.self)
                if head.hasPrefix("\u{feff}") { head.removeFirst() }
                let trimmed = head.drop { $0.isWhitespace }
                guard trimmed.hasPrefix("<svg") || (trimmed.hasPrefix("<?xml") && trimmed.contains("<svg"))
                else { return nil }
                self = .svg
            }
        }

        var mime: String {
            switch self {
            case .png: "image/png"
            case .jpeg: "image/jpeg"
            case .gif: "image/gif"
            case .webp: "image/webp"
            case .svg: "image/svg+xml"
            }
        }

        /// What a caption or a share names the format.
        var label: String { self == .jpeg ? "JPEG" : rawValue.uppercased() }
        var fileExtension: String { self == .jpeg ? "jpg" : rawValue }
    }

    /// Why a source shows an error instead of an image.
    enum Rejection: String, Hashable, Sendable {
        /// Plain `http://`.
        case insecure
        /// A data URI that is not base64.
        case unsupported
    }

    /// One place to read a local image: a root and the path inside it.
    struct Candidate: Hashable, Sendable {
        var root: String
        var path: String
    }

    /// What an image source is.
    enum Kind: Hashable, Sendable {
        /// A file on the session's host: try each candidate in order.
        case local(candidates: [Candidate], format: Format)
        /// A file outside every root the chat may read (absolute, normalized).
        case outside(path: String)
        case remote(url: String)
        /// A `data:` URI; `payload` is its base64.
        case data(format: Format, payload: String)
        case rejected(Rejection)
        /// Not an image: it stays a link or a file chip.
        case notImage
    }

    /// Where local paths resolve: the chat's folder, the roots it may read
    /// (in lookup order) and, when known, the host's home folder for `~`.
    struct Context: Hashable, Sendable {
        var cwd: String?
        var roots: [String] = []
        var home: String?
    }

    /// Whether `src` is an image at all, before any context: what the parser
    /// asks to decide between an image block and alt text.
    static func isImage(_ src: String) -> Bool {
        classify(src, Context()) != .notImage
    }

    /// What `src` is, with local paths resolved in `context`.
    static func classify(_ src: String, _ context: Context) -> Kind {
        let src = src.trimmingCharacters(in: .whitespacesAndNewlines)
        if src.isEmpty { return .notImage }
        if let rest = stripPrefix(src, "data:") { return dataURI(rest) }
        if stripPrefix(src, "http://") != nil {
            return remoteMayBeImage(src) ? .rejected(.insecure) : .notImage
        }
        if let rest = stripPrefix(src, "https://") {
            if rest.isEmpty || !remoteMayBeImage(src) { return .notImage }
            return .remote(url: src)
        }
        var path: Substring
        if let rest = stripPrefix(src, "file://") {
            path = rest.hasPrefix("localhost") ? rest.dropFirst("localhost".count) : rest
        } else if let rest = stripPrefix(src, "cypher-file:") {
            path = rest
        } else if hasScheme(src) {
            return .notImage
        } else {
            path = Substring(src)
        }
        if let cut = path.firstIndex(where: { $0 == "#" || $0 == "?" }) { path = path[..<cut] }
        let decoded = percentDecode(path) ?? String(path)
        let bare = stripLineSuffix(decoded)
        guard let format = Format(path: bare) else { return .notImage }
        return local(String(bare), format: format, context)
    }

    /// The image a whole reply paragraph names, given as its Markdown source
    /// (trimmed): one path or URL, bare, in backticks or in `<…>`, that names
    /// a supported image file. nil for anything else (prose, a link, several
    /// tokens, a path to another kind of file, plain `http://`).
    static func imageLine(_ line: String) -> String? {
        let line = line.trimmingCharacters(in: .whitespacesAndNewlines)
        let inner: String
        if line.count >= 2, line.hasPrefix("`"), line.hasSuffix("`") {
            inner = String(line.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        } else if line.count >= 2, line.hasPrefix("<"), line.hasSuffix(">") {
            inner = String(line.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
        } else {
            if line.contains(where: { "*[]()<>`!".contains($0) }) { return nil }
            inner = line
        }
        if inner.isEmpty || inner.contains(where: \.isWhitespace) || inner.contains("`") { return nil }
        let named: Bool
        switch classify(inner, Context()) {
        case .local, .outside: named = true
        // A bare URL is an image only when its path says so.
        case .remote(let url): named = remoteExtension(url).flatMap { Format(extension: $0) } != nil
        case .data, .rejected, .notImage: named = false
        }
        return named ? inner : nil
    }

    // MARK: Pieces

    private static func dataURI(_ rest: Substring) -> Kind {
        guard let comma = rest.firstIndex(of: ",") else { return .notImage }
        let meta = rest[..<comma]
        let payload = rest[rest.index(after: comma)...]
        let parts = meta.split(separator: ";", omittingEmptySubsequences: false)
        guard let format = Format(mime: parts.first ?? "") else { return .notImage }
        guard parts.dropFirst().contains(where: { $0.trimmingCharacters(in: .whitespaces).lowercased() == "base64" })
        else { return .rejected(.unsupported) }
        return .data(format: format, payload: payload.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// An `http(s)` URL may be an image unless its path names another kind
    /// of file.
    private static func remoteMayBeImage(_ url: String) -> Bool {
        guard let ext = remoteExtension(url) else { return true }
        return Format(extension: ext) != nil
    }

    /// The extension of a URL's last path segment, if it has one.
    static func remoteExtension(_ url: String) -> Substring? {
        let afterScheme = url.range(of: "://").map { url[$0.upperBound...] } ?? Substring(url)
        let path = afterScheme.prefix { $0 != "?" && $0 != "#" }
        guard let slash = path.firstIndex(of: "/") else { return nil }
        return fileExtension(path[path.index(after: slash)...])
    }

    /// Where a local path resolved to (desktop image_source.rs `Place`): an
    /// absolute path (or `~/…` under an unknown home), or above an unknown
    /// home (`~/../x`), which no root can hold — kept as written.
    private enum Place {
        case at(String)
        case above(String)

        var path: String {
            switch self {
            case .at(let path), .above(let path): return path
            }
        }
    }

    /// `~` expands only to land inside a root: with the host's home known it
    /// is that folder; unknown, `~` stays the head of the path and matches
    /// roots written the same way. The path is resolved first and then
    /// checked against the roots, so a `..` that climbs out of every root
    /// (or above an unknown home) reads nothing.
    private static func local(_ path: String, format: Format, _ context: Context) -> Kind {
        let home = context.home.map { trimTrailingSlashes($0) }
        func absolute(_ path: String) -> Place? {
            if path == "~" || path.hasPrefix("~/") {
                let rest = String(path.dropFirst())
                if let home { return .at(normalize(home + rest)) }
                return tildePlace(rest, written: path)
            }
            return path.hasPrefix("/") ? .at(normalize(path)) : nil
        }
        let roots: [String] = context.roots.compactMap {
            if case .at(let root) = absolute($0) { return root }
            return nil
        }
        var tries: [Place] = []
        if let full = absolute(path) {
            tries.append(full)
        } else {
            let bases = (context.cwd.flatMap(absolute).map { [$0] } ?? []) + roots.map { Place.at($0) }
            for base in bases {
                switch base {
                case .at(let base) where base.hasPrefix("~"):
                    tries.append(tildePlace("\(base.dropFirst())/\(path)", written: "\(base)/\(path)"))
                case .at(let base):
                    tries.append(.at(normalize("\(base)/\(path)")))
                case .above(let base):
                    tries.append(.above("\(base)/\(path)"))
                }
            }
        }
        var candidates: [Candidate] = []
        for place in tries {
            guard case .at(let full) = place else { continue }
            // The innermost root holding it (the last of the longest).
            var best: (root: String, rel: String)?
            for root in roots {
                guard let rel = inside(full, root: root) else { continue }
                if best == nil || root.count >= best!.root.count { best = (root, rel) }
            }
            if let best, !best.rel.isEmpty {
                let candidate = Candidate(root: best.root, path: best.rel)
                if !candidates.contains(candidate) { candidates.append(candidate) }
            }
        }
        if candidates.isEmpty { return .outside(path: tries.first?.path ?? path) }
        return .local(candidates: candidates, format: format)
    }

    /// `~` + `rest` under an unknown home: folded, unless a `..` climbs
    /// above the `~` (then `written` is where it pointed).
    private static func tildePlace(_ rest: String, written: String) -> Place {
        guard let folded = fold(rest) else { return .above(written) }
        return .at("~" + trimTrailingSlashes(folded))
    }

    /// `full` relative to `root` when inside it.
    private static func inside(_ full: String, root: String) -> String? {
        if root == "/" { return String(full.drop { $0 == "/" }) }
        if full == root { return "" }
        guard full.hasPrefix(root + "/") else { return nil }
        return String(full.dropFirst(root.count + 1))
    }

    /// An absolute path with `.` and `..` folded away and no trailing slash.
    static func normalize(_ path: String) -> String {
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

    /// `normalize` of a path under a head that is not `/` (an unknown `~`):
    /// nil when a `..` climbs above that head.
    private static func fold(_ path: String) -> String? {
        var parts: [Substring] = []
        for part in path.split(separator: "/", omittingEmptySubsequences: true) {
            switch part {
            case ".": continue
            case "..": if parts.popLast() == nil { return nil }
            default: parts.append(part)
            }
        }
        return "/" + parts.joined(separator: "/")
    }

    private static func trimTrailingSlashes(_ path: String) -> String {
        var path = path
        while path.hasSuffix("/") { path.removeLast() }
        return path
    }

    /// `a.png:12`, `a.png:12-20` or `a.png:12:5` → `a.png`.
    private static func stripLineSuffix(_ path: String) -> Substring {
        var out = Substring(path)
        for _ in 0..<2 {
            guard let colon = out.lastIndex(of: ":") else { break }
            let head = out[..<colon]
            let tail = out[out.index(after: colon)...]
            guard !head.isEmpty, !tail.isEmpty, tail.allSatisfy({ ($0.isASCII && $0.isNumber) || $0 == "-" })
            else { break }
            out = head
        }
        return out
    }

    static func fileExtension(_ path: some StringProtocol) -> Substring? {
        let path = Substring(path)
        let name = path.split(separator: "/", omittingEmptySubsequences: false).last ?? path
        guard let dot = name.lastIndex(of: "."), dot > name.startIndex else { return nil }
        return name[name.index(after: dot)...]
    }

    /// `text` past `prefix` (lowercase ASCII), matched in any case.
    private static func stripPrefix(_ text: String, _ prefix: String) -> Substring? {
        let lower = { (byte: UInt8) in byte >= 0x41 && byte <= 0x5A ? byte | 0x20 : byte }
        guard text.utf8.count >= prefix.utf8.count,
            text.utf8.prefix(prefix.utf8.count).map(lower).elementsEqual(prefix.utf8)
        else { return nil }
        return text[text.utf8.index(text.utf8.startIndex, offsetBy: prefix.utf8.count)...]
    }

    /// A URL scheme (`mailto:`, `ftp:`) — not a drive letter or a `:12`
    /// suffix.
    private static func hasScheme(_ text: String) -> Bool {
        guard let colon = text.firstIndex(of: ":") else { return false }
        let scheme = text[..<colon]
        return scheme.count >= 2 && scheme.first.map { $0.isASCII && $0.isLetter } == true
            && scheme.allSatisfy { ($0.isASCII && ($0.isLetter || $0.isNumber)) || $0 == "+" || $0 == "-" || $0 == "." }
            && !scheme.contains(".")
    }

    private static func percentDecode(_ text: Substring) -> String? {
        guard text.contains("%") else { return nil }
        let raw = Array(text.utf8)
        var bytes: [UInt8] = []
        var at = 0
        while at < raw.count {
            if raw[at] == UInt8(ascii: "%") {
                guard at + 2 < raw.count,
                    let byte = UInt8(String(decoding: raw[(at + 1)...(at + 2)], as: UTF8.self), radix: 16)
                else { return nil }
                bytes.append(byte)
                at += 3
            } else {
                bytes.append(raw[at])
                at += 1
            }
        }
        return String(bytes: bytes, encoding: .utf8)
    }
}

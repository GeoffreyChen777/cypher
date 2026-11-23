// Mirror of crates/proto/src/git_refs.rs: pull request, issue and commit
// references in chat text (`#123`, `owner/repo#123`, GitHub pull/issue/commit
// URLs, commit SHAs), and the `ResolveGitRef` wire shapes. Both sides run
// protocol/vectors/git-refs-v1.json (protocol/README.md); change the rules,
// this mirror and the vectors together. The rules are documented once, in
// the Rust module's header.

import Foundation

/// One reference found in chat text; also the `ref` of `ResolveGitRef`.
enum GitRef: Hashable, Sendable {
    /// An issue or pull request number; `repo` nil = the chat checkout's.
    case issue(repo: String?, number: UInt64)
    /// A commit, full or abbreviated, lowercase.
    case commit(repo: String?, sha: String)

    var repo: String? {
        switch self {
        case .issue(let repo, _), .commit(let repo, _): repo
        }
    }

    /// The `ref` param of `ResolveGitRef`.
    var wire: [String: Any] {
        var value: [String: Any]
        switch self {
        case .issue(_, let number): value = ["kind": "issue", "number": number]
        case .commit(_, let sha): value = ["kind": "commit", "sha": sha]
        }
        if let repo { value["repo"] = repo }
        return value
    }

    /// Well-formed enough to put in a URL (git_refs.rs `is_valid`).
    var isValid: Bool {
        guard repo.map(GitRefs.validRepo) ?? true else { return false }
        switch self {
        case .issue(_, let number): return number > 0 && number < 1_000_000_000
        case .commit(_, let sha): return GitRefs.isShaLike(Array(sha.utf8)) && !GitRefs.hasUpper(Array(sha.utf8))
        }
    }

    /// The github.com page for the ref, given the checkout's repository for
    /// an unqualified one.
    func webURL(checkoutRepo: String? = nil) -> URL? {
        guard let repo = repo ?? checkoutRepo else { return nil }
        switch self {
        case .issue(_, let number): return URL(string: "https://github.com/\(repo)/issues/\(number)")
        case .commit(_, let sha): return URL(string: "https://github.com/\(repo)/commit/\(sha)")
        }
    }

    /// What a card or a copy names it by: `#12`, `o/r#12`, `a1b2c3d`.
    var label: String {
        switch self {
        case .issue(let repo, let number): "\(repo ?? "")#\(number)"
        case .commit(_, let sha): String(sha.prefix(7))
        }
    }
}

/// A reference and the UTF-8 byte range of the text it was found at.
struct GitRefMatch: Hashable, Sendable {
    var range: Range<Int>
    var ref: GitRef

    /// The matched substring of `text`.
    func text(in text: String) -> String {
        let bytes = Array(text.utf8)
        return String(decoding: bytes[range], as: UTF8.self)
    }
}

enum GitRefs {
    static let minShaLength = 7
    static let maxShaLength = 40
    static let maxNumberDigits = 9

    /// Every reference in `text`, in order. `inCode`: `text` is one inline
    /// code span (all or nothing).
    static func find(_ text: String, inCode: Bool) -> [GitRefMatch] {
        let bytes = Array(text.utf8)
        if inCode { return codeRef(bytes).map { [$0] } ?? [] }
        let urls = urlSpans(bytes)
        let inURL = { (at: Int) in urls.contains { $0.contains(at) } }
        var found: [GitRefMatch] = urls.compactMap { url in
            githubURLRef(Array(bytes[url])).map { GitRefMatch(range: url, ref: $0) }
        }
        for at in bytes.indices where bytes[at] == UInt8(ascii: "#") && !inURL(at) {
            if let match = numberRef(bytes, hash: at) { found.append(match) }
        }
        var at = 0
        while at < bytes.count {
            guard isWordByte(bytes[at]) else {
                at += 1
                continue
            }
            let start = at
            while at < bytes.count, isWordByte(bytes[at]) { at += 1 }
            if !inURL(start), let sha = proseSHA(bytes, start, at) {
                found.append(GitRefMatch(range: start..<at, ref: .commit(repo: nil, sha: sha)))
            }
        }
        // A stable sort by start, as Rust's `sort_by_key`.
        return found.enumerated()
            .sorted { ($0.element.range.lowerBound, $0.offset) < ($1.element.range.lowerBound, $1.offset) }
            .map(\.element)
    }

    // MARK: Inline code

    private static func isSpace(_ b: UInt8) -> Bool {
        b == 0x20 || (0x09...0x0D).contains(b)
    }

    private static func codeRef(_ text: [UInt8]) -> GitRefMatch? {
        // Rust trims Unicode whitespace; a non-ASCII space inside the span
        // makes it no ref either way (no rule accepts non-ASCII bytes).
        let string = String(decoding: text, as: UTF8.self)
        let rest = string.drop(while: \.isWhitespace)
        let trimmedString = String(String(rest.reversed().drop(while: \.isWhitespace)).reversed())
        guard !trimmedString.isEmpty, !trimmedString.contains(where: \.isWhitespace) else { return nil }
        let leading = string.utf8.count - rest.utf8.count
        let trimmed = Array(trimmedString.utf8)
        let range = leading..<(leading + trimmed.count)
        let ref: GitRef
        if hasScheme(trimmed, 0) {
            guard let url = urlSpans(trimmed).first, url.count == trimmed.count, let found = githubURLRef(trimmed)
            else { return nil }
            ref = found
        } else if trimmed.contains(UInt8(ascii: "#")) {
            guard let hash = trimmed.lastIndex(of: UInt8(ascii: "#")),
                let found = numberRef(trimmed, hash: hash), found.range == 0..<trimmed.count
            else { return nil }
            ref = found.ref
        } else if isShaLike(trimmed), !hasUpper(trimmed), !trimmed.allSatisfy(isDigit) {
            ref = .commit(repo: nil, sha: trimmedString)
        } else {
            return nil
        }
        return GitRefMatch(range: range, ref: ref)
    }

    // MARK: URLs

    private static func lower(_ b: UInt8) -> UInt8 {
        (0x41...0x5A).contains(b) ? b + 0x20 : b
    }

    private static func hasScheme(_ bytes: [UInt8], _ at: Int) -> Bool {
        func starts(_ prefix: String) -> Bool {
            let prefix = Array(prefix.utf8)
            guard bytes.count - at >= prefix.count else { return false }
            return zip(bytes[at..<(at + prefix.count)], prefix).allSatisfy { lower($0) == $1 }
        }
        return starts("https://") || starts("http://")
    }

    /// Byte ranges of the `http(s)://` URLs in `bytes`.
    private static func urlSpans(_ bytes: [UInt8]) -> [Range<Int>] {
        var spans: [Range<Int>] = []
        var at = 0
        while at < bytes.count {
            let boundary = at == 0 || !isAlphanumeric(bytes[at - 1])
            guard boundary, hasScheme(bytes, at) else {
                at += 1
                continue
            }
            let start = at
            while at < bytes.count, isURLByte(bytes[at]) { at += 1 }
            var end = at
            while end > start {
                let last = bytes[end - 1]
                func unbalanced(_ open: UInt8) -> Bool {
                    let span = bytes[start..<end]
                    return span.filter { $0 == last }.count > span.filter { $0 == open }.count
                }
                let trim: Bool
                switch last {
                case UInt8(ascii: "."), UInt8(ascii: ","), UInt8(ascii: ";"), UInt8(ascii: ":"), UInt8(ascii: "!"),
                    UInt8(ascii: "?"), UInt8(ascii: "*"), UInt8(ascii: "_"), UInt8(ascii: "~"):
                    trim = true
                case UInt8(ascii: ")"): trim = unbalanced(UInt8(ascii: "("))
                case UInt8(ascii: "]"): trim = unbalanced(UInt8(ascii: "["))
                case UInt8(ascii: "}"): trim = unbalanced(UInt8(ascii: "{"))
                default: trim = false
                }
                guard trim else { break }
                end -= 1
            }
            spans.append(start..<end)
        }
        return spans
    }

    private static func isURLByte(_ b: UInt8) -> Bool {
        (0x21...0x7E).contains(b) && !"<>\"'`".utf8.contains(b)
    }

    /// The ref a github.com URL names, if it names one.
    private static func githubURLRef(_ url: [UInt8]) -> GitRef? {
        let string = String(decoding: url, as: UTF8.self)
        guard let scheme = string.range(of: "://") else { return nil }
        let rest = string[scheme.upperBound...]
        guard let slash = rest.firstIndex(of: "/") else { return nil }
        let host = rest[..<slash].lowercased()
        guard host == "github.com" || host == "www.github.com" else { return nil }
        let afterHost = rest[rest.index(after: slash)...]
        let path = afterHost.split(omittingEmptySubsequences: false) { $0 == "?" || $0 == "#" }.first ?? ""
        var parts = path.split(separator: "/", omittingEmptySubsequences: false).makeIterator()
        guard let owner = parts.next(), let name = parts.next() else { return nil }
        let repo = "\(owner)/\(name)"
        guard validRepo(repo), let kind = parts.next(), let id = parts.next() else { return nil }
        switch kind {
        case "pull":
            if let next = parts.next(), next == "commits" {
                guard parseNumber(Array(id.utf8)) != nil, let sha = parts.next(), isShaLike(Array(sha.utf8))
                else { return nil }
                return .commit(repo: repo, sha: sha.lowercased())
            }
            return parseNumber(Array(id.utf8)).map { .issue(repo: repo, number: $0) }
        case "issues":
            return parseNumber(Array(id.utf8)).map { .issue(repo: repo, number: $0) }
        case "commit":
            return isShaLike(Array(id.utf8)) ? .commit(repo: repo, sha: id.lowercased()) : nil
        default:
            return nil
        }
    }

    // MARK: Numbers

    private static func parseNumber(_ digits: some Collection<UInt8>) -> UInt64? {
        guard !digits.isEmpty, digits.count <= maxNumberDigits, digits.first != UInt8(ascii: "0"),
            digits.allSatisfy(isDigit)
        else { return nil }
        return UInt64(String(decoding: digits, as: UTF8.self))
    }

    /// `#<n>` or `owner/repo#<n>` with its `#` at `hash`.
    private static func numberRef(_ bytes: [UInt8], hash: Int) -> GitRefMatch? {
        var digitsEnd = hash + 1
        while digitsEnd < bytes.count, isDigit(bytes[digitsEnd]) { digitsEnd += 1 }
        guard let number = parseNumber(bytes[(hash + 1)..<digitsEnd]) else { return nil }
        // The number must not run on into a word: `#12abc`, `#1.5`, `#12-3`.
        if digitsEnd < bytes.count {
            let next = bytes[digitsEnd]
            if isWordByte(next), next != UInt8(ascii: "-") { return nil }
            if next == UInt8(ascii: ".") || next == UInt8(ascii: "-"), digitsEnd + 1 < bytes.count,
                isAlphanumeric(bytes[digitsEnd + 1])
            {
                return nil
            }
        }
        var start = hash
        var repo: String?
        if hash > 0 {
            let before = bytes[hash - 1]
            if isAlphanumeric(before) || before == UInt8(ascii: "_") {
                // Only a repository may run up to the `#`.
                guard let qualified = qualifiedRepo(bytes, hash: hash) else { return nil }
                (start, repo) = (qualified.start, qualified.repo)
            } else if before == UInt8(ascii: ".") || before == UInt8(ascii: "-") {
                if let qualified = qualifiedRepo(bytes, hash: hash) {
                    (start, repo) = (qualified.start, qualified.repo)
                }
            } else if "#&/\\@".utf8.contains(before) {
                return nil
            }
        }
        return GitRefMatch(range: start..<digitsEnd, ref: .issue(repo: repo, number: number))
    }

    /// The `owner/repo` that ends right before `hash`, and where it starts.
    private static func qualifiedRepo(_ bytes: [UInt8], hash: Int) -> (start: Int, repo: String)? {
        let isRepoByte = { (b: UInt8) in isAlphanumeric(b) || ".-_".utf8.contains(b) }
        let isOwnerByte = { (b: UInt8) in isAlphanumeric(b) || b == UInt8(ascii: "-") }
        var at = hash
        while at > 0, isRepoByte(bytes[at - 1]) { at -= 1 }
        guard at > 0, bytes[at - 1] == UInt8(ascii: "/") else { return nil }
        let slash = at - 1
        var start = slash
        while start > 0, isOwnerByte(bytes[start - 1]) { start -= 1 }
        // The owner must not continue a longer word or path.
        if start > 0 {
            let b = bytes[start - 1]
            if isAlphanumeric(b) || "_-./\\@:#".utf8.contains(b) { return nil }
        }
        let repo = String(decoding: bytes[start..<hash], as: UTF8.self)
        guard slash - start <= 39, validRepo(repo) else { return nil }
        return (start, repo)
    }

    // MARK: SHAs

    private static func isDigit(_ b: UInt8) -> Bool { (0x30...0x39).contains(b) }
    private static func isAlpha(_ b: UInt8) -> Bool { (0x41...0x5A).contains(b) || (0x61...0x7A).contains(b) }
    private static func isAlphanumeric(_ b: UInt8) -> Bool { isDigit(b) || isAlpha(b) }
    private static func isHex(_ b: UInt8) -> Bool {
        isDigit(b) || (0x41...0x46).contains(b) || (0x61...0x66).contains(b)
    }

    /// ASCII word bytes: a SHA must be a whole run of them.
    private static func isWordByte(_ b: UInt8) -> Bool {
        isAlphanumeric(b) || b == UInt8(ascii: "_") || b == UInt8(ascii: "-")
    }

    static func isShaLike(_ text: some Collection<UInt8>) -> Bool {
        (minShaLength...maxShaLength).contains(text.count) && text.allSatisfy(isHex)
    }

    static func hasUpper(_ text: some Collection<UInt8>) -> Bool {
        text.contains { (0x41...0x5A).contains($0) }
    }

    /// The prose SHA spanning the word `bytes[start..<end]`, if it is one.
    private static func proseSHA(_ bytes: [UInt8], _ start: Int, _ end: Int) -> String? {
        let word = bytes[start..<end]
        guard isShaLike(word), !hasUpper(word), word.contains(where: isDigit), word.contains(where: isAlpha)
        else { return nil }
        let before: UInt8? = start > 0 ? bytes[start - 1] : nil
        let after: UInt8? = end < bytes.count ? bytes[end] : nil
        // `.`/`:` joining this word to another: a file name, `key:value`…
        func joined(_ sep: UInt8?, _ beyond: UInt8?) -> Bool {
            guard let sep, sep == UInt8(ascii: ".") || sep == UInt8(ascii: ":"), let beyond else { return false }
            return isAlphanumeric(beyond)
        }
        let rejected =
            before.map { "#/\\@$%&".utf8.contains($0) } ?? false
            || after.map { "/\\@".utf8.contains($0) } ?? false
            || joined(before, start >= 2 ? bytes[start - 2] : nil)
            || joined(after, end + 1 < bytes.count ? bytes[end + 1] : nil)
        return rejected ? nil : String(decoding: word, as: UTF8.self)
    }

    // MARK: Repositories

    /// `owner/name` with GitHub's allowed characters.
    static func validRepo(_ repo: String) -> Bool {
        let parts = repo.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2 else { return false }
        let owner = Array(parts[0].utf8)
        let name = Array(parts[1].utf8)
        let ownerOK =
            !owner.isEmpty && owner.first != UInt8(ascii: "-")
            && owner.allSatisfy { isAlphanumeric($0) || $0 == UInt8(ascii: "-") }
        let nameOK =
            !name.isEmpty && name.count <= 100 && parts[1] != "." && parts[1] != ".."
            && name.allSatisfy { isAlphanumeric($0) || ".-_".utf8.contains($0) }
        return ownerOK && nameOK
    }
}

// MARK: - ResolveGitRef replies (git_refs.rs `GitRefResolution`)

enum GitRefState: String, Decodable, Sendable {
    case open, draft, merged, closed
}

struct GitChecksSummary: Decodable, Hashable, Sendable {
    enum State: String, Decodable, Sendable { case success, failure, pending }
    var state: State
    var passed: Int
    var failed: Int
    var pending: Int
    var skipped: Int
}

struct GitCommitPreview: Decodable, Hashable, Sendable {
    var sha: String
    var shortSha: String
    var subject: String
    var author: String
    /// Unix seconds.
    var authorTime: Int64
    var filesChanged: Int
    var insertions: Int
    var deletions: Int
    var url: String?
}

struct GitPullPreview: Decodable, Hashable, Sendable {
    var repo: String
    var number: UInt64
    var title: String
    var state: GitRefState
    var author: String
    var headBranch: String
    var baseBranch: String
    var url: String
    var checks: GitChecksSummary?
    var changedFiles: Int?
    var additions: Int?
    var deletions: Int?
}

struct GitIssuePreview: Decodable, Hashable, Sendable {
    var repo: String
    var number: UInt64
    var title: String
    var state: GitRefState
    var stateReason: String?
    var author: String
    var url: String
}

/// `ResolveGitRef`'s reply. Only commit, pull request and issue make a card;
/// `notFound`/`unavailable` (and any RPC error) leave a plain link to `url`.
enum GitRefResolution: Decodable, Hashable, Sendable {
    case commit(GitCommitPreview)
    case pullRequest(GitPullPreview)
    case issue(GitIssuePreview)
    case notFound(url: String?)
    case unavailable(reason: String, url: String?)

    private enum Keys: String, CodingKey { case kind, url, reason }

    init(from decoder: Decoder) throws {
        let keys = try decoder.container(keyedBy: Keys.self)
        let kind = try keys.decode(String.self, forKey: .kind)
        switch kind {
        case "commit": self = .commit(try GitCommitPreview(from: decoder))
        case "pullRequest": self = .pullRequest(try GitPullPreview(from: decoder))
        case "issue": self = .issue(try GitIssuePreview(from: decoder))
        case "notFound": self = .notFound(url: try keys.decodeIfPresent(String.self, forKey: .url))
        default:
            self = .unavailable(
                reason: (try? keys.decodeIfPresent(String.self, forKey: .reason)) ?? kind,
                url: try? keys.decodeIfPresent(String.self, forKey: .url))
        }
    }

    /// The page to open on a tap, whatever the outcome.
    var url: URL? {
        let raw: String?
        switch self {
        case .commit(let commit): raw = commit.url
        case .pullRequest(let pull): raw = pull.url
        case .issue(let issue): raw = issue.url
        case .notFound(let url), .unavailable(_, let url): raw = url
        }
        return raw.flatMap(URL.init(string:))
    }

    /// Whether it makes a card.
    var hasCard: Bool {
        switch self {
        case .commit, .pullRequest, .issue: true
        case .notFound, .unavailable: false
        }
    }
}

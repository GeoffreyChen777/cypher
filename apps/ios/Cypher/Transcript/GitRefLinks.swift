// Pull request, issue and commit references in a reply (git_refs.rs
// detection, `GitRefs`): in a chat with a git checkout they read as links in
// the text's own ink with a faint dotted underline, and a tap or a long
// press shows what the chat's host says about them (`ResolveGitRef`): a pull
// request's title, state, checks and branches; an issue's title and state;
// a commit's subject, author and size. A ref the host can't resolve shows no
// card: the tap opens its page, when it has one.

import SwiftUI
import UIKit

extension NSAttributedString.Key {
    /// Over a git ref in transcript text: its `GitRefBox`.
    static let cypherGitRef = NSAttributedString.Key("cypher.gitRef")
}

/// An attribute value holding a ref (attributes compare by `isEqual`).
final class GitRefBox: NSObject {
    let ref: GitRef
    init(_ ref: GitRef) { self.ref = ref }
    override func isEqual(_ object: Any?) -> Bool { (object as? GitRefBox)?.ref == ref }
    override var hash: Int { ref.hashValue }
}

/// How a transcript resolves its git refs: on the chat's host, against its
/// checkout. Absent where the chat has no checkout (refs stay plain text).
struct TranscriptGitRefs {
    /// Names the device and chat: resolutions are cached per chat.
    var scope: String
    var resolve: @MainActor (GitRef) async throws -> GitRefResolution
}

extension EnvironmentValues {
    @Entry var transcriptGitRefs: TranscriptGitRefs? = nil
}

/// Resolutions, cached per chat for a few minutes; a failed call counts as
/// `unavailable` (a card never raises an error).
@MainActor
final class GitRefStore {
    static let shared = GitRefStore()
    static let lifetime: TimeInterval = 5 * 60

    private var entries: [String: (task: Task<GitRefResolution, Never>, at: Date)] = [:]

    private static func key(_ ref: GitRef, _ refs: TranscriptGitRefs) -> String {
        "\(refs.scope)\u{1}\(ref)"
    }

    /// The finished resolution, if one is cached and fresh.
    func cached(_ ref: GitRef, refs: TranscriptGitRefs) -> GitRefResolution? {
        guard let entry = entries[Self.key(ref, refs)], Date().timeIntervalSince(entry.at) < Self.lifetime else {
            return nil
        }
        return done[Self.key(ref, refs)]
    }

    private var done: [String: GitRefResolution] = [:]

    func resolve(_ ref: GitRef, refs: TranscriptGitRefs) async -> GitRefResolution {
        let key = Self.key(ref, refs)
        if let entry = entries[key], Date().timeIntervalSince(entry.at) < Self.lifetime {
            return await entry.task.value
        }
        done[key] = nil
        let resolve = refs.resolve
        let task = Task { @MainActor in
            let result: GitRefResolution
            do {
                result = try await resolve(ref)
            } catch {
                result = .unavailable(reason: "error", url: ref.webURL()?.absoluteString)
            }
            self.done[key] = result
            return result
        }
        entries[key] = (task, Date())
        if entries.count > 400 { prune() }
        return await task.value
    }

    private func prune() {
        let now = Date()
        for (key, entry) in entries where now.timeIntervalSince(entry.at) >= Self.lifetime {
            entries[key] = nil
            done[key] = nil
        }
    }
}

@MainActor
enum GitRefText {
    /// A ref's link (the ref itself rides in `.cypherGitRef`).
    static let scheme = "cypher-git"

    /// `text` with its git refs marked: prose outside links, chips, mentions
    /// and formulas; a whole inline code span that is one ref; and a link
    /// whose address is a GitHub pull, issue or commit URL.
    static func decorate(_ text: NSAttributedString) -> NSAttributedString {
        let string = text.string
        let ns = string as NSString
        let full = NSRange(location: 0, length: ns.length)
        var marks: [(NSRange, GitRef)] = []
        var prose: [NSRange] = []
        text.enumerateAttributes(in: full) { attributes, range, _ in
            if attributes[.cypherFileRef] != nil || attributes[.attachment] != nil
                || attributes[.cypherMathSource] != nil || attributes[.cypherMentionWash] != nil
            {
                return
            }
            if let link = attributes[.link] {
                let url = (link as? URL)?.absoluteString ?? (link as? String) ?? ""
                if url.lowercased().hasPrefix("http"), let match = GitRefs.find(url, inCode: true).first {
                    marks.append((range, match.ref))
                }
                return
            }
            // Inline code is read whole, below.
            if attributes[.cypherInlineCode] == nil { prose.append(range) }
        }
        // Merge adjacent prose runs (attribute changes split them) and find.
        for segment in merged(prose) {
            let piece = ns.substring(with: segment)
            for match in GitRefs.find(piece, inCode: false) {
                if let range = nsRange(match.range, in: piece) {
                    marks.append(
                        (NSRange(location: segment.location + range.location, length: range.length), match.ref))
                }
            }
        }
        for span in mergedCode(text, full) {
            let piece = ns.substring(with: span)
            if let match = GitRefs.find(piece, inCode: true).first, let range = nsRange(match.range, in: piece) {
                marks.append((NSRange(location: span.location + range.location, length: range.length), match.ref))
            }
        }
        guard !marks.isEmpty else { return text }
        let result = NSMutableAttributedString(attributedString: text)
        for (range, ref) in marks where ref.isValid {
            let label = ref.label.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
            guard let url = URL(string: "\(GitRefText.scheme):\(label)") else { continue }
            result.addAttributes(
                [
                    .link: url,
                    .cypherGitRef: GitRefBox(ref),
                    .underlineStyle: NSUnderlineStyle.single.rawValue | NSUnderlineStyle.patternDot.rawValue,
                    .underlineColor: UIColor(Theme.textMuted),
                ], range: range)
        }
        return result
    }

    /// Whole inline code spans: maximal runs of `.cypherInlineCode` outside
    /// chips and links (runs split where other attributes change, e.g. the
    /// closing kern).
    private static func mergedCode(_ text: NSAttributedString, _ full: NSRange) -> [NSRange] {
        var spans: [NSRange] = []
        text.enumerateAttribute(.cypherInlineCode, in: full) { value, range, _ in
            guard value != nil, text.attribute(.link, at: range.location, effectiveRange: nil) == nil,
                text.attribute(.cypherFileRef, at: range.location, effectiveRange: nil) == nil
            else { return }
            if let last = spans.last, NSMaxRange(last) == range.location {
                spans[spans.count - 1] = NSUnionRange(last, range)
            } else {
                spans.append(range)
            }
        }
        return spans
    }

    private static func merged(_ ranges: [NSRange]) -> [NSRange] {
        var result: [NSRange] = []
        for range in ranges {
            if let last = result.last, NSMaxRange(last) == range.location {
                result[result.count - 1] = NSUnionRange(last, range)
            } else {
                result.append(range)
            }
        }
        return result
    }

    /// A UTF-8 byte range of `text` as UTF-16.
    private static func nsRange(_ bytes: Range<Int>, in text: String) -> NSRange? {
        let utf8 = text.utf8
        guard let lower = utf8.index(utf8.startIndex, offsetBy: bytes.lowerBound, limitedBy: utf8.endIndex),
            let upper = utf8.index(utf8.startIndex, offsetBy: bytes.upperBound, limitedBy: utf8.endIndex)
        else { return nil }
        return NSRange(lower..<upper, in: text)
    }

    /// Every ref marked in `text`, in order, once each.
    static func refs(in text: NSAttributedString) -> [GitRef] {
        var refs: [GitRef] = []
        text.enumerateAttribute(.cypherGitRef, in: NSRange(location: 0, length: text.length)) { value, _, _ in
            if let box = value as? GitRefBox, !refs.contains(box.ref) { refs.append(box.ref) }
        }
        return refs
    }
}

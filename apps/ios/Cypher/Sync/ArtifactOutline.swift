// Artifact outline — the phone's counterpart of crates/ui/src/transcript/artifacts.rs:
// the chat's named artifacts, newest revision per title. Every card with a
// title opens the newest version, a patch fence (ArtifactPatch.swift) edits
// the version before it, and an open page reloads when a revision lands.
//
// Folded over the whole doc rather than the transcript's window: a patch's
// page is usually older than the few messages a transcript opens on. Settled
// messages are read once, and only those mentioning "artifact" are parsed;
// the streaming tail is read again with each projection.

import Foundation
import Loro
import Observation

/// How a patch fence went: its hunks that took, of all of them, and whether
/// there was a page under its title to patch.
struct PatchOutcome: Hashable, Sendable {
    var applied: Int
    var total: Int
    var foundPage: Bool
}

/// The named pages, folded message by message in chat order.
struct ArtifactPages: Equatable, Sendable {
    struct Page: Hashable, Sendable {
        /// The fence that wrote this version: its message and place there.
        var key: String
        var kind: ArtifactKind
        var title: String
        var code: String
    }

    /// A patch fence by the transcript entry showing it and its code.
    struct PatchKey: Hashable, Sendable {
        var entry: String
        var code: String
    }

    private(set) var latest: [String: Page] = [:]
    private(set) var outcomes: [PatchKey: PatchOutcome] = [:]

    mutating func fold(_ message: ArtifactOutline.Message) {
        for (ix, fence) in message.fences.enumerated() {
            // A streaming fence isn't a revision yet; a settled reply's
            // unclosed one opens like its card does.
            guard fence.closed || message.settled else { continue }
            let key = "\(message.id)#\(ix)"
            guard fence.patch else {
                latest[fence.title] = Page(key: key, kind: fence.kind, title: fence.title, code: fence.code)
                continue
            }
            let outcome: PatchOutcome
            if let base = latest[fence.title] {
                let applied = ArtifactPatch.apply(base: base.code, patch: fence.code)
                outcome = PatchOutcome(applied: applied.applied, total: applied.total, foundPage: true)
                // An update with nothing applied leaves the page where it was.
                if applied.applied > 0 {
                    latest[fence.title] = Page(key: key, kind: base.kind, title: fence.title, code: applied.code)
                }
            } else {
                outcome = PatchOutcome(applied: 0, total: ArtifactPatch.hunks(fence.code).count, foundPage: false)
            }
            outcomes[PatchKey(entry: message.entry, code: fence.code)] = outcome
        }
    }
}

struct ArtifactOutline: Sendable {
    /// One named artifact fence of a message, in document order.
    struct Fence: Hashable, Sendable {
        var kind: ArtifactKind
        var title: String
        var code: String
        var closed: Bool
        var patch: Bool
    }

    /// A doc message as the outline reads it.
    struct Message: Hashable, Sendable {
        var id: String
        /// The transcript entry it shows under (a continuation's root).
        var entry: String
        /// Done streaming: its text won't change.
        var settled: Bool
        var fences: [Fence]
    }

    /// The doc's first messages, all settled, read once.
    private(set) var settled: [Message] = []
    private var settledPages = ArtifactPages()
    /// The settled messages' pages with the tail folded on.
    private(set) var pages = ArtifactPages()

    /// How many of the doc's messages are read for good.
    var upTo: Int { settled.count }

    /// Folds `messages` from `upTo` on: the settled ones for good, the rest
    /// (from the first that is still streaming) as this read's tail.
    mutating func extend(with messages: [Message]) {
        var tail: [Message] = []
        for message in messages {
            if tail.isEmpty && message.settled {
                settled.append(message)
                settledPages.fold(message)
            } else {
                tail.append(message)
            }
        }
        pages = settledPages
        for message in tail { pages.fold(message) }
    }

    // MARK: Reading

    /// `outline` brought up to the end of the doc's messages (started over
    /// when the messages it read were rewound or replaced), or nil if that
    /// means reading more than `limit` new messages.
    nonisolated static func extend(_ outline: ArtifactOutline, of doc: LoroDoc, limit: Int = .max) -> ArtifactOutline? {
        let messages = doc.getList(id: "messages")
        let count = Int(messages.len())
        var outline = outline
        if let last = outline.settled.last,
            count < outline.upTo || id(at: outline.upTo - 1, in: messages) != last.id
        {
            outline = ArtifactOutline()
        }
        guard count - outline.upTo <= limit else { return nil }
        outline.extend(with: (outline.upTo..<count).map { message(at: $0, in: messages) })
        return outline
    }

    /// The outline of a transcript that isn't a doc (a fed store).
    static func of(_ entries: [MessageEntry]) -> ArtifactOutline {
        var outline = ArtifactOutline()
        outline.extend(
            with: entries.map { entry in
                let texts =
                    entry.role == .user
                    ? []
                    : entry.parts.compactMap { part -> String? in
                        if case .text(_, let text, _) = part { return text }
                        return nil
                    }
                return Message(
                    id: entry.id, entry: entry.continuationOf ?? entry.id,
                    settled: entry.status != .streaming, fences: fences(in: texts))
            })
        return outline
    }

    nonisolated private static func id(at index: Int, in messages: LoroList) -> String? {
        messages.get(index: UInt32(index))?.asLoroMap()?.get(key: "id")?.asValue()?.stringValue
    }

    /// Message `index`, reading a reply's text parts and nothing of a
    /// prompt's. One that can't be read holds its place with no fences.
    nonisolated private static func message(at index: Int, in messages: LoroList) -> Message {
        guard let map = messages.get(index: UInt32(index))?.asLoroMap(),
            let id = map.get(key: "id")?.asValue()?.stringValue
        else { return Message(id: "", entry: "", settled: true, fences: []) }
        let field = { (key: String) in map.get(key: key)?.asValue()?.stringValue }
        let settled = field("status") != MessageStatus.streaming.rawValue
        let entry = field("continuationOf") ?? id
        guard field("role") != MessageRole.user.rawValue, let parts = map.get(key: "parts")?.asLoroList()
        else { return Message(id: id, entry: entry, settled: settled, fences: []) }
        let texts = (0..<parts.len()).compactMap { ix -> String? in
            guard let part = parts.get(index: ix)?.asLoroMap(),
                part.get(key: "kind")?.asValue()?.stringValue == "text"
            else { return nil }
            // Text bodies are LoroText containers; the deep value reads them.
            return part.getDeepValue().mapValue?["text"]?.stringValue
        }
        return Message(id: id, entry: entry, settled: settled, fences: fences(in: texts))
    }

    /// The named artifact fences in `texts`, in order. Text that never says
    /// "artifact" holds none and isn't parsed.
    nonisolated static func fences(in texts: [String]) -> [Fence] {
        var fences: [Fence] = []
        func visit(_ block: MDBlock) {
            switch block {
            case .artifact(let kind, let title?, let code, let closed, let patch):
                fences.append(Fence(kind: kind, title: title, code: code, closed: closed, patch: patch))
            case .blockquote(let children):
                children.forEach(visit)
            case .list(_, let items):
                items.forEach { $0.children.forEach(visit) }
            default:
                break
            }
        }
        for text in texts where text.contains("artifact") {
            MarkdownParser.parse(text).forEach { visit($0.block) }
        }
        return fences
    }
}

/// The chat's named pages as its cards and open pages read them, published
/// with each projection.
@MainActor
@Observable
final class ArtifactShelf {
    private(set) var pages = ArtifactPages()

    func update(_ pages: ArtifactPages) {
        if pages != self.pages { self.pages = pages }
    }

    /// What opening `page` shows: a named page's newest version.
    func latest(_ page: ArtifactPage) -> ArtifactPage {
        guard let name = page.name, let newest = pages.latest[name] else { return page }
        var shown = page
        shown.kind = newest.kind
        shown.code = newest.code
        return shown
    }

    /// How the patch fence with `code` in transcript entry `entry` went.
    func outcome(entry: String, code: String) -> PatchOutcome? {
        pages.outcomes[ArtifactPages.PatchKey(entry: entry, code: code)]
    }
}

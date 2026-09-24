// `WatchDocMessages` frames (crates/doc/src/transcript_delta.rs) — the
// transcript of a chat the phone reads from its host directly instead of a
// synced doc: a side chat, which has no room until it's promoted. The first
// frame is a `reset`; later ones carry only changed entries and text tails.
// Frames apply to the raw JSON entries, decoded after, so an entry this
// build doesn't understand still holds its place for later anchors.

import Foundation
import Loro

struct TranscriptFeed {
    /// The joined `SessionMessageEntry` JSON objects, in transcript order.
    private(set) var entries: [[String: Any]] = []

    struct Desync: Error, Equatable {
        var reason: String
    }

    /// transcript_delta.rs `apply_transcript_frame`, rule for rule. On a
    /// throw the copy is unreliable: resubscribe for a fresh reset.
    mutating func apply(_ frame: [String: Any]) throws {
        if let reset = frame["reset"] as? [[String: Any]] {
            entries = reset
            return
        }
        guard let count = (frame["count"] as? NSNumber)?.intValue else {
            throw Desync(reason: "not a transcript frame")
        }
        if let remove = frame["remove"] as? [String], !remove.isEmpty {
            let gone = Set(remove)
            entries.removeAll { gone.contains(Self.id(of: $0)) }
        }
        for upsert in frame["upsert"] as? [[String: Any]] ?? [] {
            guard let entry = upsert["entry"] as? [String: Any] else { throw Desync(reason: "upsert without entry") }
            let id = Self.id(of: entry)
            entries.removeAll { Self.id(of: $0) == id }
            var at = 0
            if let anchor = upsert["after"] as? String {
                guard let ix = entries.firstIndex(where: { Self.id(of: $0) == anchor }) else {
                    throw Desync(reason: "missing anchor \(anchor)")
                }
                at = ix + 1
            }
            entries.insert(entry, at: at)
        }
        for append in frame["append"] as? [[String: Any]] ?? [] {
            guard let entryId = append["entry"] as? String, let partId = append["part"] as? String,
                  let text = append["text"] as? String,
                  let len = (append["len"] as? NSNumber)?.intValue else {
                throw Desync(reason: "malformed append")
            }
            guard let ix = entries.firstIndex(where: { Self.id(of: $0) == entryId }) else {
                throw Desync(reason: "missing append entry \(entryId)")
            }
            var parts = entries[ix]["parts"] as? [[String: Any]] ?? []
            guard let p = parts.firstIndex(where: { $0["kind"] as? String == "text" && $0["id"] as? String == partId }) else {
                throw Desync(reason: "missing append part \(partId)")
            }
            let tail = (parts[p]["text"] as? String ?? "") + text
            // `len` counts UTF-8 bytes (Rust String::len), not characters.
            guard tail.utf8.count == len else {
                throw Desync(reason: "append length mismatch on \(entryId)#\(partId)")
            }
            parts[p]["text"] = tail
            entries[ix]["parts"] = parts
        }
        guard entries.count == count else {
            throw Desync(reason: "count mismatch: have \(entries.count), expected \(count)")
        }
    }

    /// The entries for the transcript view (undecodable ones skipped).
    var messages: [MessageEntry] {
        entries.compactMap { SessionStore.entryFrom(LoroValue.fromJSON($0)) }
    }

    private static func id(of entry: [String: Any]) -> String {
        entry["id"] as? String ?? ""
    }
}

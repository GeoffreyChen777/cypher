// Artifact patches — a port of crates/ui/src/markdown/artifact/patch.rs; a
// mirror listed in protocol/README.md, pinned by
// protocol/vectors/artifact-patch-v1.json.
//
// A fence tagged `artifact-patch` (```html artifact-patch title="…"```) edits
// a named page's latest version in place. Its body is SEARCH/REPLACE hunks,
// applied in order; a hunk applies where its SEARCH matches exactly one
// place, verbatim first, then line by line comparing each line trimmed. A
// hunk that matches nowhere or in several places is skipped.
//
// Matching is on UTF-8 bytes, as Rust's is: Swift's String comparison would
// match canonically equivalent text the desktop doesn't.

import Foundation

enum ArtifactPatch {
    struct Hunk: Equatable {
        let search: [UInt8]
        let replace: [UInt8]
    }

    struct Applied: Equatable {
        let code: String
        let applied: Int
        let total: Int
    }

    /// The patch's complete hunks; one still streaming (no REPLACE marker
    /// yet) is left out.
    static func hunks(_ patch: String) -> [Hunk] {
        enum State {
            case outside
            case search([ArraySlice<UInt8>])
            case replace([ArraySlice<UInt8>], [ArraySlice<UInt8>])
        }
        var hunks: [Hunk] = []
        var state = State.outside
        for line in lines(Array(patch.utf8)) {
            let marker = trim(line)
            switch state {
            case .outside:
                if isSearch(marker) { state = .search([]) }
            case .search(var search):
                if isDivider(marker) {
                    state = .replace(search, [])
                } else {
                    search.append(line)
                    state = .search(search)
                }
            case .replace(let search, var replace):
                if isReplace(marker) {
                    hunks.append(Hunk(search: joined(search), replace: joined(replace)))
                    state = .outside
                } else {
                    replace.append(line)
                    state = .replace(search, replace)
                }
            }
        }
        return hunks
    }

    /// `base` with `patch`'s hunks applied in order, and how many took.
    static func apply(base: String, patch: String) -> Applied {
        let hunks = hunks(patch)
        var code = Array(base.utf8)
        var applied = 0
        for hunk in hunks {
            if let next = apply(hunk, to: code) {
                code = next
                applied += 1
            }
        }
        return Applied(code: String(decoding: code, as: UTF8.self), applied: applied, total: hunks.count)
    }

    private static func apply(_ hunk: Hunk, to code: [UInt8]) -> [UInt8]? {
        guard !trim(hunk.search[...]).isEmpty else { return nil }
        if let at = find(hunk.search, in: code, from: 0) {
            let end = at + hunk.search.count
            // Occurrences count without overlap, like Rust's `match_indices`.
            guard find(hunk.search, in: code, from: end) == nil else { return nil }
            return Array(code[..<at]) + hunk.replace + Array(code[end...])
        }
        return applyByLines(hunk, to: code)
    }

    /// The fallback: SEARCH's lines against the page's, each compared
    /// trimmed, matching one run of whole lines.
    private static func applyByLines(_ hunk: Hunk, to code: [UInt8]) -> [UInt8]? {
        let search = lines(hunk.search).map(trim)
        let lines = inclusiveLines(code)
        guard search.count <= lines.count else { return nil }
        var found: Int?
        for start in 0...(lines.count - search.count) {
            let matches = search.indices.allSatisfy { trim(lines[start + $0]) == search[$0] }
            if matches {
                if found != nil { return nil }
                found = start
            }
        }
        guard let start = found else { return nil }
        let end = start + search.count
        var out = lines[..<start].flatMap { $0 }
        out += hunk.replace
        if lines[end - 1].last == newline { out.append(newline) }
        out += lines[end...].flatMap { $0 }
        return out
    }

    // MARK: Rust's string primitives, on bytes

    private static let newline = UInt8(ascii: "\n")

    /// `str::lines`: split at `\n` (a `\r` before it dropped), with no empty
    /// last line after a final newline.
    private static func lines(_ bytes: [UInt8]) -> [ArraySlice<UInt8>] {
        var out: [ArraySlice<UInt8>] = []
        var start = 0
        for ix in bytes.indices where bytes[ix] == newline {
            var line = bytes[start..<ix]
            if line.last == UInt8(ascii: "\r") { line = line.dropLast() }
            out.append(line)
            start = ix + 1
        }
        if start < bytes.count { out.append(bytes[start...]) }
        return out
    }

    /// `str::split_inclusive('\n')`: lines with their newlines.
    private static func inclusiveLines(_ bytes: [UInt8]) -> [ArraySlice<UInt8>] {
        var out: [ArraySlice<UInt8>] = []
        var start = 0
        for ix in bytes.indices where bytes[ix] == newline {
            out.append(bytes[start...ix])
            start = ix + 1
        }
        if start < bytes.count { out.append(bytes[start...]) }
        return out
    }

    /// `str::trim`: Unicode White_Space off both ends.
    private static func trim(_ bytes: ArraySlice<UInt8>) -> [UInt8] {
        let scalars = String(decoding: bytes, as: UTF8.self).unicodeScalars
        guard let first = scalars.firstIndex(where: { !$0.properties.isWhitespace }),
            let last = scalars.lastIndex(where: { !$0.properties.isWhitespace })
        else { return [] }
        return Array(String(scalars[first...last]).utf8)
    }

    private static func joined(_ lines: [ArraySlice<UInt8>]) -> [UInt8] {
        var out: [UInt8] = []
        for (ix, line) in lines.enumerated() {
            if ix > 0 { out.append(newline) }
            out += line
        }
        return out
    }

    private static func find(_ needle: [UInt8], in haystack: [UInt8], from start: Int) -> Int? {
        guard !needle.isEmpty, haystack.count - start >= needle.count else { return nil }
        let first = needle[0]
        var ix = start
        while ix <= haystack.count - needle.count {
            if haystack[ix] == first, haystack[ix..<(ix + needle.count)].elementsEqual(needle) {
                return ix
            }
            ix += 1
        }
        return nil
    }

    private static func isSearch(_ marker: [UInt8]) -> Bool {
        marked(marker, by: "<", as: "SEARCH")
    }

    private static func isDivider(_ marker: [UInt8]) -> Bool {
        marker.count >= 7 && marker.allSatisfy { $0 == UInt8(ascii: "=") }
    }

    private static func isReplace(_ marker: [UInt8]) -> Bool {
        marked(marker, by: ">", as: "REPLACE")
    }

    /// Seven or more `character`s, then `word` (whitespace around it allowed).
    private static func marked(_ marker: [UInt8], by character: Character, as word: String) -> Bool {
        let byte = UInt8(ascii: character.unicodeScalars.first!)
        guard marker.count >= 7, marker.prefix(7).allSatisfy({ $0 == byte }) else { return false }
        return trim(marker.drop { $0 == byte }) == Array(word.utf8)
    }
}

// Artifact fences — a port of the fence rules in crates/ui/src/markdown/parser.rs
// (`artifact_info`, `info_words`); a mirror listed in
// protocol/README.md.
//
// A fence whose info string carries a bare `artifact` word is a page the model
// wrote to be viewed: ```html artifact title="Sorting"```. Without a language
// it is HTML; a language that doesn't render (`python artifact`) stays an
// ordinary code block. A bare `artifact-patch` marks a patch to the page its
// `title=` names (ArtifactPatch.swift), which it needs.

import Foundation

/// What an artifact renders as.
enum ArtifactKind: Hashable {
    case html, svg

    init?(language: String) {
        switch language.lowercased() {
        case "html", "htm", "xhtml": self = .html
        case "svg": self = .svg
        default: return nil
        }
    }

    var label: String {
        switch self {
        case .html: "HTML"
        case .svg: "SVG"
        }
    }
}

enum ArtifactFence {
    /// An info string's kind, title and whether it is a patch; nil unless a
    /// bare `artifact` (or `artifact-patch`) word marks it.
    static func info(_ info: String) -> (ArtifactKind, String?, Bool)? {
        let words = self.words(info)
        let bare = words.filter { $0.value == nil }.map(\.key)
        let marker = { (word: String) in ["artifact", "artifact-patch"].contains(word.lowercased()) }
        guard bare.contains(where: marker) else { return nil }
        let patch = bare.contains { $0.lowercased() == "artifact-patch" }
        let kind: ArtifactKind
        if let language = bare.first(where: { !marker($0) }) {
            guard let named = ArtifactKind(language: language) else { return nil }
            kind = named
        } else {
            kind = .html
        }
        let title = words.first { $0.value != nil && $0.key.lowercased() == "title" }?
            .value.map { $0.trimmingCharacters(in: .whitespaces) }
            .flatMap { $0.isEmpty ? nil : $0 }
        // A patch to no page in particular is only code.
        if patch && title == nil { return nil }
        return (kind, title, patch)
    }

    /// An info string's words: bare ones (`html`) and `key=value` pairs whose
    /// value may be quoted (`title="Two words"`, `title='x'`).
    static func words(_ info: String) -> [(key: String, value: String?)] {
        var words: [(key: String, value: String?)] = []
        var chars = Substring(info)
        while true {
            chars = chars.drop(while: \.isWhitespace)
            guard !chars.isEmpty else { return words }
            let key = chars.prefix { !$0.isWhitespace && $0 != "=" }
            chars = chars.dropFirst(key.count)
            guard chars.first == "=" else {
                words.append((String(key), nil))
                continue
            }
            chars = chars.dropFirst()
            if let quote = chars.first, quote == "\"" || quote == "'" {
                chars = chars.dropFirst()
                let value = chars.prefix { $0 != quote }
                chars = chars.dropFirst(value.count).dropFirst()
                words.append((String(key), String(value)))
            } else {
                let value = chars.prefix { !$0.isWhitespace }
                chars = chars.dropFirst(value.count)
                words.append((String(key), String(value)))
            }
        }
    }

    /// The text of a page's `<title>` element, entities decoded.
    static func pageTitle(_ html: String) -> String? {
        guard let open = html.range(of: "<title", options: .caseInsensitive),
            let start = html[open.upperBound...].firstIndex(of: ">")
        else { return nil }
        let begin = html.index(after: start)
        guard let end = html.range(of: "</title", options: .caseInsensitive, range: begin..<html.endIndex)
        else { return nil }
        let title = html[begin..<end.lowerBound].split(whereSeparator: \.isWhitespace).joined(separator: " ")
            .replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&#39;", with: "'")
            .replacingOccurrences(of: "&amp;", with: "&")
        return title.isEmpty ? nil : title
    }

    /// A card's title: the fence's, else the page's own `<title>`, else a
    /// generic name.
    static func displayTitle(kind: ArtifactKind, title: String?, code: String) -> String {
        if let title { return title }
        if let page = pageTitle(code) { return page }
        return kind == .html ? "HTML page" : "SVG image"
    }
}

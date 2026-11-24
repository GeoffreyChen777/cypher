// Whether a message is a slash command — the desktop's
// crates/ui/src/slash_command.rs `slash_command` (a mirror in
// protocol/README.md); both run protocol/vectors/slash-commands-v1.json.
// A message runs as a command only when its first token is `/name` and the
// chat's host offers `name`: pi hands anything else to the model as text.
// Until the host's list has loaded (or when it lists none) a strict shape
// rule stands in, so a path, a URL or a lone `/` is never a command.

import Foundation

enum SlashCommandText {
    /// The leading `/command` token of `text` when the message runs as a
    /// slash command, else nil. `known`: the host's command names for the
    /// chat's harness (nil: not loaded, or no chat); empty counts as none.
    static func command(in text: String, known: Set<String>? = nil) -> String? {
        let token = text.drop(while: \.isWhitespace).prefix(while: { !$0.isWhitespace })
        guard token.hasPrefix("/") else { return nil }
        let name = String(token.dropFirst())
        if let known, !known.isEmpty {
            return known.contains(name) ? String(token) : nil
        }
        return shaped(name) ? String(token) : nil
    }

    static func isCommand(_ text: String, known: Set<String>? = nil) -> Bool {
        command(in: text, known: known) != nil
    }

    /// The names of a host's command list, for `known`.
    static func names(_ commands: [SlashCommand]) -> Set<String> {
        Set(commands.map(\.name))
    }

    /// An ASCII letter, then ASCII letters, digits, `_`, `:` or `-`.
    private static func shaped(_ name: String) -> Bool {
        var scalars = name.unicodeScalars.makeIterator()
        guard let first = scalars.next(), isASCIILetter(first) else { return false }
        while let scalar = scalars.next() {
            guard isASCIILetter(scalar) || ("0"..."9").contains(scalar) || "_:-".unicodeScalars.contains(scalar)
            else { return false }
        }
        return true
    }

    private static func isASCIILetter(_ scalar: Unicode.Scalar) -> Bool {
        ("a"..."z").contains(scalar) || ("A"..."Z").contains(scalar)
    }
}

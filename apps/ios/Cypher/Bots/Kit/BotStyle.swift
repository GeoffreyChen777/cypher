// A workspace's look from its stored keys (crates/ui/src/appearance/
// bot_style.rs, space_style.rs): a robot face, a symbol or the name's
// initial, tinted with one of the project colours. Faces are the desktop's
// own drawings (BotFaces.swift); symbols map to the nearest SF Symbol. An
// unknown key falls back like the desktop's: the initial, on neutral.

import SwiftUI
import UIKit

enum BotStyle {
    /// The project colours: (key, OKLCH hue). Lightness and chroma are
    /// fixed so every swatch reads at the same weight (space_style.rs).
    static let colors: [(key: String, hue: Double)] = [
        ("red", 25), ("orange", 55), ("amber", 80), ("green", 145), ("teal", 185),
        ("blue", 250), ("indigo", 277), ("purple", 310), ("pink", 350),
    ]

    /// The tint for a colour key; nil for none or an unknown key.
    static func color(_ key: String?) -> Color? {
        guard let hue = colors.first(where: { $0.key == key })?.hue else { return nil }
        return Theme.adaptive(light: oklch(0.58, 0.15, hue), dark: oklch(0.72, 0.15, hue))
    }

    /// The picker's symbols in the desktop's order (`BOT_SYMBOLS`), as SF
    /// Symbols.
    static let symbols: [(key: String, symbol: String)] = [
        ("briefcase", "briefcase"), ("chart", "chart.line.uptrend.xyaxis"), ("dollar", "dollarsign"),
        ("cart", "cart"), ("buildings", "building.2"), ("calendar", "calendar"), ("mail", "envelope"),
        ("team", "person.3"), ("code", "chevron.left.forwardslash.chevron.right"), ("terminal", "apple.terminal"),
        ("server", "server.rack"), ("database", "cylinder.split.1x2"), ("bug", "ladybug"),
        ("rocket", "paperplane"), ("magic", "wand.and.stars"), ("bolt", "bolt"), ("lightbulb", "lightbulb"),
        ("palette", "paintpalette"), ("camera", "camera"), ("music", "music.note"), ("book", "book"),
        ("academic", "graduationcap"), ("flask", "flask"), ("health", "cross.case"), ("checklist", "checklist"),
        ("chat", "bubble.left"), ("home", "house"), ("heart", "heart"), ("leaf", "leaf"), ("fire", "flame"),
        ("coffee", "cup.and.saucer"), ("gamepad", "gamecontroller"), ("place", "mappin.and.ellipse"),
        ("plane", "airplane"), ("target", "target"), ("planet", "globe.americas"), ("sun", "sun.max"),
        ("star", "star"),
    ]

    /// Earlier picker keys, still drawn when a row carries one.
    private static let retired: [(key: String, symbol: String)] = [
        ("users", "person.2"), ("pulse", "waveform.path.ecg"), ("flag", "flag"),
        ("hierarchy", "list.bullet.indent"), ("branch", "arrow.triangle.branch"), ("globe", "globe"),
        ("cloud", "cloud"), ("key", "key"), ("widget", "square.grid.2x2"),
    ]

    /// What an icon key draws.
    enum Glyph: Equatable {
        case face(BotFace)
        case symbol(String)
        /// The name's initial.
        case initial
    }

    static func glyph(_ key: String?) -> Glyph {
        guard let key else { return .initial }
        if let face = BotFace(rawValue: key) { return .face(face) }
        if let symbol = (symbols + retired).first(where: { $0.key == key })?.symbol,
            UIImage(systemName: symbol) != nil
        {
            return .symbol(symbol)
        }
        return .initial
    }

    /// The avatar's letter: the name's first letter or digit, uppercased.
    static func initial(_ name: String) -> String {
        name.first { $0.isLetter || $0.isNumber }.map { String($0).uppercased() } ?? "·"
    }

    /// A shortcut's icon key (`BotShortcut.icons`) as an SF Symbol; absent
    /// or unknown keys get the bolt.
    static func shortcutSymbol(_ key: String?) -> String {
        switch key {
        case "star": "star"
        case "rocket": "paperplane"
        case "play": "play"
        case "chat": "bubble.left"
        case "checklist": "checklist"
        case "code": "chevron.left.forwardslash.chevron.right"
        case "terminal": "apple.terminal"
        case "bug": "ladybug"
        case "test-tube": "flask"
        case "document": "doc.text"
        case "global": "globe"
        case "calendar": "calendar"
        case "cup-hot": "cup.and.saucer"
        case "magic-stick": "wand.and.stars"
        case "flag": "flag"
        default: "bolt"
        }
    }
}

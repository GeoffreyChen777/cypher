// Memory's words and rules, as the desktop says them (crates/ui/src/shell/
// bots/memory.rs, crates/ui/src/settings/memory.rs): file names and paths,
// sizes and costs, a change in a line, whether a pass waits or a change can
// be undone, and which stores Settings › Memory lists, in its order.

import Foundation

enum MemoryFormat {
    /// Changes a store's page lists, newest first.
    static let changesShown = 20

    /// A path's last part: `/decisions/rounding.md` reads `rounding.md`.
    static func fileName(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? path
    }

    /// A path as a row shows it: without the leading slash.
    static func displayPath(_ path: String) -> String {
        path.hasPrefix("/") ? String(path.dropFirst()) : path
    }

    /// What a name typed for a file is as a store path: `testing`,
    /// `/testing.md` and `testing.MD` all read `/testing.md`; folders are
    /// fine (`decisions/rounding`). nil when it can't be one
    /// (`validMemoryPath`: no empty, dot or hidden parts, within 160 bytes).
    static func path(typed: String) -> String? {
        var name = Substring(typed.trimmingCharacters(in: .whitespacesAndNewlines))
        while name.hasPrefix("/") { name = name.dropFirst() }
        guard !name.isEmpty else { return nil }
        let stem = name.lowercased().hasSuffix(".md") ? name.dropLast(3) : name
        let path = "/\(stem).md"
        return validMemoryPath(path) ? path : nil
    }

    /// "820 B", "1.2 KB", "64 KB".
    static func size(_ bytes: Int) -> String {
        switch bytes {
        case ..<1024: return "\(bytes) B"
        case ..<(10 * 1024): return String(format: "%.1f KB", Double(bytes) / 1024)
        default: return "\(bytes / 1024) KB"
        }
    }

    /// "820 tokens", "12.4k tokens", "1.2M tokens".
    static func tokens(_ tokens: Int64) -> String {
        switch tokens {
        case ..<1000: return "\(tokens) tokens"
        case ..<1_000_000: return String(format: "%.1fk tokens", Double(tokens) / 1000)
        default: return String(format: "%.1fM tokens", Double(tokens) / 1_000_000)
        }
    }

    static func count(_ n: Int, _ one: String, _ many: String) -> String {
        "\(n) \(n == 1 ? one : many)"
    }

    /// What a pass changed and cost: "3 changes · 12.4k tokens".
    static func passCost(_ pass: MemoryPass) -> String? {
        let parts = [
            pass.changes > 0 ? count(pass.changes, "change", "changes") : nil,
            pass.tokens > 0 ? tokens(pass.tokens) : nil,
        ].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// "03:00".
    static func hour(_ hour: Int) -> String {
        String(format: "%02d:00", hour)
    }

    /// A pass runs, or Run now asked for one its host has not started yet.
    static func passWaiting(_ store: MemoryStore) -> Bool {
        if store.lastPass?.isRunning == true { return true }
        guard let asked = store.passRequestedAt else { return false }
        return store.lastPass.map { $0.at < asked } ?? true
    }

    /// Whether a change can still be put back: it kept the version before
    /// it, is not undone, and is not itself an undo.
    static func undoable(_ change: MemoryChange) -> Bool {
        if case .undo = change.action { return false }
        return change.undo != nil && !change.undone
    }

    /// A change in a line: its own summary, else what it did, on which file.
    static func title(_ change: MemoryChange) -> String {
        let file = fileName(change.path)
        let summary = change.summary.split(whereSeparator: \.isNewline).joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
        if !summary.isEmpty { return "\(file) · \(summary)" }
        switch change.action {
        case .create: return "Created \(file)"
        case .edit: return "Edited \(file)"
        case .delete: return "Deleted \(file)"
        case .rename(let from): return "Renamed \(fileName(from)) to \(file)"
        case .undo: return "Put back \(file)"
        }
    }

    /// The SF Symbol for what a change did.
    static func symbol(_ action: MemoryAction) -> String {
        switch action {
        case .create: "doc.badge.plus"
        case .edit: "pencil"
        case .delete: "trash"
        case .rename: "arrow.right"
        case .undo: "arrow.uturn.backward"
        }
    }

    /// "You · 2h ago": who last changed a file, and when.
    static func byLine(_ file: MemoryFile, now: Int64 = nowMs()) -> String {
        "\(file.author.label) · \(BotTime.ago(max(file.updatedAt, file.createdAt), now: now))"
    }

    /// A pass's outcome in words and tone.
    static func outcome(_ outcome: PassOutcome) -> BotStatusLabel {
        switch outcome {
        case .running: BotStatusLabel("Running", .active)
        case .done: BotStatusLabel("Done", .done)
        case .skipped: BotStatusLabel("Skipped", .muted)
        case .failed: BotStatusLabel("Failed", .failed)
        case .undone: BotStatusLabel("Undone", .muted)
        }
    }

    /// The quiet line under an empty store.
    static func emptyHint(_ scope: MemoryScope) -> String {
        switch scope {
        case .you:
            "Agents keep what holds across your projects here, like how you like your answers. Every session and workspace reads it."
        case .project:
            "Sessions in this project keep what they learn here: its commands, pitfalls and decisions."
        case .workspace:
            "The workspace keeps what it learns here: who owns what, standing decisions, what to watch."
        case .role:
            "This agent keeps what it learns here."
        }
    }

    /// Who reads a store, under its title.
    static func readers(_ scope: MemoryScope) -> String {
        switch scope {
        case .you: "Every session and workspace reads it."
        case .project:
            "What sessions in this project learned. Every one of them reads it, and so do the workspaces that read the project."
        case .workspace:
            "What this workspace learned coordinating your work. Its chat and sessions read it; your other sessions don't."
        case .role: "This agent's own memory."
        }
    }
}

/// A store on Settings › Memory's lists, by the name of what owns it.
struct ListedMemoryStore: Identifiable, Equatable {
    var name: String
    var store: MemoryStore

    var id: String { store.id }
}

/// The stores Settings › Memory lists (settings/memory.rs
/// `listed_stores`): yours, then each project's, then each workspace's, by
/// name. A store whose project or workspace is gone is left out, as are
/// subagent profiles' stores.
struct MemoryListing: Equatable {
    var you: MemoryStore?
    var projects: [ListedMemoryStore] = []
    var workspaces: [ListedMemoryStore] = []

    init(stores: [MemoryStore], spaces: [Space], bots: [Bot]) {
        for store in stores {
            switch store.scope {
            case .you:
                you = store
            case .project(let spaceId):
                if let space = spaces.first(where: { $0.id == spaceId }) {
                    projects.append(ListedMemoryStore(name: space.displayName, store: store))
                }
            case .workspace(let botId):
                if let bot = bots.first(where: { $0.id == botId }) {
                    workspaces.append(ListedMemoryStore(name: bot.name, store: store))
                }
            case .role:
                break
            }
        }
        let byName = { (a: ListedMemoryStore, b: ListedMemoryStore) in
            (a.name.lowercased(), a.store.id) < (b.name.lowercased(), b.store.id)
        }
        projects.sort(by: byName)
        workspaces.sort(by: byName)
    }
}

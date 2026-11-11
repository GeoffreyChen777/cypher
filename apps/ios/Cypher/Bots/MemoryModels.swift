// Memory (docs/design/memory.md, crates/proto/src/memory.rs): what agents
// learn and keep between sessions, as small trees of markdown files. A store
// is one synced doc; the registry holds one `memoryStores` row per store so
// every device can list stores and show counts without loading the doc. The
// files, pending writes and change log arrive as one `MemoryView` frame
// (`WatchMemory`); edits go through `EditMemory`.

import Foundation

/// Whose memory a store is (tag `kind`).
enum MemoryScope: Hashable, Sendable {
    /// The user's own memory, read by every session and workspace.
    case you
    /// One project's memory, shared by every session in it.
    case project(spaceId: String)
    /// A workspace's own memory.
    case workspace(botId: String)
    /// One subagent profile's memory: the user's (no space) or one project's.
    case role(agent: String, spaceId: String?)

    /// The root this store has in a run's memory paths.
    var root: String {
        switch self {
        case .you: "/you"
        case .project: "/project"
        case .workspace: "/workspace"
        case .role: "/role"
        }
    }
}

extension MemoryScope: Codable {
    private enum CodingKeys: String, CodingKey { case kind, spaceId, botId, agent }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "you": self = .you
        case "project": self = .project(spaceId: try c.decode(String.self, forKey: .spaceId))
        case "workspace": self = .workspace(botId: try c.decode(String.self, forKey: .botId))
        case "role":
            self = .role(
                agent: try c.decode(String.self, forKey: .agent),
                spaceId: try c.decodeIfPresent(String.self, forKey: .spaceId))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown scope \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .you:
            try c.encode("you", forKey: .kind)
        case .project(let spaceId):
            try c.encode("project", forKey: .kind)
            try c.encode(spaceId, forKey: .spaceId)
        case .workspace(let botId):
            try c.encode("workspace", forKey: .kind)
            try c.encode(botId, forKey: .botId)
        case .role(let agent, let spaceId):
            try c.encode("role", forKey: .kind)
            try c.encode(agent, forKey: .agent)
            try c.encodeIfPresent(spaceId, forKey: .spaceId)
        }
    }
}

/// When a store's consolidation pass runs: nightly by default, at 03:00 on
/// the host.
struct Consolidation: Hashable, Sendable {
    var enabled = true
    /// The hour (0–23, the host's local time) it starts.
    var hour = 3
}

extension Consolidation: Codable {
    private enum CodingKeys: String, CodingKey { case enabled, hour }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try c.decode(.enabled, or: true)
        hour = try c.decode(.hour, or: 3)
    }
}

enum PassOutcome: String, Codable, Hashable, Sendable {
    case running, done
    /// Nothing new since the last pass: no run.
    case skipped
    case failed
    /// Undone by the owner.
    case undone
}

/// One consolidation pass. Every field defaults (`#[serde(default)]`).
struct MemoryPass: Hashable, Sendable {
    var id = ""
    var at: Int64 = 0
    var endedAt: Int64?
    var outcome: PassOutcome = .running
    /// The run's short summary, or why it failed.
    var summary: String?
    /// Files it changed.
    var changes = 0
    /// Tokens the run used, when known.
    var tokens: Int64 = 0
    /// The hidden chat it ran in.
    var chatId: String?
    /// Sessions and threads it read since the last pass.
    var sources = 0
    /// Asked for with "Run now".
    var manual = false
    /// The store's version before the pass: "Undo" restores it.
    var undo: String?

    var isRunning: Bool { outcome == .running }
}

extension MemoryPass: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, at, endedAt, outcome, summary, changes, tokens, chatId, sources, manual, undo
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(.id, or: "")
        at = try c.decode(.at, or: 0)
        endedAt = try c.decodeIfPresent(Int64.self, forKey: .endedAt)
        outcome = try c.decode(.outcome, or: .running)
        summary = try c.decodeIfPresent(String.self, forKey: .summary)
        changes = try c.decode(.changes, or: 0)
        tokens = try c.decode(.tokens, or: 0)
        chatId = try c.decodeIfPresent(String.self, forKey: .chatId)
        sources = try c.decode(.sources, or: 0)
        manual = try c.decode(.manual, or: false)
        undo = try c.decodeIfPresent(String.self, forKey: .undo)
    }
}

/// One month's consolidation cost.
struct MonthUsage: Hashable, Sendable {
    /// `YYYY-MM`.
    var month = ""
    var tokens: Int64 = 0
    var passes = 0
}

extension MonthUsage: Codable {
    private enum CodingKeys: String, CodingKey { case month, tokens, passes }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        month = try c.decode(.month, or: "")
        tokens = try c.decode(.tokens, or: 0)
        passes = try c.decode(.passes, or: 0)
    }
}

/// A held write as a chat card shows it.
struct HeldWrite: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var chatId: String
    /// The `memory` tool call that made it: where the card is drawn.
    var toolCallId: String?
    var path: String
    /// The file as the write would leave it, clipped (nil: deleted).
    var preview: String?
    var reason: String
    var at: Int64
}

/// What the owner decided about one held write.
struct HeldDecision: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var toolCallId: String?
    var saved: Bool
    var at: Int64
}

/// A store's registry row (kind `memoryStores`): what devices know without
/// loading its doc.
struct MemoryStore: Hashable, Identifiable, Sendable {
    /// The doc id.
    var id: String
    var scope: MemoryScope
    /// The device that made the store: where a user-level store's pass runs.
    /// Project and workspace stores follow their project's or workspace's
    /// host instead.
    var hostDeviceId = ""
    var files = 0
    var bytes: Int64 = 0
    /// Writes waiting for the owner's review.
    var pending = 0
    /// The start of `MEMORY.md`, for cards.
    var preview = ""
    var createdAt: Int64 = 0
    var updatedAt: Int64 = 0
    var consolidation = Consolidation()
    var lastPass: MemoryPass?
    var month: MonthUsage?
    /// A pass was asked for ("Run now"); the host runs it.
    var passRequestedAt: Int64?
    /// The newest held writes, for the cards drawn at their tool calls.
    var held: [HeldWrite] = []
    /// Recent decisions on held writes, newest last.
    var decided: [HeldDecision] = []

    /// The held write a `memory` tool call made, still waiting.
    func heldForCall(_ toolCallId: String) -> HeldWrite? {
        held.first { $0.toolCallId == toolCallId }
    }

    /// The decision on the write a `memory` tool call made.
    func decidedForCall(_ toolCallId: String) -> HeldDecision? {
        decided.last { $0.toolCallId == toolCallId }
    }
}

extension MemoryStore: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, scope, hostDeviceId, files, bytes, pending, preview, createdAt, updatedAt
        case consolidation, lastPass, month, passRequestedAt, held, decided
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        scope = try c.decode(MemoryScope.self, forKey: .scope)
        hostDeviceId = try c.decode(.hostDeviceId, or: "")
        files = try c.decode(.files, or: 0)
        bytes = try c.decode(.bytes, or: 0)
        pending = try c.decode(.pending, or: 0)
        preview = try c.decode(.preview, or: "")
        createdAt = try c.decode(.createdAt, or: 0)
        updatedAt = try c.decode(.updatedAt, or: 0)
        consolidation = try c.decode(.consolidation, or: Consolidation())
        lastPass = try c.decodeIfPresent(MemoryPass.self, forKey: .lastPass)
        month = try c.decodeIfPresent(MonthUsage.self, forKey: .month)
        passRequestedAt = try c.decodeIfPresent(Int64.self, forKey: .passRequestedAt)
        held = try c.decode(.held, or: [])
        decided = try c.decode(.decided, or: [])
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(scope, forKey: .scope)
        try c.encode(hostDeviceId, forKey: .hostDeviceId)
        try c.encode(files, forKey: .files)
        try c.encode(bytes, forKey: .bytes)
        try c.encode(pending, forKey: .pending)
        try c.encode(preview, forKey: .preview)
        try c.encode(createdAt, forKey: .createdAt)
        try c.encode(updatedAt, forKey: .updatedAt)
        try c.encode(consolidation, forKey: .consolidation)
        try c.encodeIfPresent(lastPass, forKey: .lastPass)
        try c.encodeIfPresent(month, forKey: .month)
        try c.encodeIfPresent(passRequestedAt, forKey: .passRequestedAt)
        if !held.isEmpty { try c.encode(held, forKey: .held) }
        if !decided.isEmpty { try c.encode(decided, forKey: .decided) }
    }
}

/// Who wrote a file, a pending write or a change (tag `kind`).
enum MemoryAuthor: Hashable, Sendable {
    /// The user, in the app.
    case owner
    /// An agent in a session (or a subagent's chat).
    case session(chatId: String, title: String?)
    /// A workspace's agent, in one of its threads.
    case workspace(botId: String, chatId: String, title: String?)
    /// A consolidation pass.
    case consolidation(passId: String)
    /// Moved from the old workspace notebook.
    case notebook

    /// A short "who" for lists: "You", the session's title, "Consolidation".
    var label: String {
        func titled(_ title: String?, else fallback: String) -> String {
            guard let title, !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return fallback }
            return title
        }
        switch self {
        case .owner: return "You"
        case .session(_, let title): return titled(title, else: "A session")
        case .workspace(_, _, let title): return titled(title, else: "The workspace")
        case .consolidation: return "Consolidation"
        case .notebook: return "Notebook"
        }
    }

    /// The chat an agent author wrote from.
    var chatId: String? {
        switch self {
        case .session(let chatId, _), .workspace(_, let chatId, _): return chatId
        default: return nil
        }
    }
}

extension MemoryAuthor: Codable {
    private enum CodingKeys: String, CodingKey { case kind, chatId, title, botId, passId }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "owner": self = .owner
        case "session":
            self = .session(
                chatId: try c.decode(String.self, forKey: .chatId),
                title: try c.decodeIfPresent(String.self, forKey: .title))
        case "workspace":
            self = .workspace(
                botId: try c.decode(String.self, forKey: .botId),
                chatId: try c.decode(String.self, forKey: .chatId),
                title: try c.decodeIfPresent(String.self, forKey: .title))
        case "consolidation": self = .consolidation(passId: try c.decode(String.self, forKey: .passId))
        case "notebook": self = .notebook
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown author \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .owner:
            try c.encode("owner", forKey: .kind)
        case .session(let chatId, let title):
            try c.encode("session", forKey: .kind)
            try c.encode(chatId, forKey: .chatId)
            try c.encodeIfPresent(title, forKey: .title)
        case .workspace(let botId, let chatId, let title):
            try c.encode("workspace", forKey: .kind)
            try c.encode(botId, forKey: .botId)
            try c.encode(chatId, forKey: .chatId)
            try c.encodeIfPresent(title, forKey: .title)
        case .consolidation(let passId):
            try c.encode("consolidation", forKey: .kind)
            try c.encode(passId, forKey: .passId)
        case .notebook:
            try c.encode("notebook", forKey: .kind)
        }
    }
}

/// One file of a store. `path` starts with `/` and is store-relative.
struct MemoryFile: Hashable, Identifiable, Sendable {
    var path: String
    var text: String
    var author: MemoryAuthor = .owner
    var createdAt: Int64 = 0
    var updatedAt: Int64 = 0

    var id: String { path }
}

extension MemoryFile: Codable {
    private enum CodingKeys: String, CodingKey { case path, text, author, createdAt, updatedAt }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        text = try c.decode(String.self, forKey: .text)
        author = try c.decode(.author, or: .owner)
        createdAt = try c.decode(.createdAt, or: 0)
        updatedAt = try c.decode(.updatedAt, or: 0)
    }
}

/// What a change did to its file (tag `kind`).
enum MemoryAction: Hashable, Sendable {
    case create, edit, delete
    case rename(from: String)
    /// An earlier change, or a whole pass, put back.
    case undo(changeId: String)
}

extension MemoryAction: Codable {
    private enum CodingKeys: String, CodingKey { case kind, from, changeId }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "create": self = .create
        case "edit": self = .edit
        case "delete": self = .delete
        case "rename": self = .rename(from: try c.decode(String.self, forKey: .from))
        case "undo": self = .undo(changeId: try c.decode(String.self, forKey: .changeId))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown action \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .create: try c.encode("create", forKey: .kind)
        case .edit: try c.encode("edit", forKey: .kind)
        case .delete: try c.encode("delete", forKey: .kind)
        case .rename(let from):
            try c.encode("rename", forKey: .kind)
            try c.encode(from, forKey: .from)
        case .undo(let changeId):
            try c.encode("undo", forKey: .kind)
            try c.encode(changeId, forKey: .changeId)
        }
    }
}

/// One entry of a store's change log.
struct MemoryChange: Hashable, Identifiable, Sendable {
    var id: String
    var at: Int64
    var author: MemoryAuthor
    var path: String
    var action: MemoryAction
    /// One line: what changed ("Added 2 lines", "Removed the file").
    var summary = ""
    /// The file's text before the change (clipped).
    var before: String?
    /// It replaced text the owner wrote.
    var ownerText = false
    /// The doc version before the change, to undo it exactly.
    var undo: String?
    /// Already undone.
    var undone = false
}

extension MemoryChange: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, at, author, path, action, summary, before, ownerText, undo, undone
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        at = try c.decode(Int64.self, forKey: .at)
        author = try c.decode(MemoryAuthor.self, forKey: .author)
        path = try c.decode(String.self, forKey: .path)
        action = try c.decode(MemoryAction.self, forKey: .action)
        summary = try c.decode(.summary, or: "")
        before = try c.decodeIfPresent(String.self, forKey: .before)
        ownerText = try c.decode(.ownerText, or: false)
        undo = try c.decodeIfPresent(String.self, forKey: .undo)
        undone = try c.decode(.undone, or: false)
    }
}

/// A write held for review: made by a run that had read untrusted content.
struct MemoryPending: Hashable, Identifiable, Sendable {
    var id: String
    var at: Int64
    var author: MemoryAuthor
    var path: String
    /// The memory command (`create`, `str_replace`, …), applied as given
    /// when accepted.
    var command: String
    var args: JSONValue = .null
    /// Why it is held ("the session fetched a web page").
    var reason: String
    /// The file as the write would leave it (nil: deleted).
    var preview: String?
    /// The `memory` tool call that made it.
    var toolCallId: String?
}

extension MemoryPending: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, at, author, path, command, args, reason, preview, toolCallId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        at = try c.decode(Int64.self, forKey: .at)
        author = try c.decode(MemoryAuthor.self, forKey: .author)
        path = try c.decode(String.self, forKey: .path)
        command = try c.decode(String.self, forKey: .command)
        args = try c.decode(.args, or: .null)
        reason = try c.decode(String.self, forKey: .reason)
        preview = try c.decodeIfPresent(String.self, forKey: .preview)
        toolCallId = try c.decodeIfPresent(String.self, forKey: .toolCallId)
    }
}

/// Everything the app shows of one store (`WatchMemory` frames).
struct MemoryView: Codable, Hashable, Sendable {
    var store: MemoryStore
    /// `MEMORY.md` first, then by path.
    var files: [MemoryFile]
    var pending: [MemoryPending]
    /// Newest first.
    var changes: [MemoryChange]
}

/// One edit from the app (`EditMemory {scope, edit}`, tag `op`).
enum MemoryEdit: Hashable, Sendable {
    /// Create or replace a file.
    case write(path: String, text: String)
    case delete(path: String)
    case rename(from: String, to: String)
    /// Apply a pending write.
    case accept(id: String)
    case discard(id: String)
    /// Put back what one change did.
    case undo(changeId: String)
    /// Put back what a whole pass did.
    case undoPass(passId: String)
    case setConsolidation(enabled: Bool, hour: Int)
    /// Ask the store's host to run a pass now.
    case runPass
}

extension MemoryEdit: Codable {
    private enum CodingKeys: String, CodingKey { case op, path, text, from, to, id, changeId, passId, enabled, hour }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .op) {
        case "write":
            self = .write(
                path: try c.decode(String.self, forKey: .path), text: try c.decode(String.self, forKey: .text))
        case "delete": self = .delete(path: try c.decode(String.self, forKey: .path))
        case "rename":
            self = .rename(from: try c.decode(String.self, forKey: .from), to: try c.decode(String.self, forKey: .to))
        case "accept": self = .accept(id: try c.decode(String.self, forKey: .id))
        case "discard": self = .discard(id: try c.decode(String.self, forKey: .id))
        case "undo": self = .undo(changeId: try c.decode(String.self, forKey: .changeId))
        case "undoPass": self = .undoPass(passId: try c.decode(String.self, forKey: .passId))
        case "setConsolidation":
            self = .setConsolidation(
                enabled: try c.decode(Bool.self, forKey: .enabled), hour: try c.decode(Int.self, forKey: .hour))
        case "runPass": self = .runPass
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .op, in: c, debugDescription: "unknown edit \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .write(let path, let text):
            try c.encode("write", forKey: .op)
            try c.encode(path, forKey: .path)
            try c.encode(text, forKey: .text)
        case .delete(let path):
            try c.encode("delete", forKey: .op)
            try c.encode(path, forKey: .path)
        case .rename(let from, let to):
            try c.encode("rename", forKey: .op)
            try c.encode(from, forKey: .from)
            try c.encode(to, forKey: .to)
        case .accept(let id):
            try c.encode("accept", forKey: .op)
            try c.encode(id, forKey: .id)
        case .discard(let id):
            try c.encode("discard", forKey: .op)
            try c.encode(id, forKey: .id)
        case .undo(let changeId):
            try c.encode("undo", forKey: .op)
            try c.encode(changeId, forKey: .changeId)
        case .undoPass(let passId):
            try c.encode("undoPass", forKey: .op)
            try c.encode(passId, forKey: .passId)
        case .setConsolidation(let enabled, let hour):
            try c.encode("setConsolidation", forKey: .op)
            try c.encode(enabled, forKey: .enabled)
            try c.encode(hour, forKey: .hour)
        case .runPass:
            try c.encode("runPass", forKey: .op)
        }
    }
}

enum MemoryLimits {
    /// The always-injected index of every store.
    static let index = "/MEMORY.md"
    static let fileMax = 64 * 1024
    static let pathMax = 160
}

/// memory.rs `valid_memory_path`: starts with `/`, ends in `.md`, no empty,
/// `.` or `..` segments (nor hidden ones), printable, within 160 bytes.
func validMemoryPath(_ path: String) -> Bool {
    guard path.hasPrefix("/"), path.hasSuffix(".md"), path.utf8.count <= MemoryLimits.pathMax else { return false }
    let segments = path.dropFirst().split(separator: "/", omittingEmptySubsequences: false)
    guard segments.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." && !$0.hasPrefix(".") }) else { return false }
    return !path.unicodeScalars.contains {
        CharacterSet.controlCharacters.contains($0) || $0 == "\\" || $0 == "|"
    }
}

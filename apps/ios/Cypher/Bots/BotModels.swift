// Workspaces — agent bots in code (docs/design/agent-bots.md): the Swift
// mirror of crates/proto/src/bot.rs. Each type round-trips the engine's serde
// JSON (camelCase keys, epoch-ms times) and decodes as leniently as serde
// does: a missing defaulted field takes the Rust default, and an origin or
// thread kind from a newer engine reads as the narrowest case, so a row
// written by a newer engine never grants more than it meant to.

import Foundation

enum BotLimits {
    /// Longest workspace name, in characters.
    static let nameMaxChars = 40
    /// Longest owner instructions, in characters.
    static let instructionsMaxChars = 8 * 1024
    /// Projects or session ids one scope may list.
    static let scopeMaxSessions = 2_000
    /// Largest field set one record write may carry, in UTF-16 units of its
    /// JSON (registry.rs `RECORD_MAX_BYTES`, under the Edge's 16 KiB op cap).
    static let recordUnits = 14 * 1024
}

extension KeyedDecodingContainer {
    /// serde's `#[serde(default)]`: a missing (or null) field reads as the
    /// default; a field of the wrong type still fails, as it does in Rust.
    func decode<T: Decodable>(_ key: Key, or fallback: @autoclosure () -> T) throws -> T {
        try decodeIfPresent(T.self, forKey: key) ?? fallback()
    }
}

// MARK: - Bot

/// One workspace definition (a registry `bots` row).
struct Bot: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var name: String
    /// Avatar glyph key (a robot face or a symbol, `BotStyle`); nil = the
    /// name's initial.
    var icon: String?
    /// Avatar colour key from the project colour set; nil = neutral.
    var color: String?
    /// The engine that runs the workspace. Fixed at creation.
    var hostDeviceId: String
    /// The workspace's own conversation, hosted on `hostDeviceId`.
    var chatId: String
    /// The owner's standing instructions.
    var instructions: String?
    var scope = BotScope()
    /// What its owner threads may do beyond reading.
    var capabilities = BotCapabilities()
    /// Kill switch: no routine runs and no new work starts while set.
    var paused = false
    /// Epoch ms.
    var createdAt: Int64
}

extension Bot {
    /// rows.rs `RawBot`: a scope or capabilities this app can't read (a newer
    /// app wrote it) read as the empty scope and no capabilities, so the
    /// workspace shows as seeing less, never more. `createdAt` is epoch ms
    /// in the registry row and an RFC 3339 string in `CreateBot`'s reply.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        icon = try c.decodeIfPresent(String.self, forKey: .icon)
        color = try c.decodeIfPresent(String.self, forKey: .color)
        hostDeviceId = try c.decode(String.self, forKey: .hostDeviceId)
        chatId = try c.decode(String.self, forKey: .chatId)
        instructions = try c.decodeIfPresent(String.self, forKey: .instructions)
        let scope = (try? c.decodeIfPresent(BotScope.self, forKey: .scope)) ?? nil
        self.scope = scope.flatMap { $0.validationError == nil ? $0 : nil } ?? BotScope()
        capabilities =
            ((try? c.decodeIfPresent(BotCapabilities.self, forKey: .capabilities)) ?? nil) ?? BotCapabilities()
        paused = ((try? c.decodeIfPresent(Bool.self, forKey: .paused)) ?? nil) ?? false
        if let ms = try? c.decodeIfPresent(Int64.self, forKey: .createdAt) {
            createdAt = ms
        } else if let text = try? c.decodeIfPresent(String.self, forKey: .createdAt) {
            createdAt = rfc3339Millis(text) ?? 0
        } else {
            createdAt = 0
        }
    }

    /// bot.rs `validate_name`: a trimmed, non-empty name of at most 40
    /// characters; nil when it is fine.
    static func nameError(_ name: String) -> String? {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty { return "Give the workspace a name." }
        if name.count > BotLimits.nameMaxChars {
            return "Name must be \(BotLimits.nameMaxChars) characters or fewer."
        }
        return nil
    }
}

/// chrono's RFC 3339 serialization (up to nanosecond fractions) as epoch ms.
func rfc3339Millis(_ text: String) -> Int64? {
    // ISO8601DateFormatter takes at most millisecond fractions: cut the
    // fraction out and add it back.
    var base = text
    var fraction = 0.0
    if let dot = text.firstIndex(of: "."), let tIndex = text.firstIndex(of: "T"), dot > tIndex {
        let digits = text[text.index(after: dot)...].prefix { $0.isNumber }
        fraction = Double("0.\(digits)") ?? 0
        base = String(text[..<dot]) + String(text[text.index(after: dot)...].dropFirst(digits.count))
    }
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime]
    guard let date = formatter.date(from: base) else { return nil }
    return Int64(((date.timeIntervalSince1970 + fraction) * 1000).rounded())
}

// MARK: - Scope

/// Which projects a workspace sees (tag `mode`).
enum ProjectGrant: Hashable, Sendable {
    /// No project: only sessions named in `includeSessions`.
    case noProjects
    /// Every project, including ones created later.
    case all
    /// The listed spaces.
    case selected([String])
}

extension ProjectGrant: Codable {
    private enum CodingKeys: String, CodingKey { case mode, spaceIds }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .mode) {
        case "none": self = .noProjects
        case "all": self = .all
        case "selected": self = .selected(try c.decode([String].self, forKey: .spaceIds))
        case let other:
            throw DecodingError.dataCorruptedError(
                forKey: .mode, in: c, debugDescription: "unknown project grant \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .noProjects: try c.encode("none", forKey: .mode)
        case .all: try c.encode("all", forKey: .mode)
        case .selected(let ids):
            try c.encode("selected", forKey: .mode)
            try c.encode(ids, forKey: .spaceIds)
        }
    }
}

/// What a workspace may read (bot.rs `BotScope`). A granted project grants
/// its sessions, including future ones; `excludeSessions` removes single
/// sessions and `includeSessions` adds sessions from ungranted projects.
struct BotScope: Hashable, Sendable {
    var projects: ProjectGrant = .noProjects
    var includeSessions: [String] = []
    var excludeSessions: [String] = []
    /// Archived sessions stay hidden unless this is set.
    var includeArchived = false
    /// Message text may be read; off, only titles, previews and status.
    var transcripts = true

    /// Whether the project (space) is granted.
    func seesSpace(_ spaceId: String) -> Bool {
        switch projects {
        case .noProjects: return false
        case .all: return true
        case .selected(let ids): return ids.contains(spaceId)
        }
    }

    /// bot.rs `BotScope::validate`; nil when the engine accepts it.
    var validationError: String? {
        var selected = 0
        if case .selected(let ids) = projects { selected = ids.count }
        if selected > BotLimits.scopeMaxSessions || includeSessions.count > BotLimits.scopeMaxSessions
            || excludeSessions.count > BotLimits.scopeMaxSessions
        {
            return "The scope lists too many projects or sessions."
        }
        return nil
    }
}

extension BotScope: Codable {
    private enum CodingKeys: String, CodingKey {
        case projects, includeSessions, excludeSessions, includeArchived, transcripts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        projects = try c.decode(.projects, or: .noProjects)
        includeSessions = try c.decode(.includeSessions, or: [])
        excludeSessions = try c.decode(.excludeSessions, or: [])
        includeArchived = try c.decode(.includeArchived, or: false)
        transcripts = try c.decode(.transcripts, or: true)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(projects, forKey: .projects)
        if !includeSessions.isEmpty { try c.encode(includeSessions, forKey: .includeSessions) }
        if !excludeSessions.isEmpty { try c.encode(excludeSessions, forKey: .excludeSessions) }
        try c.encode(includeArchived, forKey: .includeArchived)
        try c.encode(transcripts, forKey: .transcripts)
    }
}

/// What a workspace's owner threads may do beyond reading. Rows written
/// before these existed read as all off (read-only).
struct BotCapabilities: Hashable, Sendable {
    /// Pi's shell and file tools in its local projects, under permission
    /// control.
    var workDirectly = false
    /// Plans, worktrees and sessions in its projects, on any owner device.
    var coordinate = false
    /// Steer, answer or stop sessions it did not start.
    var steer = false

    /// What a new workspace starts with: it works directly and coordinates
    /// (every plan still waits for approval); steering is opt-in.
    static let newBot = BotCapabilities(workDirectly: true, coordinate: true, steer: false)
}

extension BotCapabilities: Codable {
    private enum CodingKeys: String, CodingKey { case workDirectly, coordinate, steer }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        workDirectly = try c.decode(.workDirectly, or: false)
        coordinate = try c.decode(.coordinate, or: false)
        steer = try c.decode(.steer, or: false)
    }
}

// MARK: - Chat markers

/// Who started a workspace thread, which fixes what it may do. An origin
/// this app does not know reads as `peer`, the narrowest.
enum BotOrigin: String, Hashable, Sendable {
    case owner, routine, peer
}

extension BotOrigin: Codable {
    init(from decoder: Decoder) throws {
        self = BotOrigin(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .peer
    }
}

/// Which of a workspace's threads a chat is (bot_records.rs `BotThread`,
/// tag `kind`). The workspace chat is `main`; the rest are hidden chats the
/// host creates.
enum BotThread: Hashable, Sendable {
    case main
    /// Carries out one approved plan.
    case work(workId: String)
    /// Where one routine runs.
    case routine(routineId: String)
    /// A project routine's session: an ordinary coding session the routine
    /// owns, hidden like the threads but run as a plain session.
    case routineSession(routineId: String)
    /// Where one connection's questions run.
    case peer(connectionId: String)
    /// A thread kind from a newer engine: shown nowhere, does nothing.
    case unknown
}

extension BotThread: Codable {
    private enum CodingKeys: String, CodingKey { case kind, workId, routineId, connectionId }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "main": self = .main
        case "work": self = .work(workId: try c.decode(String.self, forKey: .workId))
        case "routine": self = .routine(routineId: try c.decode(String.self, forKey: .routineId))
        case "routineSession": self = .routineSession(routineId: try c.decode(String.self, forKey: .routineId))
        case "peer": self = .peer(connectionId: try c.decode(String.self, forKey: .connectionId))
        default: self = .unknown
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .main: try c.encode("main", forKey: .kind)
        case .work(let id):
            try c.encode("work", forKey: .kind)
            try c.encode(id, forKey: .workId)
        case .routine(let id):
            try c.encode("routine", forKey: .kind)
            try c.encode(id, forKey: .routineId)
        case .routineSession(let id):
            try c.encode("routineSession", forKey: .kind)
            try c.encode(id, forKey: .routineId)
        case .peer(let id):
            try c.encode("peer", forKey: .kind)
            try c.encode(id, forKey: .connectionId)
        case .unknown: try c.encode("unknown", forKey: .kind)
        }
    }
}

/// The marker on a workspace thread's chat row (bot.rs `BotChat`). Only the
/// host engine writes it. Rows from before origins existed are the
/// workspaces' own chats, so a missing origin reads as the owner.
struct BotChatMarker: Hashable, Sendable {
    var botId: String
    var origin: BotOrigin = .owner
    var thread: BotThread = .main

    /// The workspace chat itself, not a hidden thread.
    var isMain: Bool { thread == .main }

    /// A thread the workspace's agent runs in: every marked chat but a
    /// project routine's session, which the marker only hides.
    var isBotThread: Bool {
        if case .routineSession = thread { return false }
        return true
    }
}

extension BotChatMarker: Codable {
    private enum CodingKeys: String, CodingKey { case botId, origin, thread }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        botId = try c.decode(String.self, forKey: .botId)
        origin = try c.decode(.origin, or: .owner)
        thread = try c.decode(.thread, or: .main)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(botId, forKey: .botId)
        try c.encode(origin, forKey: .origin)
        if thread != .main { try c.encode(thread, forKey: .thread) }
    }
}

/// A memory consolidation pass's hidden chat (memory.rs `MemoryChat`).
struct MemoryChatMarker: Codable, Hashable, Sendable {
    var storeId: String
    var scope: MemoryScope
    var passId: String
}

// MARK: - Create

/// `CreateBot` parameters (crates/engine/src/bots.rs `CreateBot`, which
/// refuses unknown fields). The target device becomes the host.
struct CreateBotRequest: Encodable, Hashable, Sendable {
    var name: String
    var icon: String?
    var color: String?
    var instructions: String?
    var scope = BotScope()
    /// The workspace chat's model; nil = the host's default.
    var model: String?
    /// Lowercase reasoning level ("high", "max", …).
    var reasoning: String?
    /// nil = a new workspace's defaults (`BotCapabilities.newBot`).
    var capabilities: BotCapabilities?
}

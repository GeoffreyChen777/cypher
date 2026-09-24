// Entity model — Swift mirrors of the workspace/session doc rows
// (crates/doc/src/workspace.rs, schema.rs) and the derived display state
// (crates/ui/src/state.rs, entities.rs). Field names match the doc schema
// exactly; derivations (indicator, staleness, attention rank) are ports.

import Foundation

// MARK: - Workspace doc rows

struct DeviceRow: Identifiable, Hashable {
    var id: String
    var name: String
    var platform: String
    var lastSeenAt: Int64?
    var createdAt: Int64?
}

struct Space: Identifiable, Hashable {
    var id: String
    var deviceId: String
    var path: String
    var name: String?
    var gitDetected: Bool
    var gitCheckedAt: Int64?
    var checkoutId: String?
    var createdAt: Int64

    /// Display name: explicit name, else the folder's basename.
    var displayName: String {
        if let name, !name.isEmpty { return name }
        return (path as NSString).lastPathComponent
    }
}

struct ChatConfig: Hashable, Codable {
    var harness: String
    var model: String?
    var reasoning: String?
    /// Harness-specific option picks (option id → choice id, proto
    /// `ChatConfig.model_options`). Round-tripped so a mobile config edit
    /// never clobbers options the desktop pickers set — `setChatConfig`
    /// rewrites the whole `config` field under per-field LWW.
    var modelOptions: [String: JSONValue] = [:]
    var sandbox: String?
}

struct Chat: Identifiable, Hashable {
    var id: String
    var deviceId: String
    var title: String?
    var archived: Bool
    var cwd: String?
    var branch: String?
    var checkoutId: String?
    var config: ChatConfig?
    var lastMessagePreview: String?
    var lastMessageAt: Int64?
    var createdAt: Int64
    var spaceId: String?
    var lastSeenAt: Int64?
    /// Sync room generation (docs/chat2-sync.md M2): absent/1 = legacy s2
    /// (never dialed from mobile), 2 = chat2. The host flips it when seeding.
    var roomGen: Int? = nil
    var child: ChildChat? = nil

    var isChild: Bool { child != nil }

    /// A quick chat (proto scratch.rs): no project, and the cwd is the
    /// host's `…/cypher-scratch/<chat id>` folder. The folder shape IS the
    /// identity — there is no flag on the row.
    var isScratch: Bool {
        guard spaceId == nil, var path = cwd else { return false }
        while path.hasSuffix("/") { path.removeLast() }
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        return parts.count >= 2 && parts[parts.count - 1] == Substring(id)
            && parts[parts.count - 2] == "cypher-scratch"
    }

    var displayTitle: String {
        if let title, !title.isEmpty { return title }
        return "New session"
    }

    /// entities.rs:123 — unseen when a message arrived after the last seen mark.
    var unseen: Bool {
        guard let lastMessageAt else { return false }
        guard let lastSeenAt else { return true }
        return lastMessageAt > lastSeenAt
    }
}

enum SessionStatus: String {
    case idle, working, awaitingInput, errored
}

struct SessionRow: Hashable {
    var chatId: String
    var deviceId: String
    var status: SessionStatus
    var startedAt: Int64?
    var updatedAt: Int64
    var subagents: [SubagentRun] = []
    /// The agent's context-window occupancy (entities.rs `context_usage`).
    /// Rides the row's next write while a turn runs, so it can trail a live
    /// turn by ~20s; settles immediately.
    var contextUsage: ContextUsage? = nil
}

/// entities.rs `ContextUsage`: tokens in the context window, of its size.
struct ContextUsage: Hashable {
    var used: Int64
    var size: Int64

    /// Clamped to 0…1; an unknown (zero) size reads as empty.
    var fraction: Double {
        size > 0 ? min(max(Double(used) / Double(size), 0), 1) : 0
    }

    /// workspace.rs: lenient — a malformed value drops the reading, never
    /// the row.
    init?(_ value: JSONValue?) {
        guard let object = value?.objectValue,
              let used = object["used"]?.int64Value,
              let size = object["size"]?.int64Value else { return nil }
        self.used = used
        self.size = size
    }

    init(used: Int64, size: Int64) {
        self.used = used
        self.size = size
    }
}

// MARK: - Derived display status (entities.rs / state.rs ports)

enum ChatIndicator: Int {
    case awaitingInput = 0
    case errored = 1
    case working = 2
    case completed = 3
    case idle = 4

    /// Projects keep activity visible while any session is running. Once all
    /// runs stop, surface attention-needed states before unread completions.
    static func projectSummary(_ indicators: [ChatIndicator]) -> ChatIndicator? {
        if indicators.contains(.working) { return .working }
        return indicators.min { $0.rawValue < $1.rawValue }
    }
}

/// state.rs:277 — a Working/AwaitingInput row older than this reads as stale
/// (a crashed backend never shows eternal "Working").
let sessionStaleMs: Int64 = 45_000
/// workspace_host.rs:45 — presence freshness window for device online dots.
let presenceFreshMs: Int64 = 45_000

func effectiveStatus(_ row: SessionRow?, now: Int64) -> SessionStatus? {
    guard let row else { return nil }
    switch row.status {
    case .working, .awaitingInput:
        let age = now - row.updatedAt
        // Negative ages (clock skew) are fresh.
        return age > sessionStaleMs ? nil : row.status
    case .errored, .idle:
        return row.status
    }
}

/// entities.rs:147 — live Working/AwaitingInput win; Errored only if unseen;
/// else unseen ⇒ Completed; else Idle.
func chatIndicator(chat: Chat, live: SessionStatus?) -> ChatIndicator {
    switch live {
    case .working: return .working
    case .awaitingInput: return .awaitingInput
    case .errored: return chat.unseen ? .errored : .idle
    default: return chat.unseen ? .completed : .idle
    }
}

/// The Sessions list order: PURE RECENCY, id tiebreak — a port of state.rs
/// `sort_active`. Status drives the dot, never the position.
///
/// This used to bucket by attention first, which is what the desktop did
/// before 55e1845: opening a completed session marks it seen (completed →
/// idle), and the row then dropped a bucket out from under the pointer. The
/// dots carry urgency instead, so the order never moves on its own.
func sortActive(_ chats: [Chat]) -> [Chat] {
    chats.sorted { a, b in
        let ta = a.lastMessageAt ?? a.createdAt, tb = b.lastMessageAt ?? b.createdAt
        if ta != tb { return ta > tb }
        return a.id < b.id
    }
}

// MARK: - Session doc entries

enum MessageRole: String {
    case user, assistant, system
}

enum MessageStatus: String {
    case streaming, complete, aborted
}

struct UserInputQuestion: Hashable, Codable {
    var id: String
    var header: String
    var question: String
    /// Plain labels — `proto::agent::UserInputQuestion.options` is a
    /// `Vec<String>`. This was modelled as `{label, description}` objects,
    /// which NEVER decoded: every question arrived empty, so the panel had no
    /// options to show and an unresolved request crashed the app.
    var options: [String]
    var multiSelect: Bool?
}

struct UserInputAnswer: Hashable, Codable {
    var questionId: String
    var labels: [String]
}

/// Render-only sanitized tool call (packages render-parts policy).
struct RenderToolCall: Hashable {
    var tag: String
    /// Loose payload — only render-relevant fields survive in the doc.
    var fields: [String: AnyHashable]
    var subagent: SubagentCallMetadata? = nil
    var progress: String? = nil

    var string: (String) -> String? { { key in self.fields[key] as? String } }
}

enum MessagePart: Hashable, Identifiable {
    case text(id: String, text: String)
    case tool(id: String, call: RenderToolCall, isError: Bool, resolved: Bool)
    case input(id: String, requestId: String, questions: [UserInputQuestion], resolved: Bool)
    case error(id: String, message: String)

    var id: String {
        switch self {
        case .text(let id, _), .tool(let id, _, _, _), .input(let id, _, _, _), .error(let id, _):
            return id
        }
    }
}

struct MessageEntry: Identifiable, Hashable {
    var id: String
    var role: MessageRole
    var parts: [MessagePart]
    var createdAt: Int64
    var deviceId: String
    var status: MessageStatus?
    var continuationOf: String?
    /// Explicit sender intent from the durable command ledger, not a claim
    /// that the agent has consumed the instruction (or a new wire field).
    var isSteer = false
}

struct PendingSend {
    var messageId: String
    var text: String
    var at: Int64
    var isSteer = false
}

// MARK: - Folder browsing (add-space palette data)

/// cypher-proto FolderListing (entities.rs:225): the device's answer to
/// ListFolders. Dotfiles are pre-filtered and entries are capped at 500 by
/// the engine; the parent path is computed client-side.
struct FolderEntry: Codable, Hashable {
    var name: String
    var isDir: Bool
    var isRepo: Bool
}

struct FolderListing: Codable {
    var path: String
    var entries: [FolderEntry]
    var truncated: Bool

    var parent: String? {
        guard path.contains("/"), path != "/" else { return nil }
        let trimmed = String(path[..<(path.lastIndex(of: "/") ?? path.startIndex)])
        return trimmed.isEmpty ? "/" : trimmed
    }
}

/// pickers.rs CheckoutKind — where a new session runs. "Current worktree" is
/// NOT a third mode: it's `local` when the picked ref is already materialized
/// as a worktree (the session reuses that checkout's path).
enum CheckoutKind {
    case local
    case newWorktree
}

/// cypher-proto RepoRef (entities.rs:193): one selectable ref from ListRefs.
struct RepoRef: Codable, Hashable, Identifiable {
    var name: String
    var current: Bool = false
    var worktreePath: String?

    var id: String { name }
}

// MARK: - Command ledger (commands.rs port)

let commandDefaultTtlMs: Int64 = 86_400_000

/// cypher-proto RunRequest (agent.rs:81). `reasoning` is lowercase
/// ("high"/"xhigh"/…), `sandbox` kebab-case ("workspace-write"), harness ids
/// kebab-case ("claude-code").
/// proto session_fork.rs `SessionForkResponse`, tagged by `kind`.
enum ForkResponse: Decodable, Equatable {
    /// `composerText`: forking before a user message hands its text back
    /// for editing.
    case created(chatId: String, title: String?, composerText: String?)
    case unavailable(message: String)

    private enum Keys: String, CodingKey { case kind, chat, composerText, message }
    private struct Row: Decodable { var id: String; var title: String? }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "created":
            let row = try c.decode(Row.self, forKey: .chat)
            self = .created(chatId: row.id, title: row.title,
                            composerText: try c.decodeIfPresent(String.self, forKey: .composerText))
        default:
            self = .unavailable(message: try c.decodeIfPresent(String.self, forKey: .message)
                                ?? "This session can't be forked here.")
        }
    }
}

struct RunRequest: Codable {
    var prompt: String
    /// Harness id ("claude-code") picked at send time; rides the command so
    /// the host's claim-on-first-command records it even when the chat row is
    /// still syncing.
    var harness: String?
    var model: String?
    var reasoning: String?
    var modelOptions: [String: JSONValue] = [:]
    var cwd: String
    var sandbox: String = "workspace-write"
    var autoApprove: Bool = true
    var resume: String?
    /// Absolute paths of image attachments already staged on the run device
    /// (UploadChunk/UploadCommit). The same paths ride the prompt text as
    /// `Attached images (local files …)` refs — this field additionally lets
    /// a harness inline the bytes as image content blocks.
    var attachments: [String] = []
}

enum SessionCommandPayload {
    case run(request: RunRequest, messageId: String, agentPrompt: String? = nil)
    case steer(prompt: String, messageId: String?, agentPrompt: String? = nil)
    case interrupt
    case respondInput(requestId: String, answers: [UserInputAnswer])

    var kind: String {
        switch self {
        case .run: return "run"
        case .steer: return "steer"
        case .interrupt: return "interrupt"
        case .respondInput: return "respondInput"
        }
    }
}

func nowMs() -> Int64 {
    Int64(Date().timeIntervalSince1970 * 1000)
}

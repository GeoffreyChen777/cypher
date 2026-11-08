// Entity model — Swift mirrors of the workspace/session doc rows
// (crates/doc/src/registry/rows.rs, schema.rs) and the derived display state
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
    /// A context recovery under way (entities.rs `recovery`): the host is
    /// writing supplementary context for a failed turn, or restarting it.
    var recovery: RecoveryProgress? = nil
    /// A long turn's narrated progress (entities.rs `progress`): the status
    /// strip's progress bar. Belongs to one turn; a settled row drops it.
    var progress: RunProgress? = nil
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
            let size = object["size"]?.int64Value
        else { return nil }
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
        let ta = a.lastMessageAt ?? a.createdAt
        let tb = b.lastMessageAt ?? b.createdAt
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
    /// Set when this is a permission prompt, not a question the agent asks
    /// (proto agent.rs `PermissionPrompt`): shown as an approval, and filed
    /// under the tool call it gates once answered.
    var permission: PermissionPrompt?
    /// The subagent that asked, when the host forwarded a child chat's
    /// question or permission prompt into this one (proto `asked_by`).
    var askedBy: String?
}

/// proto agent.rs `PermissionPrompt`: what a permission prompt is about.
struct PermissionPrompt: Hashable, Codable {
    /// `escalate` (run a command outside the sandbox), `access` (a file
    /// tool past the defaults), `network`, `tool`.
    var kind: String
    /// The tool call waiting on the answer.
    var toolCallId: String?
}

/// proto agent.rs `ToolPermission`: how permission control treated one tool
/// call — the badge on its row.
struct ToolPermission: Hashable, Codable {
    enum Outcome: String, Codable {
        /// Ran outside the sandbox.
        case outside
        /// Ran in the sandbox, which withheld something it needed.
        case blocked
        /// Refused; it did not run.
        case denied
        /// A file tool's access was approved.
        case allowed
    }

    enum Scope: String, Codable {
        case once, chat, always
        /// An earlier grant or rule; nobody was asked.
        case rule
    }

    var outcome: Outcome
    var scope: Scope?
    /// The command rule that let it run outside, as people read it.
    var rule: String?
    /// What the agent said the command was for.
    var purpose: String?
}

struct UserInputAnswer: Hashable, Codable {
    var questionId: String
    var labels: [String]
}

/// Render-only sanitized tool call (packages render-parts policy).
/// `@unchecked Sendable`: `fields` only ever holds JSON scalars and string
/// lists (SessionStore's decoder), all immutable values.
struct RenderToolCall: Hashable, @unchecked Sendable {
    var tag: String
    /// Loose payload — only render-relevant fields survive in the doc.
    var fields: [String: AnyHashable]
    var subagent: SubagentCallMetadata? = nil
    var progress: String? = nil
    /// How permission control treated the call (the part's `permission`).
    /// Not part of the call; it rides here so every tool part keeps one shape.
    var permission: ToolPermission? = nil

    var string: (String) -> String? { { key in self.fields[key] as? String } }
}

enum MessagePart: Hashable, Identifiable {
    /// `agentText`: with Pi translation on, the agent's own words when `text`
    /// shows a translation (doc parts.rs `agent_text`). An append-mode
    /// rendering is `agentText`, a rule, then the translation.
    case text(id: String, text: String, agentText: String? = nil)
    case tool(id: String, call: RenderToolCall, isError: Bool, resolved: Bool)
    case input(id: String, requestId: String, questions: [UserInputQuestion], resolved: Bool)
    case error(id: String, message: String)
    /// The model's thinking (doc parts.rs `Reasoning`), stored under the
    /// part's `reasoning` key.
    case reasoning(id: String, text: String)

    var id: String {
        switch self {
        case .text(let id, _, _), .tool(let id, _, _, _), .input(let id, _, _, _), .error(let id, _),
            .reasoning(let id, _):
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
    /// schema.rs `models`: the models that answered an assistant entry,
    /// distinct and in the order they first answered. Only Pi writes it.
    var models: [AnsweredModel] = []
    /// schema.rs `recovery`: a prompt the host sent again after a failed
    /// turn, with the context the agent received ahead of it.
    var recovery: ContextRecovery? = nil
    /// schema.rs `turnChanges`: the files the round this entry closed
    /// changed in its checkout, stamped by the host once the round settled.
    var turnChanges: TurnChanges? = nil
}

// MARK: - Turn changes (proto entities.rs `TurnChanges`)

/// The files one round of a conversation changed — from the prompt that set
/// the agent working to the moment it settled. `base` and `head` are the
/// host's `git write-tree` snapshots of the checkout at the round's start and
/// end, so the round's diff stays readable after later rounds (until `git gc`
/// prunes the unreferenced trees).
struct TurnChanges: Decodable, Hashable {
    /// Canonical checkout root on the host device.
    var cwd: String
    var base: String
    var head: String
    /// At most 100 files, in git's path order.
    var files: [WorkspaceChange]
    /// Every changed file, including any `files` leaves out.
    var fileCount: Int
    var additions: Int
    var deletions: Int

    /// Files the card lists; the rest are counted on its last row.
    static let cardFiles = 5

    /// "3 files changed" — the count covers files the entry left out.
    var label: String {
        let count = max(fileCount, files.count)
        return count == 1 ? "1 file changed" : "\(count) files changed"
    }

    /// How many changed files the card does not list.
    var hiddenFiles: Int {
        max(0, max(fileCount, files.count) - min(files.count, Self.cardFiles))
    }

    /// The quiet word a file's status earns beside its name — nil for a
    /// plain modification — and whether it reads as an addition, a deletion
    /// or neither (turn_changes.rs `status_tag`).
    static func statusTag(_ status: String) -> (word: String, added: Bool?)? {
        switch status {
        case "added": ("new", true)
        case "deleted": ("deleted", false)
        case "renamed": ("renamed", nil)
        case "copied": ("copied", nil)
        case "unmerged": ("conflict", nil)
        default: nil
        }
    }
}

// MARK: - Context recovery (proto context_recovery.rs)

/// Where a recovery's trigger matched.
enum RecoveryTriggerSource: String, Codable, Hashable {
    /// The text the model wrote.
    case response
    /// An error the turn ended with or reported.
    case error
}

/// `ContextRecovery`: what a recovered prompt carries in the transcript.
struct ContextRecovery: Codable, Hashable {
    /// The supplementary context, as the agent received it.
    var context: String
    /// The configured trigger that matched.
    var trigger: String
    var matchedIn: RecoveryTriggerSource
    /// The failed answer or error around the match.
    var excerpt: String
    /// The entry the conversation restarted after; nil when it restarted
    /// from the beginning of the chat.
    var restartAfter: String?
    /// That entry's opening words.
    var restartPreview: String?
    /// 1 for the first recovery of a request, counting up while recovered
    /// prompts keep failing.
    var attempt: UInt32
    /// The model that wrote `context`.
    var model: String?
    /// What the failed attempt had already done when it failed, as the agent
    /// received it, in the chat's language. Nil when it did no tool work.
    var alreadyDone: String?

    /// recovery.rs `caption_label`: what happened, and which attempt it was.
    var captionLabel: String {
        attempt > 1 ? "Restarted with context · attempt \(attempt)" : "Restarted with context"
    }

    /// recovery.rs `restart_label`: where the conversation restarted.
    var restartLabel: String {
        guard restartAfter != nil else { return "The beginning of the chat" }
        if let restartPreview { return "After \u{201c}\(restartPreview)\u{201d}" }
        return "After the previous answer"
    }

    /// recovery.rs `trigger_label`: what set the recovery off.
    var triggerLabel: String {
        let place = matchedIn == .response ? "the agent\u{2019}s answer" : "an error"
        return "\u{201c}\(trigger)\u{201d} in \(place)"
    }
}

/// `RecoveryStage`: how far a recovery under way has got.
enum RecoveryStage: String, Hashable {
    /// The recovery model is writing the supplementary context.
    case writingContext
    /// The chat is being rewound and the prompt sent again.
    case restarting
}

/// `RecoveryProgress`: a recovery under way, on the chat's session row.
struct RecoveryProgress: Hashable {
    var stage: RecoveryStage
    var trigger: String
    var matchedIn: RecoveryTriggerSource
    var attempt: UInt32
    /// Unix ms.
    var startedAt: Int64

    /// `STALE_AFTER_SECS`: a row still claiming a recovery past this was left
    /// by an engine that stopped mid-recovery, and shows nothing.
    static let staleAfterMs: Int64 = 5 * 60 * 1000

    func isLive(now: Int64) -> Bool {
        now - startedAt < Self.staleAfterMs
    }

    /// recovery.rs `recovery_trailer_text`: the strip's word and its faint
    /// tail (elapsed time, the matched phrase, the attempt).
    func trailerText(now: Int64) -> (word: String, tail: String) {
        let word =
            switch stage {
            case .writingContext: "Writing context to retry"
            case .restarting: "Restarting with added context"
            }
        var tail = "\(Motion.formatElapsed(max(0, (now - startedAt) / 1000))) · matched \u{201c}\(trigger)\u{201d}"
        if attempt > 1 { tail += " · attempt \(attempt)" }
        return (word, tail)
    }

    init(
        stage: RecoveryStage, trigger: String, matchedIn: RecoveryTriggerSource,
        attempt: UInt32, startedAt: Int64
    ) {
        self.stage = stage
        self.trigger = trigger
        self.matchedIn = matchedIn
        self.attempt = attempt
        self.startedAt = startedAt
    }

    /// rows.rs: lenient — a malformed value drops the recovery, never the
    /// row. `startedAt` is chrono's RFC 3339 (`…T21:39:44.123456789Z`).
    init?(_ value: JSONValue?) {
        guard let object = value?.objectValue,
            let stage = object["stage"]?.stringValue.flatMap(RecoveryStage.init(rawValue:)),
            let trigger = object["trigger"]?.stringValue,
            let matchedIn = object["matchedIn"]?.stringValue.flatMap(RecoveryTriggerSource.init(rawValue:)),
            let attempt = object["attempt"]?.int64Value, attempt >= 0, attempt <= Int64(UInt32.max),
            let startedAt = object["startedAt"]?.stringValue.flatMap(Self.rfc3339Ms)
        else { return nil }
        self.init(
            stage: stage, trigger: trigger, matchedIn: matchedIn,
            attempt: UInt32(attempt), startedAt: startedAt)
    }

    /// RFC 3339 to Unix ms. Foundation's ISO 8601 parser takes at most
    /// millisecond fractions, and chrono writes up to nanoseconds, so the
    /// fraction is read here.
    static func rfc3339Ms(_ text: String) -> Int64? {
        var text = text
        var fraction = 0.0
        if let dot = text.firstIndex(of: ".") {
            let digits = text[text.index(after: dot)...].prefix { $0.isASCII && $0.isNumber }
            guard !digits.isEmpty else { return nil }
            fraction = Double("0." + digits) ?? 0
            text.removeSubrange(dot..<text.index(dot, offsetBy: digits.count + 1))
        }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        guard let date = formatter.date(from: text) else { return nil }
        return Int64(((date.timeIntervalSince1970 + fraction) * 1000).rounded(.down))
    }
}

// MARK: - Run progress (proto entities.rs `RunProgress`)

/// `ProgressStage`: what the agent was doing, and how far the turn had got
/// (0–100) when that stage began.
struct ProgressStage: Hashable {
    var label: String
    var percent: Int
}

/// `RunProgress`: a cheap model's reading of how far a long turn has got. An
/// estimate for the user's eyes only — nothing drives off it.
struct RunProgress: Hashable {
    /// 0–100.
    var percent: Int
    /// What the agent is doing now, one short line.
    var current: String
    /// The steps still to come after `current`, in order (may be empty).
    var next: [String] = []
    /// How many steps remain when the model counts more than it lists.
    var stepsLeft: Int? = nil
    /// The stages the turn has been through, oldest first; the last one is
    /// `current`.
    var stages: [ProgressStage] = []

    /// progress_bar.rs `MAX_UPCOMING`.
    static let maxUpcoming = 12

    /// progress.rs `steps_left`: the listed steps, or the model's count when
    /// it names more.
    var stepsLeftCount: Int { max(stepsLeft ?? 0, next.count) }

    /// progress.rs `shown_percent`: the reading's, but never behind where a
    /// stage began (stages never move once recorded).
    var shownPercent: Int {
        min(stages.map(\.percent).reduce(percent, max), 100)
    }

    /// The stages the bar marks: a host that keeps none still shows its
    /// current one.
    var shownStages: [ProgressStage] {
        stages.isEmpty ? [ProgressStage(label: current, percent: percent)] : stages
    }

    /// progress.rs `upcoming_positions`: where each upcoming step begins,
    /// 0–1 — the stretch past the fill shared evenly between the current
    /// step and those after it.
    static func upcomingPositions(percent: Int, count: Int) -> [Double] {
        let from = Double(min(max(percent, 0), 100)) / 100
        let shown = min(count, maxUpcoming)
        let share = (1 - from) / Double(shown + 1)
        return shown == 0 ? [] : (1...shown).map { from + share * Double($0) }
    }

    /// progress.rs `left_note`: "3 steps left", for the current step.
    static func leftNote(_ left: Int) -> String {
        switch left {
        case 0: "last step"
        case 1: "1 step left"
        default: "\(left) steps left"
        }
    }

    /// "42% · 3 steps left".
    var note: String { "\(shownPercent)% \u{b7} \(Self.leftNote(stepsLeftCount))" }

    init(
        percent: Int, current: String, next: [String] = [], stepsLeft: Int? = nil,
        stages: [ProgressStage] = []
    ) {
        self.percent = percent
        self.current = current
        self.next = next
        self.stepsLeft = stepsLeft
        self.stages = stages
    }

    /// rows.rs: lenient — a malformed value drops the reading, never the row.
    init?(_ value: JSONValue?) {
        guard let object = value?.objectValue,
            let percent = object["percent"]?.int64Value, (0...255).contains(percent),
            let current = object["current"]?.stringValue
        else { return nil }
        var stages: [ProgressStage] = []
        for stage in object["stages"]?.arrayValue ?? [] {
            guard let stage = stage.objectValue,
                let label = stage["label"]?.stringValue,
                let percent = stage["percent"]?.int64Value, (0...255).contains(percent)
            else { return nil }
            stages.append(ProgressStage(label: label, percent: Int(percent)))
        }
        let next = (object["next"]?.arrayValue ?? []).compactMap(\.stringValue)
        let stepsLeft = object["stepsLeft"]?.int64Value.map { Int(min(max($0, 0), Int64(UInt16.max))) }
        self.init(
            percent: Int(percent), current: current, next: next, stepsLeft: stepsLeft,
            stages: stages)
    }
}

/// proto agent.rs `AnsweredModel`: a model that answered (part of) a turn,
/// and the one that was asked for when the host recorded it.
struct AnsweredModel: Hashable {
    var model: String
    var requested: String?

    /// A different model answered than the one requested. Spellings of the
    /// same model don't count (agent.rs `substituted`).
    var substituted: Bool {
        requested.map { Self.family($0) != Self.family(model) } ?? false
    }

    /// agent.rs `model_family`: the id without the decorations that name the
    /// same model — provider prefix, `[1m]`, a dated snapshot
    /// (`-20261001`, `@20261001`, `-2026-10-01`), `-latest`, and case.
    static func family(_ id: String) -> String {
        var id = Substring(id.trimmingCharacters(in: .whitespaces).lowercased())
        if let slash = id.lastIndex(of: "/") { id = id[id.index(after: slash)...] }
        if id.hasSuffix("]"), let open = id.lastIndex(of: "[") { id = id[..<open] }
        let digits = { (s: Substring) in !s.isEmpty && s.allSatisfy(\.isASCIIDigit) }
        if let at = id.lastIndex(where: { $0 == "-" || $0 == "@" }),
            id.distance(from: at, to: id.endIndex) == 9,
            digits(id[id.index(after: at)...])
        {
            id = id[..<at]
        }
        if id.count > 11 {
            let date = Array(id.suffix(11))
            let numeric = [date[1..<5], date[6..<8], date[9...]].allSatisfy { $0.allSatisfy(\.isASCIIDigit) }
            if date[0] == "-", date[5] == "-", date[8] == "-", numeric {
                id = id.dropLast(11)
            }
        }
        if id.hasSuffix("-latest") { id = id.dropLast(7) }
        return String(id)
    }
}

extension Character {
    fileprivate var isASCIIDigit: Bool { isASCII && isNumber }
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
    private struct Row: Decodable {
        var id: String
        var title: String?
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "created":
            let row = try c.decode(Row.self, forKey: .chat)
            self = .created(
                chatId: row.id, title: row.title,
                composerText: try c.decodeIfPresent(String.self, forKey: .composerText))
        default:
            self = .unavailable(
                message: try c.decodeIfPresent(String.self, forKey: .message)
                    ?? "This session can't be forked here.")
        }
    }
}

/// proto session_fork.rs `SessionRewindResponse`, tagged by `kind`.
enum RewindResponse: Decodable, Equatable {
    /// `composerText`: a rewind at a user message hands its text back.
    case rewound(composerText: String?, removed: Int)
    case unavailable(message: String)

    private enum Keys: String, CodingKey { case kind, composerText, removedMessageIds, message }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "rewound":
            self = .rewound(
                composerText: try c.decodeIfPresent(String.self, forKey: .composerText),
                removed: (try c.decodeIfPresent([String].self, forKey: .removedMessageIds))?.count ?? 0)
        default:
            self = .unavailable(
                message: try c.decodeIfPresent(String.self, forKey: .message)
                    ?? "This conversation can't be restarted here.")
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

func nowMs() -> Int64 {
    Int64(Date().timeIntervalSince1970 * 1000)
}

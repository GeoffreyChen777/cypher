// The records behind a workspace's board (crates/proto/src/bot_records.rs):
// plans and their orchestrations, routines, shortcuts, the activity log and
// approvals. Each is a registry row of its own kind, LWW per field: the
// host engine writes the state it owns, and any owner device writes the
// small control fields a person sets (a plan's `decision`, a work item's
// `stopRequestedAt`, a routine's definition). Times are epoch ms.

import Foundation

/// bot_records.rs caps: every text field is clipped before it is written,
/// because the registry refuses an op over 16 KiB.
enum BotCaps {
    static let title = 120
    static let line = 300
    static let stepTitle = 160
    static let detail = 400
    static let instruction = 1_500
    static let result = 2_000
    static let prompt = 4_000
    static let answer = 8_000
    static let planSteps = 10
}

/// `text` trimmed to at most `max` characters, an ellipsis marking a cut
/// (bot_records.rs `clip`).
func clip(_ text: String, max limit: Int) -> String {
    let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if text.count <= limit { return text }
    return String(text.prefix(Swift.max(limit - 1, 0))) + "…"
}

// MARK: - Plans and orchestrations

enum WorkState: String, Codable, Hashable, Sendable {
    case proposed, running, done, failed, stopped, cancelled
}

enum StepState: String, Codable, Hashable, Sendable {
    case pending, running
    /// A session it started is waiting on an answer.
    case asking
    case done, failed, skipped
}

/// An owner's approve-or-cancel (plans) or approve-or-decline (approvals).
struct PlanDecision: Codable, Hashable, Sendable {
    var approve: Bool
    var at: Int64
}

/// One step of a plan.
struct WorkStep: Hashable, Identifiable, Sendable {
    var id: String
    var title: String
    /// The project (space id) it works in, when it works in one.
    var project: String?
    var detail: String?
    var state: StepState = .pending
    /// Sessions this step started.
    var sessions: [String] = []
    var note: String?
    var startedAt: Int64?
    var endedAt: Int64?
    /// The model, thinking and Fast mode of the sessions it starts.
    var sessionChoice = SessionChoice()
}

extension WorkStep: Codable {
    private enum CodingKeys: String, CodingKey {
        case id, title, project, detail, state, sessions, note, startedAt, endedAt, sessionChoice
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        project = try c.decodeIfPresent(String.self, forKey: .project)
        detail = try c.decodeIfPresent(String.self, forKey: .detail)
        state = try c.decode(.state, or: .pending)
        sessions = try c.decode(.sessions, or: [])
        note = try c.decodeIfPresent(String.self, forKey: .note)
        startedAt = try c.decodeIfPresent(Int64.self, forKey: .startedAt)
        endedAt = try c.decodeIfPresent(Int64.self, forKey: .endedAt)
        sessionChoice = ((try? c.decodeIfPresent(SessionChoice.self, forKey: .sessionChoice)) ?? nil) ?? SessionChoice()
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(title, forKey: .title)
        try c.encodeIfPresent(project, forKey: .project)
        try c.encodeIfPresent(detail, forKey: .detail)
        try c.encode(state, forKey: .state)
        if !sessions.isEmpty { try c.encode(sessions, forKey: .sessions) }
        try c.encodeIfPresent(note, forKey: .note)
        try c.encodeIfPresent(startedAt, forKey: .startedAt)
        try c.encodeIfPresent(endedAt, forKey: .endedAt)
        if !sessionChoice.isEmpty { try c.encode(sessionChoice, forKey: .sessionChoice) }
    }
}

/// One plan, from proposal through its orchestration to the result
/// (registry kind `botWork`).
struct BotWork: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var title: String
    /// The owner's instruction, verbatim: the orchestration's brief.
    var instruction = ""
    var summary: String?
    var steps: [WorkStep] = []
    var state: WorkState = .proposed
    /// The chat the plan was proposed in (its card lives there).
    var chatId: String
    /// The `propose_plan` tool call that draws the card.
    var toolCallId: String?
    /// The orchestration thread, once approved.
    var threadChatId: String?
    var createdAt: Int64
    var startedAt: Int64?
    var endedAt: Int64?
    /// Markdown, once it ended.
    var result: String?
    /// Owner-written: approve or cancel the proposal.
    var decision: PlanDecision?
    /// Owner-written: stop the orchestration and its sessions.
    var stopRequestedAt: Int64?
    /// Minutes it may run, set when it was proposed; nil runs on the
    /// workspace's.
    var timeLimitMinutes: Int?
    /// Owner-written: minutes added to the limit while it runs, in all.
    var timeAddedMinutes: Int?
    /// The coordinating session's model, thinking and Fast mode.
    var coordinatorChoice = SessionChoice()
    /// Set when the plan's text was cut to fit the row: the rev of its full
    /// text in the workspace chat doc's `planBodies[id].plan` (PlanText.swift).
    /// nil: the row is the whole plan.
    var bodyRev: String?
    /// The same for `result` (`planBodies[id].result`).
    var resultRev: String?
    /// Host-written: sessions the orchestration steered but didn't start,
    /// oldest first. Their questions are the plan's too (`planChatIds`),
    /// but they are not `sessionIds`: steering one again needs the owner.
    var steered: [String] = []

    var isLive: Bool { state == .proposed || state == .running }

    /// Its whole limit: its own (else `fallback`, the workspace's) plus
    /// what the owner added (bot_records.rs `limit_minutes`).
    func limitMinutes(fallback: Int) -> Int {
        let own = timeLimitMinutes.map { PlanSettings.clamp($0, PlanSettings.timeLimitRange) } ?? fallback
        return own + min(timeAddedMinutes ?? 0, PlanSettings.maxAddedMinutes)
    }

    /// When it is stopped for time, once it has started.
    func deadlineMs(fallback: Int) -> Int64? {
        startedAt.map { $0 + Int64(limitMinutes(fallback: fallback)) * 60_000 }
    }

    /// Its own limit, when it differs from the workspace's.
    func ownLimit(workspace: Int) -> Int? {
        timeLimitMinutes.map { PlanSettings.clamp($0, PlanSettings.timeLimitRange) }.flatMap {
            $0 == workspace ? nil : $0
        }
    }

    /// Awaiting the owner's approval: proposed, and nobody decided yet.
    var awaitingApproval: Bool { state == .proposed && decision == nil }

    /// Every session the orchestration started, in step order.
    var sessionIds: [String] { steps.flatMap(\.sessions) }

    /// Every chat whose question is the plan's (bot_records.rs
    /// `plan_chat_ids`): the sessions it started, then the ones it steered,
    /// each once. Child chats under them count too (`ChatTree.question`).
    var planChatIds: [String] {
        var seen = Set<String>()
        return (sessionIds + steered).filter { seen.insert($0).inserted }
    }

    /// The steps' states with `asking(chatId)` applied (bot_records.rs
    /// `settled_steps`): a running step whose sessions (or, for the current
    /// step, a steered session) wait on an answer is Asking, and back to
    /// Running once none does. Every other state stays.
    func settledSteps(asking: (String) -> Bool) -> [StepState] {
        let current = steps.firstIndex { $0.state == .running || $0.state == .asking }
        let steeredAsks = steered.contains(where: asking)
        return steps.enumerated().map { index, step in
            let waiting = step.sessions.contains(where: asking) || (steeredAsks && current == index)
            switch step.state {
            case .running where waiting: return .asking
            case .asking where !waiting: return .running
            default: return step.state
            }
        }
    }

    /// Steps done (or skipped) out of all, for a progress bar.
    var progress: (done: Int, total: Int) {
        (steps.filter { $0.state == .done || $0.state == .skipped }.count, steps.count)
    }

    /// The step running now (or asking), else the first pending one.
    var currentStep: (index: Int, step: WorkStep)? {
        if let ix = steps.firstIndex(where: { $0.state == .running || $0.state == .asking }) {
            return (ix, steps[ix])
        }
        if let ix = steps.firstIndex(where: { $0.state == .pending }) {
            return (ix, steps[ix])
        }
        return nil
    }

    /// How an open step reads once its plan has ended (bot_records.rs
    /// `ended_step_state`): what was under way ended with the plan (done, or
    /// failed), what never started was skipped, and a stopped plan skipped
    /// the rest. The steps of a live plan, of a cancelled one and steps
    /// already closed read as they are.
    func endedStepState(_ step: StepState) -> StepState {
        guard step == .pending || step == .running || step == .asking else { return step }
        switch (state, step) {
        case (.proposed, _), (.running, _), (.cancelled, _): return step
        case (.done, .running), (.done, .asking): return .done
        case (.failed, .running), (.failed, .asking): return .failed
        default: return .skipped
        }
    }

    /// Close the steps an ended plan left open (bot_records.rs
    /// `close_steps`). Its host does this in the row; a row from an older
    /// host, which didn't, would show a step running, its spinner turning and
    /// its timer counting, under a finished plan for good.
    mutating func closeSteps() {
        for ix in steps.indices {
            let closed = endedStepState(steps[ix].state)
            guard closed != steps[ix].state else { continue }
            steps[ix].state = closed
            if steps[ix].endedAt == nil { steps[ix].endedAt = endedAt }
        }
    }
}

extension BotWork {
    private enum CodingKeys: String, CodingKey {
        case id, botId, title, instruction, summary, steps, state, chatId, toolCallId, threadChatId
        case createdAt, startedAt, endedAt, result, decision, stopRequestedAt
        case timeLimitMinutes, timeAddedMinutes, coordinatorChoice, bodyRev, resultRev, steered
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        title = try c.decode(String.self, forKey: .title)
        instruction = try c.decode(.instruction, or: "")
        summary = try c.decodeIfPresent(String.self, forKey: .summary)
        steps = try c.decode(.steps, or: [])
        state = try c.decode(.state, or: .proposed)
        chatId = try c.decode(String.self, forKey: .chatId)
        toolCallId = try c.decodeIfPresent(String.self, forKey: .toolCallId)
        threadChatId = try c.decodeIfPresent(String.self, forKey: .threadChatId)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        startedAt = try c.decodeIfPresent(Int64.self, forKey: .startedAt)
        endedAt = try c.decodeIfPresent(Int64.self, forKey: .endedAt)
        result = try c.decodeIfPresent(String.self, forKey: .result)
        decision = try c.decodeIfPresent(PlanDecision.self, forKey: .decision)
        stopRequestedAt = try c.decodeIfPresent(Int64.self, forKey: .stopRequestedAt)
        timeLimitMinutes = (try? c.decodeIfPresent(Int.self, forKey: .timeLimitMinutes)) ?? nil
        timeAddedMinutes = (try? c.decodeIfPresent(Int.self, forKey: .timeAddedMinutes)) ?? nil
        coordinatorChoice =
            ((try? c.decodeIfPresent(SessionChoice.self, forKey: .coordinatorChoice)) ?? nil) ?? SessionChoice()
        bodyRev = try c.decodeIfPresent(String.self, forKey: .bodyRev)
        resultRev = try c.decodeIfPresent(String.self, forKey: .resultRev)
        steered = (try? c.decodeIfPresent([String].self, forKey: .steered)) ?? []
        closeSteps()
    }
}

// MARK: - Schedules and routines

/// When a routine runs, in the host device's local time (tag `kind`).
enum Schedule: Hashable, Sendable {
    /// At `time` ("HH:MM") on `days` (1 = Monday … 7 = Sunday; empty =
    /// every day).
    case daily(time: String, days: [Int])
    /// Every `minutes` (at least `minInterval`).
    case interval(minutes: Int)
    /// Only when run by hand.
    case manual
    /// Once, at `at` ("YYYY-MM-DDTHH:MM", local time): a reminder.
    case once(at: String)

    static let minInterval = 15
    static let `default` = Schedule.daily(time: "09:00", days: [])

    /// "Daily 09:00", "Weekdays 18:30", "Every 2 h", "Manual" (bot_records.rs
    /// `Schedule::describe`).
    var describe: String {
        switch self {
        case .manual:
            return "Manual"
        case .once(let at):
            guard let date = Self.parseOnce(at) else { return "Once" }
            return "Once, \(Self.onceLabel.string(from: date))"
        case .interval(let minutes) where minutes % 60 == 0:
            let hours = minutes / 60
            return hours == 1 ? "Every hour" : "Every \(hours) h"
        case .interval(let minutes):
            return "Every \(minutes) min"
        case .daily(let time, let days):
            let sorted = Array(Set(days)).sorted()
            let when: String
            switch sorted {
            case [], [1, 2, 3, 4, 5, 6, 7]: when = "Daily"
            case [1, 2, 3, 4, 5]: when = "Weekdays"
            case [6, 7]: when = "Weekends"
            default:
                let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                when = sorted.map { names[Swift.min(Swift.max($0, 1), 7) - 1] }.joined(separator: ", ")
            }
            return "\(when) \(time)"
        }
    }

    /// `Schedule::validate`; nil when the engine accepts it.
    var validationError: String? {
        switch self {
        case .daily(let time, let days):
            if Self.parseTime(time) == nil { return "Pick a time like 09:00." }
            if days.contains(where: { !(1...7).contains($0) }) { return "Days run from 1 (Monday) to 7 (Sunday)." }
            return nil
        case .interval(let minutes) where minutes < Self.minInterval:
            return "Run at most every \(Self.minInterval) minutes."
        case .interval, .manual:
            return nil
        case .once(let at):
            return Self.parseOnce(at) == nil ? "Pick a date and time like 2026-11-10T09:00." : nil
        }
    }

    /// A once-only schedule's local date and time.
    static func parseOnce(_ text: String) -> Date? {
        onceFormat.date(from: text)
    }

    /// "HH:MM" as (hour, minute), strictly two-digit fields.
    static func parseTime(_ text: String) -> (hour: Int, minute: Int)? {
        let parts = text.split(separator: ":", omittingEmptySubsequences: false)
        guard parts.count == 2, parts.allSatisfy({ (1...2).contains($0.count) && $0.allSatisfy(\.isASCII) }),
            let hour = Int(parts[0]), let minute = Int(parts[1]),
            (0...23).contains(hour), (0...59).contains(minute)
        else { return nil }
        return (hour, minute)
    }

    private static let onceFormat: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm"
        return f
    }()

    private static let onceLabel: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "EEE MMM d HH:mm"
        return f
    }()
}

extension Schedule: Codable {
    private enum CodingKeys: String, CodingKey { case kind, time, days, minutes, at }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "daily":
            self = .daily(time: try c.decode(String.self, forKey: .time), days: try c.decode(.days, or: []))
        case "interval": self = .interval(minutes: try c.decode(Int.self, forKey: .minutes))
        case "manual": self = .manual
        case "once": self = .once(at: try c.decode(String.self, forKey: .at))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown schedule \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .daily(let time, let days):
            try c.encode("daily", forKey: .kind)
            try c.encode(time, forKey: .time)
            if !days.isEmpty { try c.encode(days, forKey: .days) }
        case .interval(let minutes):
            try c.encode("interval", forKey: .kind)
            try c.encode(minutes, forKey: .minutes)
        case .manual:
            try c.encode("manual", forKey: .kind)
        case .once(let at):
            try c.encode("once", forKey: .kind)
            try c.encode(at, forKey: .at)
        }
    }
}

enum RunOutcome: String, Codable, Hashable, Sendable {
    case running, done, failed
    /// The workspace was paused, or can no longer run it there.
    case skipped
}

/// One run of a routine.
struct RunRecord: Hashable, Sendable {
    var at: Int64
    var endedAt: Int64?
    var outcome: RunOutcome
    var summary: String?
    /// The thread or session the run happened in.
    var chatId: String?
    /// Started by hand (Run now) in a fresh session.
    var manual = false
}

extension RunRecord: Codable {
    private enum CodingKeys: String, CodingKey { case at, endedAt, outcome, summary, chatId, manual }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        at = try c.decode(Int64.self, forKey: .at)
        endedAt = try c.decodeIfPresent(Int64.self, forKey: .endedAt)
        outcome = try c.decode(RunOutcome.self, forKey: .outcome)
        summary = try c.decodeIfPresent(String.self, forKey: .summary)
        chatId = try c.decodeIfPresent(String.self, forKey: .chatId)
        manual = try c.decode(.manual, or: false)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(at, forKey: .at)
        try c.encodeIfPresent(endedAt, forKey: .endedAt)
        try c.encode(outcome, forKey: .outcome)
        try c.encodeIfPresent(summary, forKey: .summary)
        try c.encodeIfPresent(chatId, forKey: .chatId)
        if manual { try c.encode(manual, forKey: .manual) }
    }
}

/// Who posted a message into a workspace chat outside any turn (the doc's
/// `post` message part, tag `kind`): a routine's output, or how a plan
/// ended (bot_records.rs `BotPost`).
enum BotPost: Hashable, Sendable {
    case routine(routineId: String, name: String, outcome: RunOutcome, runAt: Int64, threadChatId: String?)
    /// A plan's end: `outcome` is done, failed or stopped; the post's text
    /// is its result.
    case work(workId: String, title: String, outcome: WorkState, startedAt: Int64?, endedAt: Int64)
    /// A source from a newer engine: shown as plain text.
    case unknown

    /// The id prefix of the turn a host starts in a workspace chat to tell
    /// its agent that a job ended (bot_records.rs `BOT_EVENT_PREFIX`): the
    /// post before it is what the owner sees, so its own message draws
    /// nothing.
    static let eventPrefix = "bot-event-"
}

extension BotPost: Codable {
    private enum CodingKeys: String, CodingKey {
        case kind, routineId, name, outcome, runAt, threadChatId, workId, title, startedAt, endedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "routine":
            self = .routine(
                routineId: try c.decode(String.self, forKey: .routineId),
                name: try c.decode(String.self, forKey: .name),
                outcome: try c.decode(RunOutcome.self, forKey: .outcome),
                runAt: try c.decode(Int64.self, forKey: .runAt),
                threadChatId: try c.decodeIfPresent(String.self, forKey: .threadChatId))
        case "work":
            self = .work(
                workId: try c.decode(String.self, forKey: .workId),
                title: try c.decode(String.self, forKey: .title),
                outcome: try c.decode(WorkState.self, forKey: .outcome),
                startedAt: try c.decodeIfPresent(Int64.self, forKey: .startedAt),
                endedAt: try c.decode(Int64.self, forKey: .endedAt))
        default:
            self = .unknown
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .routine(let routineId, let name, let outcome, let runAt, let threadChatId):
            try c.encode("routine", forKey: .kind)
            try c.encode(routineId, forKey: .routineId)
            try c.encode(name, forKey: .name)
            try c.encode(outcome, forKey: .outcome)
            try c.encode(runAt, forKey: .runAt)
            try c.encodeIfPresent(threadChatId, forKey: .threadChatId)
        case .work(let workId, let title, let outcome, let startedAt, let endedAt):
            try c.encode("work", forKey: .kind)
            try c.encode(workId, forKey: .workId)
            try c.encode(title, forKey: .title)
            try c.encode(outcome, forKey: .outcome)
            try c.encodeIfPresent(startedAt, forKey: .startedAt)
            try c.encode(endedAt, forKey: .endedAt)
        case .unknown:
            try c.encode("unknown", forKey: .kind)
        }
    }
}

/// Where a routine runs and what it can reach (tag `kind`).
enum RoutineTarget: Hashable, Sendable {
    /// The workspace's own agent, in a hidden thread; it posts its output in
    /// the workspace chat.
    case workspace
    /// An ordinary coding session in one of the workspace's projects.
    /// Needs *Coordinate*; `worktree` gives it its own worktree.
    case project(spaceId: String, worktree: Bool, model: String?)

    var spaceId: String? {
        if case .project(let spaceId, _, _) = self { return spaceId }
        return nil
    }
}

extension RoutineTarget: Codable {
    private enum CodingKeys: String, CodingKey { case kind, spaceId, worktree, model }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "workspace": self = .workspace
        case "project":
            self = .project(
                spaceId: try c.decode(String.self, forKey: .spaceId),
                worktree: try c.decode(.worktree, or: false),
                model: try c.decodeIfPresent(String.self, forKey: .model))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown target \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .workspace:
            try c.encode("workspace", forKey: .kind)
        case .project(let spaceId, let worktree, let model):
            try c.encode("project", forKey: .kind)
            try c.encode(spaceId, forKey: .spaceId)
            try c.encode(worktree, forKey: .worktree)
            try c.encodeIfPresent(model, forKey: .model)
        }
    }
}

/// A workspace job run by hand or on a schedule, in the background (registry kind
/// `botRoutines`). Scheduled runs reuse one standing session
/// (`threadChatId`); each Run now gets a fresh one (`manualChatId`).
struct BotRoutine: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var name: String
    var prompt: String
    var target: RoutineTarget = .workspace
    /// The model its runs use; nil follows the workspace chat's.
    var model: String?
    /// Its runs' thinking level and Fast mode; nil follows the default.
    var thinking: String?
    var fast: Bool?
    var schedule: Schedule = .default
    var enabled = true
    /// May write to memory (a workspace routine's).
    var writeNotes = false
    /// May use Pi's shell and file tools, like an owner thread with *Work
    /// directly*.
    var workDirectly = false
    var createdAt: Int64
    var updatedAt: Int64
    /// Owner-written: run once now.
    var runRequestedAt: Int64?
    /// Owner-written with a Run now: extra instructions for that run only.
    var runNote: RunNote?
    // Host-written:
    var threadChatId: String?
    var manualChatId: String?
    var worktreePath: String?
    var lastRun: RunRecord?
    var nextRunAt: Int64?

    /// Its runs' model, thinking and Fast mode, as far as it names them.
    var sessionChoice: SessionChoice {
        get { SessionChoice(model: chosenModel, thinking: thinking, fast: fast) }
        set {
            model = newValue.model
            thinking = newValue.thinking
            fast = newValue.fast
        }
    }

    /// The model its runs should use, when it names one.
    var chosenModel: String? {
        if let model { return model }
        if case .project(_, _, let model) = target { return model }
        return nil
    }

    /// `BotRoutine::validate`; nil when the engine accepts it.
    var validationError: String? {
        if name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "Name the routine." }
        if prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "Say what the routine should do."
        }
        if let spaceId = target.spaceId, spaceId.trimmingCharacters(in: .whitespaces).isEmpty {
            return "Pick the project it runs in."
        }
        return schedule.validationError
    }
}

extension BotRoutine {
    private enum CodingKeys: String, CodingKey {
        case id, botId, name, prompt, target, model, thinking, fast, schedule, enabled, writeNotes, workDirectly
        case createdAt, updatedAt, runRequestedAt, runNote, threadChatId, manualChatId, worktreePath, lastRun
        case nextRunAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        name = try c.decode(String.self, forKey: .name)
        prompt = try c.decode(String.self, forKey: .prompt)
        target = try c.decode(.target, or: .workspace)
        model = try c.decodeIfPresent(String.self, forKey: .model)
        thinking = (try? c.decodeIfPresent(String.self, forKey: .thinking)) ?? nil
        fast = (try? c.decodeIfPresent(Bool.self, forKey: .fast)) ?? nil
        schedule = try c.decode(.schedule, or: .default)
        enabled = try c.decode(.enabled, or: true)
        writeNotes = try c.decode(.writeNotes, or: false)
        workDirectly = try c.decode(.workDirectly, or: false)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        updatedAt = try c.decode(Int64.self, forKey: .updatedAt)
        runRequestedAt = try c.decodeIfPresent(Int64.self, forKey: .runRequestedAt)
        runNote = (try? c.decodeIfPresent(RunNote.self, forKey: .runNote)) ?? nil
        threadChatId = try c.decodeIfPresent(String.self, forKey: .threadChatId)
        manualChatId = try c.decodeIfPresent(String.self, forKey: .manualChatId)
        worktreePath = try c.decodeIfPresent(String.self, forKey: .worktreePath)
        lastRun = try c.decodeIfPresent(RunRecord.self, forKey: .lastRun)
        nextRunAt = try c.decodeIfPresent(Int64.self, forKey: .nextRunAt)
    }
}

/// A Run now's extra instructions, for the request at `at` alone.
struct RunNote: Codable, Hashable, Sendable {
    var at: Int64
    var text: String

    static let max = 2_000
}

// MARK: - Shortcuts

/// A button under a workspace's chat: a request the owner makes often, sent
/// to the agent again on each tap (registry kind `botShortcuts`). Only the
/// agent makes and changes shortcuts; owner devices order and delete them.
struct BotShortcut: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var label: String
    /// One of `icons`; the app picks one when absent.
    var icon: String?
    /// What a tap sends; may hold `input` blanks.
    var request: String
    /// Position among the workspace's shortcuts, lowest first.
    var order: Int64 = 0
    var createdAt: Int64
    var updatedAt: Int64

    static let labelMax = 32
    /// The blank a request may hold: a tap asks for it first.
    static let input = "{input}"
    /// The icons a shortcut may name (bot_records.rs `SHORTCUT_ICONS`).
    static let icons = [
        "bolt", "star", "rocket", "play", "chat", "checklist", "code", "terminal",
        "bug", "test-tube", "document", "global", "calendar", "cup-hot", "magic-stick", "flag",
    ]

    /// Whether a tap asks for text first.
    var wantsInput: Bool { request.contains(Self.input) }

    /// Punctuation that sits against the word before it.
    private static let tight: Set<Character> = [",", ".", ":", ";", "!", "?", "，", "。", "：", "；", "！", "？"]

    /// The request a tap sends, with `input` in every blank. Left empty, the
    /// blanks are dropped and the text around each tidied (see
    /// `join(_:overBlankWith:)`); the rest of the request stays as written.
    /// The same as bot_records.rs `BotShortcut::request_with`.
    func request(with input: String) -> String {
        let input = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard input.isEmpty, wantsInput else {
            return request.replacingOccurrences(of: Self.input, with: input)
        }
        let pieces = request.components(separatedBy: Self.input)
        var out = pieces[0]
        for piece in pieces.dropFirst() { Self.join(&out, overBlankWith: piece) }
        return out.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Appends `next` to `out` where an empty blank was: the spaces and tabs
    /// on both sides become one space (none before `tight` punctuation or at
    /// a line's edge, none when neither side had any); a blank that started
    /// the text or a line, or followed punctuation, takes the punctuation
    /// after it along ("{input}, please review" → "please review"); a blank
    /// alone on its line takes the line. Line breaks elsewhere stay.
    private static func join(_ out: inout String, overBlankWith next: String) {
        let gap: (Character) -> Bool = { $0 == " " || $0 == "\t" }
        let spaced = out.last.map(gap) == true || next.first.map(gap) == true
        while let last = out.last, gap(last) { out.removeLast() }
        var next = Substring(next).drop(while: gap)
        if out.isEmpty || out.last == "\n" || out.last.map(tight.contains) == true {
            next = next.drop(while: tight.contains).drop(while: gap)
        }
        if out.last == "\n", next.first == "\n" { next = next.dropFirst() }
        if spaced, let last = out.last, last != "\n", let first = next.first, first != "\n",
            !tight.contains(first)
        {
            out.append(" ")
        }
        out += next
    }
}

extension BotShortcut {
    private enum CodingKeys: String, CodingKey {
        case id, botId, label, icon, request, order, createdAt, updatedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        label = try c.decode(String.self, forKey: .label)
        icon = try c.decodeIfPresent(String.self, forKey: .icon)
        request = try c.decode(String.self, forKey: .request)
        order = try c.decode(.order, or: 0)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        updatedAt = try c.decode(Int64.self, forKey: .updatedAt)
    }
}

// MARK: - Activity log

/// The Activity page's filter groups. The engine calls the fourth one
/// Notes and the fifth Network; the app names them as users see them.
enum ActivityGroup: String, CaseIterable, Hashable, Sendable {
    case plans, sessions, routines, memory, connections

    var label: String {
        switch self {
        case .plans: "Plans"
        case .sessions: "Sessions"
        case .routines: "Routines"
        case .memory: "Memory"
        case .connections: "Connections"
        }
    }
}

enum ActivityKind: String, Hashable, Sendable {
    case planProposed, planApproved, planCancelled, workFinished, workFailed, workStopped
    case sessionStarted, sessionSteered, sessionAnswered, sessionStopped, worktreeCreated
    case noteWritten, routineRan, taskRan, approvalAsked
    case peerAsked, peerAnswered, connectionAdded, connectionRevoked
    /// A kind from a newer engine.
    case other

    var group: ActivityGroup {
        switch self {
        case .planProposed, .planApproved, .planCancelled, .workFinished, .workFailed, .workStopped,
            .approvalAsked, .other:
            return .plans
        case .sessionStarted, .sessionSteered, .sessionAnswered, .sessionStopped, .worktreeCreated:
            return .sessions
        case .routineRan, .taskRan:
            return .routines
        case .noteWritten:
            return .memory
        case .peerAsked, .peerAnswered, .connectionAdded, .connectionRevoked:
            return .connections
        }
    }
}

extension ActivityKind: Codable {
    init(from decoder: Decoder) throws {
        self = ActivityKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .other
    }
}

/// One line of a workspace's audit log (registry kind `botActivity`).
struct BotActivity: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var at: Int64
    var kind: ActivityKind
    var text: String
    var chatId: String?
    var workId: String?
    var spaceId: String?
}

// MARK: - Approvals

enum ApprovalState: String, Codable, Hashable, Sendable {
    case pending, approved, declined, expired
}

/// An action a thread may only take once the owner approves it (registry
/// kind `botApprovals`): the taint rule, or steering another's session.
struct BotApproval: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var threadChatId: String
    var workId: String?
    /// The action in plain words ("Start a session in web: …").
    var action: String
    /// Why it needs approval.
    var reason: String
    var createdAt: Int64
    var state: ApprovalState = .pending
    /// Owner-written.
    var decision: PlanDecision?

    var awaiting: Bool { state == .pending && decision == nil }
}

extension BotApproval {
    private enum CodingKeys: String, CodingKey {
        case id, botId, threadChatId, workId, action, reason, createdAt, state, decision
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        threadChatId = try c.decode(String.self, forKey: .threadChatId)
        workId = try c.decodeIfPresent(String.self, forKey: .workId)
        action = try c.decode(String.self, forKey: .action)
        reason = try c.decode(String.self, forKey: .reason)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        state = try c.decode(.state, or: .pending)
        decision = try c.decodeIfPresent(PlanDecision.self, forKey: .decision)
    }
}

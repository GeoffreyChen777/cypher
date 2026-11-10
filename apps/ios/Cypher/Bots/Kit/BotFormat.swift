// Words for workspace times and states, in the desktop's wording
// (crates/ui/src/shell/bots/parts.rs time labels, transcript/bot_plan.rs
// plan phases, schedule.rs, network.rs, needs.rs): every screen says
// "Running · step 2 of 4 · 12m" or "Today 09:00" the same way.

import SwiftUI

// MARK: - Times

enum BotTime {
    /// A duration in its largest units: "40s", "12m", "1h 5m", "3d"
    /// (bot_plan.rs `format_span`).
    static func span(_ ms: Int64) -> String {
        let s = max(ms / 1000, 0)
        switch s {
        case ..<60: return "\(s)s"
        case ..<3_600: return "\(s / 60)m"
        case ..<86_400 where (s / 60) % 60 == 0: return "\(s / 3_600)h"
        case ..<86_400: return "\(s / 3_600)h \((s / 60) % 60)m"
        default: return "\(s / 86_400)d"
        }
    }

    /// How long ago, for sub-lines: "just now", "5m ago", "2h ago", "3d ago"
    /// (parts.rs `ago_ms`).
    static func ago(_ ms: Int64, now: Int64 = nowMs()) -> String {
        let s = max(now - ms, 0) / 1000
        let short: String
        switch s {
        case ..<60: return "just now"
        case ..<3_600: short = "\(s / 60)m"
        case ..<86_400: short = "\(s / 3_600)h"
        case ..<(7 * 86_400): short = "\(s / 86_400)d"
        case ..<(35 * 86_400): short = "\(s / (7 * 86_400))w"
        case ..<(360 * 86_400): short = "\(s / (30 * 86_400))mo"
        default: short = "\(s / (365 * 86_400))y"
        }
        return "\(short) ago"
    }

    /// When something happened, in a page: "Today 14:02", "Yesterday 09:00",
    /// "Nov 9, 16:15", "Nov 9, 2025" (parts.rs `past_full`).
    static func past(_ ms: Int64, now: Int64 = nowMs(), calendar: Calendar = .current) -> String {
        let at = date(ms)
        let days = dayDistance(from: at, to: date(now), calendar: calendar)
        let time = format("HH:mm", at, calendar)
        switch days {
        case ...0: return "Today \(time)"
        case 1: return "Yesterday \(time)"
        default:
            let sameYear = calendar.component(.year, from: at) == calendar.component(.year, from: date(now))
            return sameYear ? "\(format("MMM d", at, calendar)), \(time)" : format("MMM d, yyyy", at, calendar)
        }
    }

    /// When something next happens, as a row's trailing note: "in 25m",
    /// "in 3h", "21:00" (later today), "Mon" (within a week), else "Oct 30"
    /// (parts.rs `next_label`).
    static func next(_ ms: Int64, now: Int64 = nowMs(), calendar: Calendar = .current) -> String {
        let minutes = (ms - now) / 60_000
        if minutes < 1 { return "now" }
        if minutes < 60 { return "in \(minutes)m" }
        if minutes < 12 * 60 { return "in \(minutes / 60)h" }
        let at = date(ms)
        let days = dayDistance(from: date(now), to: at, calendar: calendar)
        if days <= 0 { return format("HH:mm", at, calendar) }
        if days < 7 { return format("EEE", at, calendar) }
        return format("MMM d", at, calendar)
    }

    /// When something next happens, in a page: "Today 09:00", "Tomorrow
    /// 02:00", "Mon 18:30", "Oct 30 09:00" (parts.rs `next_full`).
    static func nextFull(_ ms: Int64, now: Int64 = nowMs(), calendar: Calendar = .current) -> String {
        let at = date(ms)
        let time = format("HH:mm", at, calendar)
        switch dayDistance(from: date(now), to: at, calendar: calendar) {
        case ...0: return "Today \(time)"
        case 1: return "Tomorrow \(time)"
        case 2...6: return "\(format("EEE", at, calendar)) \(time)"
        default: return "\(format("MMM d", at, calendar)) \(time)"
        }
    }

    private static func date(_ ms: Int64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
    }

    private static func dayDistance(from: Date, to: Date, calendar: Calendar) -> Int {
        calendar.dateComponents([.day], from: calendar.startOfDay(for: from), to: calendar.startOfDay(for: to)).day ?? 0
    }

    /// Fixed English month and weekday names, as on the desktop.
    private static func format(_ pattern: String, _ date: Date, _ calendar: Calendar) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = pattern
        return formatter.string(from: date)
    }
}

// MARK: - Tones

/// A state's colour: amber while it waits on the owner, green done, red
/// failed, the text colour while it runs, muted otherwise (bot_plan.rs
/// `PlanPhase::tint`).
enum BotTone: Hashable {
    case waiting, done, failed, active, muted

    var color: Color {
        switch self {
        case .waiting: Theme.warning
        case .done: Theme.success
        case .failed: Theme.danger
        case .active: Theme.text
        case .muted: Theme.textMuted
        }
    }

    /// The small glyph beside the words; nil draws none.
    var symbol: String? {
        switch self {
        case .waiting: "clock"
        case .done: "checkmark"
        case .failed: "exclamationmark.triangle"
        case .active: "circle.dotted"
        case .muted: nil
        }
    }
}

/// A state in words with its tone: what `BotStatusText` draws.
struct BotStatusLabel: Hashable {
    var text: String
    var tone: BotTone
    /// Overrides the tone's glyph.
    var symbol: String?

    init(_ text: String, _ tone: BotTone, symbol: String? = nil) {
        self.text = text
        self.tone = tone
        self.symbol = symbol
    }
}

// MARK: - Plans

/// Where a plan stands, the owner's pending decisions included (bot_plan.rs
/// `PlanPhase`): an approval or stop shows at once, before the host acts.
enum PlanPhase: Hashable {
    /// Proposed; nobody decided yet.
    case awaiting
    /// Approved here; its host has not started it yet.
    case starting
    /// Cancelled here; its host has not closed it yet.
    case cancelling
    case running
    /// Stop requested; its host has not stopped it yet.
    case stopping
    case done, failed, stopped, cancelled

    /// Approve / Cancel apply.
    var awaiting: Bool { self == .awaiting }
    /// Stop applies.
    var running: Bool { self == .running }
    /// It ended (or never ran): the result shows.
    var ended: Bool { [.done, .failed, .stopped, .cancelled].contains(self) }

    var tone: BotTone {
        switch self {
        case .awaiting: .waiting
        case .done: .done
        case .failed: .failed
        case .running, .starting, .stopping: .active
        case .cancelling, .stopped, .cancelled: .muted
        }
    }
}

extension BotWork {
    var phase: PlanPhase {
        switch state {
        case .proposed:
            guard let decision else { return .awaiting }
            return decision.approve ? .starting : .cancelling
        case .running: return stopRequestedAt != nil ? .stopping : .running
        case .done: return .done
        case .failed: return .failed
        case .stopped: return .stopped
        case .cancelled: return .cancelled
        }
    }

    /// The plan's state in one line: "Running · step 2 of 4 · 12m", "Done ·
    /// took 25m", "Awaiting your approval · 3 steps" (bot_plan.rs
    /// `phase_line`).
    func phaseLine(now: Int64 = nowMs()) -> String {
        let (done, total) = progress
        let steps = "\(total) step\(total == 1 ? "" : "s")"
        let took = startedAt.flatMap { start in endedAt.map { BotTime.span($0 - start) } }
        switch phase {
        case .awaiting: return "Awaiting your approval · \(steps)"
        case .starting: return "Approved · starting…"
        case .cancelling: return "Cancelling…"
        case .running:
            let step = currentStep.map { $0.index + 1 } ?? min(done, total)
            var line = "Running · step \(step) of \(total)"
            if let startedAt { line += " · \(BotTime.span(now - startedAt))" }
            return line
        case .stopping: return "Stopping…"
        case .done: return took.map { "Done · took \($0)" } ?? "Done"
        case .failed: return took.map { "Failed after \($0)" } ?? "Failed"
        case .stopped: return took.map { "Stopped after \($0)" } ?? "Stopped"
        case .cancelled: return "Cancelled"
        }
    }

    /// The status fact: the phase's line in its tone.
    func status(now: Int64 = nowMs()) -> BotStatusLabel {
        BotStatusLabel(phaseLine(now: now), phase.tone)
    }

    /// A running item's second line: "Step 2 of 4 · Run the tests in web"
    /// (work.rs `current_step_line`).
    var currentStepLine: String {
        let total = steps.count
        if let (index, step) = currentStep {
            return "Step \(index + 1) of \(total) · \(step.title.replacingOccurrences(of: "\n", with: " "))"
        }
        return total > 0 ? "Finishing · \(total) of \(total) steps done" : "Starting…"
    }
}

extension WorkStep {
    /// How long it took, or has been running (bot_plan.rs `step_span`).
    func span(now: Int64 = nowMs()) -> String? {
        guard let startedAt else { return nil }
        return BotTime.span((endedAt ?? now) - startedAt)
    }
}

extension StepState {
    var status: BotStatusLabel {
        switch self {
        case .pending: BotStatusLabel("Pending", .muted)
        case .running: BotStatusLabel("Running", .active)
        case .asking: BotStatusLabel("Asking", .waiting, symbol: "questionmark.circle")
        case .done: BotStatusLabel("Done", .done)
        case .failed: BotStatusLabel("Failed", .failed, symbol: "xmark.circle")
        case .skipped: BotStatusLabel("Skipped", .muted)
        }
    }
}

// MARK: - Routines

extension RunOutcome {
    /// schedule.rs: a run's outcome.
    var status: BotStatusLabel {
        switch self {
        case .running: BotStatusLabel("Running", .active)
        case .done: BotStatusLabel("Done", .done)
        case .failed: BotStatusLabel("Failed", .failed)
        case .skipped: BotStatusLabel("Skipped", .muted)
        }
    }
}

extension RoutineTarget {
    /// Where it runs, for a fact: "Workspace" or the project's name.
    func label(projectName: (String) -> String?) -> String {
        switch self {
        case .workspace: return "Workspace"
        case .project(let spaceId, let worktree, _):
            let name = projectName(spaceId) ?? "A removed project"
            return worktree ? "\(name) · own worktree" : name
        }
    }
}

// MARK: - Approvals and connections

extension BotApproval {
    /// needs.rs: an approval's state, the owner's decision before the host's.
    var status: BotStatusLabel {
        if awaiting { return BotStatusLabel("Awaiting your approval", .waiting) }
        switch (state, decision?.approve) {
        case (.approved, _), (.pending, true): return BotStatusLabel("Approved", .done)
        case (.declined, _), (.pending, false): return BotStatusLabel("Declined", .muted)
        default: return BotStatusLabel("Expired before an answer", .muted)
        }
    }
}

extension ReplyMode {
    /// network.rs `mode_words`.
    var label: String {
        switch self {
        case .auto: "Auto replies"
        case .review: "Review replies"
        case .off: "Replies off"
        }
    }

    /// The picker's explanation.
    var detail: String {
        switch self {
        case .auto: "Answers go back at once."
        case .review: "Answers wait as drafts until you send them."
        case .off: "Questions are declined."
        }
    }
}

extension BotExchange {
    /// The state in lower-case words for a list's sub-line ("awaiting your
    /// review", "answered"; network.rs `exchange_state_words`).
    var stateWords: String {
        switch state {
        case .queued: return "queued"
        case .delivered: return "delivered"
        case .working: return "working"
        case .awaitingReview:
            guard let review else { return "awaiting your review" }
            return review.send ? "sending your answer" : "declining"
        case .answered: return "answered"
        case .declined: return "declined"
        case .expired: return "expired"
        }
    }

    /// How it ended (or stands), as the question's page says it (needs.rs).
    var status: BotStatusLabel {
        if awaitingReview { return BotStatusLabel("Awaiting your review", .waiting) }
        switch direction {
        case .incoming:
            switch (state, review?.send) {
            case (.answered, _): return BotStatusLabel("Sent", .done)
            case (_, true): return BotStatusLabel("Sending your answer", .active)
            case (.declined, _), (_, false): return BotStatusLabel("Declined", .muted)
            case (.expired, _): return BotStatusLabel("Expired", .muted)
            default: return BotStatusLabel("Working on its answer", .active)
            }
        case .outgoing:
            switch state {
            case .answered: return BotStatusLabel("Answered", .done)
            case .declined: return BotStatusLabel("They declined to answer", .muted)
            case .expired: return BotStatusLabel("No answer came; it expired", .muted)
            default: return BotStatusLabel("Waiting for their answer", .active)
            }
        }
    }
}

// When a routine runs next (bot_records.rs `Schedule::next_after`,
// crates/ui/src/shell/bots/schedule.rs `next_run`): the host's plan while it
// is still ahead, else the schedule's own next slot. Its row's state is the
// board's `BoardRoutineState` (BoardModel.swift); this file adds the
// schedule arithmetic and the words the Routine page says it in.

import Foundation

extension Schedule {
    /// The first run after `after`, in `calendar`'s time zone; nil for a
    /// manual schedule, or a once-only one whose time has passed.
    func nextAfter(_ after: Date, calendar: Calendar = .current) -> Date? {
        switch self {
        case .manual:
            return nil
        case .once(let at):
            return Self.onceDate(at, calendar: calendar).flatMap { $0 > after ? $0 : nil }
        case .interval(let minutes):
            return after.addingTimeInterval(TimeInterval(Swift.max(minutes, Self.minInterval) * 60))
        case .daily(let time, let days):
            guard let (hour, minute) = Self.parseTime(time) else { return nil }
            let start = calendar.startOfDay(for: after)
            for offset in 0...8 {
                guard let day = calendar.date(byAdding: .day, value: offset, to: start) else { continue }
                if !days.isEmpty && !days.contains(Self.weekday(of: day, calendar: calendar)) { continue }
                guard let candidate = calendar.date(bySettingHour: hour, minute: minute, second: 0, of: day)
                else { continue }
                if candidate > after { return candidate }
            }
            return nil
        }
    }

    /// The schedule's day number for `date`: 1 = Monday … 7 = Sunday.
    static func weekday(of date: Date, calendar: Calendar = .current) -> Int {
        // Calendar weekdays run 1 = Sunday … 7 = Saturday.
        (calendar.component(.weekday, from: date) + 5) % 7 + 1
    }

    /// A once-only schedule's "YYYY-MM-DDTHH:MM" in `calendar`'s zone.
    static func onceDate(_ text: String, calendar: Calendar = .current) -> Date? {
        onceFormatter(calendar).date(from: text)
    }

    /// `date` as a once-only schedule's "YYYY-MM-DDTHH:MM", local to
    /// `calendar`.
    static func onceText(_ date: Date, calendar: Calendar = .current) -> String {
        onceFormatter(calendar).string(from: date)
    }

    private static func onceFormatter(_ calendar: Calendar) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.dateFormat = "yyyy-MM-dd'T'HH:mm"
        return f
    }
}

extension BotRoutine {
    /// The run after `now`: the host's plan while it is still ahead, else
    /// the schedule's own next slot; nil while off or manual.
    func nextRun(now: Int64 = nowMs(), calendar: Calendar = .current) -> Int64? {
        guard enabled, schedule != .manual else { return nil }
        if let planned = nextRunAt, planned > now { return planned }
        let after = Date(timeIntervalSince1970: TimeInterval(now) / 1000)
        return schedule.nextAfter(after, calendar: calendar).map { Int64(($0.timeIntervalSince1970 * 1000).rounded()) }
    }

    var lastRunFailed: Bool { lastRun?.outcome == .failed }

    /// How it is set up, on one line, each part only when it is not the
    /// default (schedule.rs `setup_line`): its schedule (a manual one has
    /// none, a once-only one is just "Once": its status names the time),
    /// the project it runs in, its own worktree, its model. "Weekdays
    /// 09:00 · billing · own worktree"; empty for a manual routine in the
    /// workspace.
    func setupLine(place: String?, model: String? = nil) -> String {
        let when: String? =
            switch schedule {
            case .manual: nil
            case .once: "Once"
            default: schedule.describe
            }
        var worktree = false
        if case .project(_, true, _) = target { worktree = true }
        return [when, place, worktree ? "own worktree" : nil, model].compactMap { $0 }.joined(separator: " · ")
    }
}

extension BoardRoutineState {
    /// Whether the Enabled switch applies: a manual routine has no schedule
    /// to switch off, nor a reminder that has had its run.
    func switchable(_ routine: BotRoutine) -> Bool {
        routine.schedule != .manual && self != .done
    }

    /// Running, or asked to run: Run now waits.
    var runBusy: Bool { self == .running || self == .starting }
}

extension WorkspaceData {
    /// A workspace's routines as the Routines page lists them: running
    /// first, then by next run, manual, done and switched off last.
    func routinesInOrder(of botId: String, now: Int64 = nowMs()) -> [BotRoutine] {
        BoardRoutineState.ordered(botRecords.routines.filter { $0.botId == botId }, now: now)
    }
}

enum RoutineWords {
    /// How soon `at` comes: "now", "in 25m", "in 9h", "in 3 days".
    static func inWords(_ at: Int64, now: Int64 = nowMs()) -> String {
        let minutes = max((at - now) / 60_000, 0)
        switch minutes {
        case 0: return "now"
        case 1...59: return "in \(minutes)m"
        case 60...2879: return "in \(minutes / 60)h"
        default: return "in \(minutes / 1440) days"
        }
    }

    /// How a routine stands, in a word or two for its page's head
    /// (schedule.rs `status_word`): what its run is doing, else why none is
    /// coming, else how soon the next one comes ("Next in 3h"; a once-only
    /// one names its time, "Tomorrow 09:00", which its setup line then
    /// leaves out).
    static func status(
        _ state: BoardRoutineState, schedule: Schedule, asking: Bool, paused: Bool, now: Int64 = nowMs()
    ) -> BotStatusLabel {
        switch state {
        case .running:
            return asking ? BotStatusLabel("Waiting on you", .waiting) : BotStatusLabel("Running", .active)
        case .starting: return BotStatusLabel("Starting…", .active)
        case .done: return BotStatusLabel("Done", .muted)
        case .off: return BotStatusLabel("Off", .muted)
        default: break
        }
        // A paused workspace runs nothing on a schedule; Run now still works.
        if paused { return BotStatusLabel("Workspace paused", .waiting) }
        switch state {
        case .manual: return BotStatusLabel("Manual", .muted)
        case .next(let at):
            if case .once = schedule { return BotStatusLabel(BotTime.nextFull(at, now: now), .active) }
            return BotStatusLabel("Next \(inWords(at, now: now))", .active)
        default: return BotStatusLabel("No next run", .muted)
        }
    }

    /// What a tap on the status adds: the exact time of the next run, or
    /// when the run under way started (schedule.rs `status_tip`).
    static func statusDetails(
        _ routine: BotRoutine, state: BoardRoutineState, now: Int64 = nowMs()
    ) -> [(label: String, value: String)] {
        switch state {
        case .running:
            guard let run = routine.lastRun else { return [] }
            return [("Started", BotTime.past(run.at, now: now) + (run.manual ? " · by hand" : ""))]
        case .next(let at):
            if case .once = routine.schedule { return [] }
            return [("Next run", BotTime.nextFull(at, now: now))]
        default: return []
        }
    }
}

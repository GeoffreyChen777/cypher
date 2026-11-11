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

    /// The Trigger fact: the schedule in words, a manual one as how it runs.
    static func trigger(_ schedule: Schedule) -> String {
        schedule == .manual ? "Only when you press Run now" : schedule.describe
    }

    /// The Next run fact: "Tomorrow 09:00 · in 14h", "Off", "Done".
    static func nextRun(_ state: BoardRoutineState, now: Int64 = nowMs()) -> String {
        switch state {
        case .next(let at): "\(BotTime.nextFull(at, now: now)) · \(inWords(at, now: now))"
        case .running: "Running now"
        case .starting: "Run requested"
        case .manual: "When you press Run now"
        case .done: "Done; it ran once"
        case .unplanned: "Not planned yet"
        case .off: "Off"
        }
    }
}

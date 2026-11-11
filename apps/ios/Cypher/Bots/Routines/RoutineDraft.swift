// The routine form's state and its checks (crates/ui/src/shell/bots/
// schedule.rs `ScheduleDraft`, `JobForm`, `save_job_form`): the schedule
// editor's choices, where it runs and what it may do, built into the
// `BotRoutine` that `saveRoutine` sends, or the sentence saying what to fix.

import Foundation

/// The schedule editor: one kind and the fields each kind reads.
struct ScheduleDraft: Equatable {
    enum Kind: String, CaseIterable, Identifiable {
        case daily, interval, once, manual

        var id: Self { self }

        var label: String {
            switch self {
            case .daily: "Daily"
            case .interval: "Interval"
            case .once: "Once"
            case .manual: "Manual"
            }
        }
    }

    /// The day sets the Daily editor offers in one tap.
    enum DayPreset: CaseIterable, Identifiable {
        case everyDay, weekdays, weekends

        var id: Self { self }

        var label: String {
            switch self {
            case .everyDay: "Every day"
            case .weekdays: "Weekdays"
            case .weekends: "Weekends"
            }
        }

        /// Empty means every day.
        var days: [Int] {
            switch self {
            case .everyDay: []
            case .weekdays: [1, 2, 3, 4, 5]
            case .weekends: [6, 7]
            }
        }
    }

    var kind: Kind = .daily
    /// A daily routine's time.
    var hour = 9
    var minute = 0
    /// 1 = Monday … 7 = Sunday; empty = every day.
    var days: [Int] = []
    var intervalMinutes = 60
    /// A once-only run's local date and time.
    var onceAt: Date

    /// The intervals the picker offers; an edited routine's own is added.
    static let intervalChoices = [15, 30, 60, 120, 180, 360, 720, 1_440]
    /// One-letter weekday chips, Monday first.
    static let dayLetters = ["M", "T", "W", "T", "F", "S", "S"]
    static let dayNames = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]

    /// The editor for `schedule`. A once-only run starts out tomorrow at
    /// 09:00.
    init(schedule: Schedule = .default, now: Date = Date(), calendar: Calendar = .current) {
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: now)) ?? now
        onceAt = calendar.date(bySettingHour: 9, minute: 0, second: 0, of: tomorrow) ?? tomorrow
        switch schedule {
        case .daily(let time, let days):
            if let (hour, minute) = Schedule.parseTime(time) {
                self.hour = hour
                self.minute = minute
            }
            self.days = Self.normalized(days)
        case .interval(let minutes):
            kind = .interval
            intervalMinutes = minutes
        case .once(let at):
            kind = .once
            if let date = Schedule.onceDate(at, calendar: calendar) { onceAt = date }
        case .manual:
            kind = .manual
        }
    }

    /// The schedule the editor describes, or what to fix.
    func build(calendar: Calendar = .current) -> Result<Schedule, BotActionError> {
        let schedule: Schedule
        switch kind {
        case .daily:
            schedule = .daily(time: String(format: "%02d:%02d", hour, minute), days: Self.normalized(days))
        case .interval:
            schedule = .interval(minutes: intervalMinutes)
        case .once:
            schedule = .once(at: Schedule.onceText(onceAt, calendar: calendar))
        case .manual:
            schedule = .manual
        }
        if let error = schedule.validationError { return .failure(BotActionError(error)) }
        return .success(schedule)
    }

    /// Whether `day` (1 = Monday) is picked; none picked means every day.
    func hasDay(_ day: Int) -> Bool {
        days.isEmpty || days.contains(day)
    }

    /// Flip one day. Every day (none picked) drops just this one; the last
    /// day left stays, since a daily routine runs on at least one.
    mutating func toggleDay(_ day: Int) {
        if days.isEmpty {
            days = (1...7).filter { $0 != day }
        } else if let ix = days.firstIndex(of: day) {
            if days.count > 1 { days.remove(at: ix) }
        } else {
            days = Self.normalized(days + [day])
        }
    }

    /// The preset the picked days match, if any.
    var preset: DayPreset? {
        DayPreset.allCases.first { $0.days == days }
    }

    /// The interval picker's choices: the standard ones and this one.
    var intervalOptions: [Int] {
        Array(Set(Self.intervalChoices + [intervalMinutes])).filter { $0 > 0 }.sorted()
    }

    /// "15 min", "1 hour", "6 hours", "1 day".
    static func intervalLabel(_ minutes: Int) -> String {
        if minutes >= 1_440, minutes % 1_440 == 0 {
            let days = minutes / 1_440
            return days == 1 ? "1 day" : "\(days) days"
        }
        if minutes % 60 == 0 {
            let hours = minutes / 60
            return hours == 1 ? "1 hour" : "\(hours) hours"
        }
        return "\(minutes) min"
    }

    /// Days sorted and unique, every day as none.
    static func normalized(_ days: [Int]) -> [Int] {
        let sorted = Array(Set(days)).sorted()
        return sorted == [1, 2, 3, 4, 5, 6, 7] ? [] : sorted
    }
}

/// Everything the routine form edits.
struct RoutineDraft: Equatable {
    var name = ""
    var prompt = ""
    /// Where it runs: a project (space id), or nil for the workspace.
    var spaceId: String?
    /// A project routine's own worktree rather than the project checkout.
    var worktree = true
    /// The model its runs use; nil follows the workspace chat's.
    var model: String?
    var schedule: ScheduleDraft
    var writeNotes = false
    var workDirectly = false
    var enabled = true

    /// A new routine's form.
    init(now: Date = Date(), calendar: Calendar = .current) {
        schedule = ScheduleDraft(now: now, calendar: calendar)
    }

    /// An edit form, from the record.
    init(routine: BotRoutine, now: Date = Date(), calendar: Calendar = .current) {
        name = routine.name
        prompt = routine.prompt
        if case .project(let spaceId, let worktree, _) = routine.target {
            self.spaceId = spaceId
            self.worktree = worktree
        }
        model = routine.chosenModel
        schedule = ScheduleDraft(schedule: routine.schedule, now: now, calendar: calendar)
        writeNotes = routine.writeNotes
        workDirectly = routine.workDirectly
        enabled = routine.enabled
    }

    var target: RoutineTarget {
        // The model lives on the routine, not its target.
        spaceId.map { .project(spaceId: $0, worktree: worktree, model: nil) } ?? .workspace
    }

    /// The routine to save for `bot`, or what to fix. `existing` is the
    /// record an edit started from; `id` names a new one. Host fields go
    /// out empty: the host keeps its own (as the desktop sends them).
    func routine(
        for bot: Bot, existing: BotRoutine?, id: @autoclosure () -> String = UUID().uuidString.lowercased(),
        now: Date = Date(), calendar: Calendar = .current
    ) -> Result<BotRoutine, BotActionError> {
        let schedule: Schedule
        switch self.schedule.build(calendar: calendar) {
        case .success(let built): schedule = built
        case .failure(let error): return .failure(error)
        }
        // A reminder set for a time gone by would never come; one that has
        // run keeps its time through other edits.
        if case .once = schedule, schedule.nextAfter(now, calendar: calendar) == nil,
            existing?.schedule != schedule
        {
            return .failure(BotActionError("That time has passed; pick a later one."))
        }
        let nowMs = Int64((now.timeIntervalSince1970 * 1000).rounded())
        let routine = BotRoutine(
            id: existing?.id ?? id(), botId: bot.id,
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            prompt: prompt.trimmingCharacters(in: .whitespacesAndNewlines),
            target: target, model: model, schedule: schedule, enabled: enabled,
            writeNotes: writeNotes, workDirectly: workDirectly,
            createdAt: existing?.createdAt ?? nowMs, updatedAt: nowMs)
        if let error = routine.validationError { return .failure(BotActionError(error)) }
        if routine.target.spaceId != nil && !bot.capabilities.coordinate {
            return .failure(BotActionError(Self.coordinateNote))
        }
        return .success(routine)
    }

    static let coordinateNote = "Running in a project needs Coordinate in workspace settings."

    /// The line under the schedule: when it next runs, or what to fix
    /// ("Weekdays 09:00 · next run today 09:00").
    func scheduleHint(now: Date = Date(), calendar: Calendar = .current) -> (text: String, problem: Bool) {
        switch schedule.build(calendar: calendar) {
        case .failure(let error):
            return (error.message, true)
        case .success(.manual):
            return ("Runs only when you press Run now.", false)
        case .success(let built):
            let nowMs = Int64((now.timeIntervalSince1970 * 1000).rounded())
            guard let next = built.nextAfter(now, calendar: calendar) else {
                return ("\(built.describe) · that time has passed", true)
            }
            let at = Int64((next.timeIntervalSince1970 * 1000).rounded())
            if case .once = built {
                return ("\(built.describe) · \(RoutineWords.inWords(at, now: nowMs))", false)
            }
            let full = BotTime.nextFull(at, now: nowMs, calendar: calendar)
            let lowered = full.hasPrefix("To") ? full.prefix(1).lowercased() + full.dropFirst() : full
            return ("\(built.describe) · next run \(lowered)", false)
        }
    }
}

// The routine pages' logic (crates/ui/src/shell/bots/schedule.rs tests,
// bot_records.rs `next_after`): the schedule editor building and checking
// what it sends (weekdays 1 = Monday … 7 = Sunday, the 15-minute interval
// floor, a once-only run's local "YYYY-MM-DDTHH:MM"), the form's checks, the
// next run, and the order the Routines page lists them in.

import XCTest
@testable import Cypher

final class RoutineScheduleTests: XCTestCase {
    private var calendar: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "Europe/Berlin")!
        return c
    }()

    /// A local time in the test calendar.
    private func at(_ y: Int, _ mo: Int, _ d: Int, _ h: Int, _ mi: Int = 0) -> Date {
        calendar.date(from: DateComponents(year: y, month: mo, day: d, hour: h, minute: mi))!
    }

    private func ms(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }

    private func bot(coordinate: Bool = true) -> Bot {
        Bot(
            id: "b", name: "Atlas", hostDeviceId: "mac", chatId: "c",
            capabilities: BotCapabilities(workDirectly: true, coordinate: coordinate), createdAt: 0)
    }

    // MARK: Schedules

    func testWeekdaysCountFromMonday() {
        // 2026-11-09 is a Monday, 2026-11-15 a Sunday.
        XCTAssertEqual(Schedule.weekday(of: at(2026, 11, 9, 12), calendar: calendar), 1)
        XCTAssertEqual(Schedule.weekday(of: at(2026, 11, 10, 12), calendar: calendar), 2)
        XCTAssertEqual(Schedule.weekday(of: at(2026, 11, 14, 12), calendar: calendar), 6)
        XCTAssertEqual(Schedule.weekday(of: at(2026, 11, 15, 12), calendar: calendar), 7)
    }

    func testTheNextRunFollowsTheSchedule() {
        let tuesday10 = at(2026, 11, 10, 10)
        XCTAssertEqual(
            Schedule.daily(time: "09:00", days: []).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 11, 9))
        XCTAssertEqual(
            Schedule.daily(time: "18:30", days: []).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 10, 18, 30))
        XCTAssertEqual(
            Schedule.daily(time: "09:00", days: [6, 7]).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 14, 9))
        XCTAssertEqual(
            Schedule.daily(time: "09:00", days: [2]).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 17, 9),
            "today's slot has passed: next week's")
        XCTAssertEqual(
            Schedule.interval(minutes: 120).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 10, 12))
        XCTAssertEqual(
            Schedule.interval(minutes: 5).nextAfter(tuesday10, calendar: calendar), at(2026, 11, 10, 10, 15),
            "never sooner than the floor")
        XCTAssertEqual(
            Schedule.once(at: "2026-11-12T10:30").nextAfter(tuesday10, calendar: calendar), at(2026, 11, 12, 10, 30))
        XCTAssertNil(Schedule.once(at: "2026-11-09T10:30").nextAfter(tuesday10, calendar: calendar))
        XCTAssertNil(Schedule.manual.nextAfter(tuesday10, calendar: calendar))
    }

    func testTheHostsPlanWinsWhileItIsAhead() {
        let now = ms(at(2026, 11, 10, 10))
        var routine = BotRoutine(
            id: "r", botId: "b", name: "Brief", prompt: "p", schedule: .daily(time: "09:00", days: []),
            createdAt: 0, updatedAt: 0)
        routine.nextRunAt = now + 60_000
        XCTAssertEqual(routine.nextRun(now: now, calendar: calendar), now + 60_000)
        routine.nextRunAt = now - 60_000
        XCTAssertEqual(routine.nextRun(now: now, calendar: calendar), ms(at(2026, 11, 11, 9)), "a stale plan")
        routine.enabled = false
        XCTAssertNil(routine.nextRun(now: now, calendar: calendar))
        routine.enabled = true
        routine.schedule = .manual
        XCTAssertNil(routine.nextRun(now: now, calendar: calendar))
    }

    // MARK: The editor

    func testTheEditorRoundTripsEverySchedule() throws {
        let now = at(2026, 11, 10, 10)
        for schedule: Schedule in [
            .daily(time: "07:05", days: [1, 3, 5]), .daily(time: "23:59", days: []), .interval(minutes: 45),
            .once(at: "2026-12-01T08:15"), .manual,
        ] {
            let draft = ScheduleDraft(schedule: schedule, now: now, calendar: calendar)
            XCTAssertEqual(try draft.build(calendar: calendar).get(), schedule)
        }
    }

    func testTheEditorBuildsWhatTheEngineAccepts() throws {
        var draft = ScheduleDraft(now: at(2026, 11, 10, 10), calendar: calendar)
        XCTAssertEqual(draft.kind, .daily)
        draft.hour = 7
        draft.minute = 5
        draft.days = [7, 1, 1, 6]
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .daily(time: "07:05", days: [1, 6, 7]))
        draft.days = [1, 2, 3, 4, 5, 6, 7]
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .daily(time: "07:05", days: []), "every day")

        draft.kind = .interval
        draft.intervalMinutes = 10
        XCTAssertEqual(draft.build(calendar: calendar), .failure(BotActionError("Run at most every 15 minutes.")))
        draft.intervalMinutes = 15
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .interval(minutes: 15))

        // A once-only run starts out tomorrow at 09:00, sent as local time.
        draft.kind = .once
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .once(at: "2026-11-11T09:00"))
        draft.onceAt = at(2027, 1, 2, 18, 45).addingTimeInterval(37)
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .once(at: "2027-01-02T18:45"), "seconds drop")

        draft.kind = .manual
        XCTAssertEqual(try draft.build(calendar: calendar).get(), .manual)
    }

    func testDaysStartAsEveryDayAndKeepAtLeastOne() {
        var draft = ScheduleDraft(now: Date(), calendar: calendar)
        XCTAssertTrue((1...7).allSatisfy(draft.hasDay))
        XCTAssertEqual(draft.preset, .everyDay)
        draft.toggleDay(3)
        XCTAssertEqual(draft.days, [1, 2, 4, 5, 6, 7])
        XCTAssertFalse(draft.hasDay(3))
        draft.toggleDay(3)
        XCTAssertEqual(draft.days, [], "all seven again: every day")
        draft.days = ScheduleDraft.DayPreset.weekends.days
        XCTAssertEqual(draft.preset, .weekends)
        draft.toggleDay(6)
        XCTAssertEqual(draft.days, [7])
        draft.toggleDay(7)
        XCTAssertEqual(draft.days, [7], "the last day stays")
        XCTAssertNil(draft.preset)
        draft.days = ScheduleDraft.DayPreset.weekdays.days
        XCTAssertEqual(draft.preset, .weekdays)
    }

    func testIntervalChoices() {
        var draft = ScheduleDraft(schedule: .interval(minutes: 45), now: Date(), calendar: calendar)
        XCTAssertEqual(draft.intervalOptions, [15, 30, 45, 60, 120, 180, 360, 720, 1_440])
        draft.intervalMinutes = 60
        XCTAssertEqual(draft.intervalOptions, ScheduleDraft.intervalChoices)
        XCTAssertEqual(
            [15, 60, 120, 90, 1_440, 2_880].map(ScheduleDraft.intervalLabel),
            ["15 min", "1 hour", "2 hours", "90 min", "1 day", "2 days"])
    }

    // MARK: The form

    func testTheFormSaysWhatToFix() {
        let now = at(2026, 11, 10, 10)
        var draft = RoutineDraft(now: now, calendar: calendar)
        XCTAssertEqual(
            draft.routine(for: bot(), existing: nil, id: "new", now: now, calendar: calendar),
            .failure(BotActionError("Name the routine.")))
        draft.name = "  Brief "
        XCTAssertEqual(
            draft.routine(for: bot(), existing: nil, id: "new", now: now, calendar: calendar),
            .failure(BotActionError("Say what the routine should do.")))
        draft.prompt = "Summarise my day.\n"
        draft.schedule.kind = .once
        draft.schedule.onceAt = at(2026, 11, 10, 9)
        XCTAssertEqual(
            draft.routine(for: bot(), existing: nil, id: "new", now: now, calendar: calendar),
            .failure(BotActionError("That time has passed; pick a later one.")))
        draft.schedule.kind = .daily
        draft.spaceId = "space-web"
        XCTAssertEqual(
            draft.routine(for: bot(coordinate: false), existing: nil, id: "new", now: now, calendar: calendar),
            .failure(BotActionError(RoutineDraft.coordinateNote)))

        let saved = try? draft.routine(for: bot(), existing: nil, id: "new", now: now, calendar: calendar).get()
        XCTAssertEqual(saved?.id, "new")
        XCTAssertEqual(saved?.name, "Brief")
        XCTAssertEqual(saved?.prompt, "Summarise my day.")
        XCTAssertEqual(saved?.target, .project(spaceId: "space-web", worktree: true, model: nil))
        XCTAssertEqual(saved?.schedule, .daily(time: "09:00", days: []))
        XCTAssertEqual(saved?.createdAt, ms(now))
    }

    func testAnEditKeepsItsIdentityAndAnOnceThatRan() throws {
        let now = at(2026, 11, 10, 10)
        var existing = BotRoutine(
            id: "r1", botId: "b", name: "Release", prompt: "Remind me.",
            target: .project(spaceId: "s", worktree: false, model: "anthropic/opus"),
            schedule: .once(at: "2026-11-09T10:00"), enabled: false, writeNotes: true, workDirectly: true,
            createdAt: 5, updatedAt: 6)
        existing.lastRun = RunRecord(at: 1, outcome: .done)
        var draft = RoutineDraft(routine: existing, now: now, calendar: calendar)
        XCTAssertEqual(draft.spaceId, "s")
        XCTAssertFalse(draft.worktree)
        XCTAssertEqual(draft.model, "anthropic/opus", "a target's model moves onto the routine")
        XCTAssertFalse(draft.enabled)
        draft.name = "Release 0.9"
        // Its time has passed, but it is the time it had.
        let saved = try draft.routine(for: bot(), existing: existing, id: "unused", now: now, calendar: calendar).get()
        XCTAssertEqual(saved.id, "r1")
        XCTAssertEqual(saved.createdAt, 5)
        XCTAssertEqual(saved.updatedAt, ms(now))
        XCTAssertEqual(saved.model, "anthropic/opus")
        XCTAssertEqual(saved.target, .project(spaceId: "s", worktree: false, model: nil))
        XCTAssertEqual(saved.schedule, .once(at: "2026-11-09T10:00"))
        XCTAssertTrue(saved.writeNotes && saved.workDirectly)
        XCTAssertNil(saved.lastRun, "host fields go out empty")
    }

    func testTheScheduleHintSaysWhenItNextRuns() {
        let now = at(2026, 11, 10, 10)
        var draft = RoutineDraft(now: now, calendar: calendar)
        XCTAssertEqual(draft.scheduleHint(now: now, calendar: calendar).text, "Daily 09:00 · next run tomorrow 09:00")
        draft.schedule.hour = 18
        draft.schedule.days = [1, 2, 3, 4, 5]
        XCTAssertEqual(draft.scheduleHint(now: now, calendar: calendar).text, "Weekdays 18:00 · next run today 18:00")
        draft.schedule.kind = .interval
        draft.schedule.intervalMinutes = 10
        XCTAssertEqual(draft.scheduleHint(now: now, calendar: calendar).text, "Run at most every 15 minutes.")
        XCTAssertTrue(draft.scheduleHint(now: now, calendar: calendar).problem)
        draft.schedule.kind = .manual
        XCTAssertEqual(draft.scheduleHint(now: now, calendar: calendar).text, "Runs only when you press Run now.")
        draft.schedule.kind = .once
        draft.schedule.onceAt = at(2026, 11, 9, 9)
        XCTAssertTrue(draft.scheduleHint(now: now, calendar: calendar).problem)
    }

    // MARK: The list

    func testRoutinesListRunningFirstThenByNextRun() {
        let now: Int64 = 1_000_000_000
        func routine(_ id: String, _ schedule: Schedule, enabled: Bool = true, next: Int64? = nil) -> BotRoutine {
            var r = BotRoutine(
                id: id, botId: "b", name: id, prompt: "p", schedule: schedule, enabled: enabled, createdAt: 0,
                updatedAt: 0)
            r.nextRunAt = next
            return r
        }
        var running = routine("running", .daily(time: "09:00", days: []), next: now + 9_000)
        running.lastRun = RunRecord(at: now - 1, outcome: .running)
        var asked = routine("asked", .manual)
        asked.runRequestedAt = now - 10
        var reminded = routine("reminded", .once(at: "2001-01-01T09:00"))
        reminded.lastRun = RunRecord(at: now - 50, outcome: .done)
        let ordered = BoardRoutineState.ordered(
            [
                routine("off", .daily(time: "09:00", days: []), enabled: false),
                routine("Later", .interval(minutes: 60), next: now + 5_000), reminded,
                routine("manual", .manual), routine("soon", .daily(time: "09:00", days: []), next: now + 1_000),
                asked, running,
            ], now: now)
        XCTAssertEqual(ordered.map(\.id), ["running", "asked", "soon", "Later", "manual", "reminded", "off"])
        XCTAssertEqual(BoardRoutineState(asked, now: now), .starting)
        XCTAssertTrue(BoardRoutineState(asked, now: now).runBusy)
        XCTAssertFalse(BoardRoutineState(reminded, now: now).switchable(reminded))
        let manual = routine("m", .manual)
        XCTAssertFalse(BoardRoutineState(manual, now: now).switchable(manual))
    }

    func testRoutineWords() {
        let now: Int64 = 1_000_000_000_000
        XCTAssertEqual(RoutineWords.inWords(now + 20_000, now: now), "now")
        XCTAssertEqual(RoutineWords.inWords(now + 25 * 60_000, now: now), "in 25m")
        XCTAssertEqual(RoutineWords.inWords(now + 9 * 3_600_000, now: now), "in 9h")
        XCTAssertEqual(RoutineWords.inWords(now + 3 * 86_400_000, now: now), "in 3 days")
        XCTAssertEqual(RoutineWords.trigger(.manual), "Only when you press Run now")
        XCTAssertEqual(RoutineWords.trigger(.daily(time: "09:00", days: [6, 7])), "Weekends 09:00")
        XCTAssertEqual(RoutineWords.nextRun(.off, now: now), "Off")
    }

    @MainActor
    func testTheDemoRoutinesAreInOrder() {
        let demo = DemoDataset.standard()
        let ids = demo.routinesInOrder(of: DemoDataset.atlasId).map(\.id)
        XCTAssertEqual(
            Set(ids), ["routine-brief", "routine-tests", "routine-triage", "routine-release", "routine-deps"])
        XCTAssertEqual(ids.suffix(2), ["routine-triage", "routine-deps"], "manual, then switched off")
    }
}

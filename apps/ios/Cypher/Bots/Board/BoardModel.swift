// What the board's pages read, worked out apart from the views so it can be
// tested: the notice when something is off (board.rs `board_notice`), the
// Needs you rows (needs.rs `need_rows`), the Work lists and their lines
// (work.rs), each granted project's load (board.rs `project_loads`), a
// routine's row note (schedule.rs `job_state`), the Connections summary
// (network.rs) and the Activity log read apart and grouped by day
// (activity.rs `parts_of`).

import Foundation

/// One line, whitespace runs folded to a space (transcript.rs
/// `single_line`).
func singleLine(_ text: String) -> String {
    text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
}

/// Markdown an agent wrote, read as plain text for a row (items.rs
/// `plain_markdown`): a link keeps its text, emphasis, code ticks and heading
/// marks drop, a table row reads as its cells joined by " · " (its rule line
/// goes); lines stay.
func plainMarkdown(_ text: String) -> String {
    let lines: [String] = text.components(separatedBy: "\n").compactMap { line in
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("|") {
            if trimmed.allSatisfy({ "|-: ".contains($0) }) { return nil }
            return trimmed.trimmingCharacters(in: CharacterSet(charactersIn: "|"))
                .split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }.joined(separator: " · ")
        }
        return String(trimmed.drop { $0 == "#" }).trimmingCharacters(in: .whitespaces)
    }
    var rest = Substring(lines.joined(separator: "\n"))
    var out = ""
    while let open = rest.firstIndex(of: "[") {
        out += rest[..<open]
        let after = rest[rest.index(after: open)...]
        if let close = after.range(of: "]("), !after[..<close.lowerBound].contains("\n"),
            let end = after[close.upperBound...].firstIndex(of: ")")
        {
            out += after[..<close.lowerBound]
            rest = after[after.index(after: end)...]
        } else {
            out += "["
            rest = after
        }
    }
    out += rest
    return out.replacingOccurrences(of: "**", with: "").replacingOccurrences(of: "__", with: "")
        .replacingOccurrences(of: "`", with: "")
}

// MARK: - Notice

/// Something off about a workspace, said once under its header; the normal
/// case says nothing.
enum BoardNotice: Equatable {
    case paused
    /// Its host (by name) is offline.
    case offline(String)
    /// Its chat's last turn failed.
    case failed

    static func of(paused: Bool, hostOnline: Bool, host: String, chat: ChatIndicator) -> BoardNotice? {
        if paused { return .paused }
        if !hostOnline { return .offline(host) }
        if chat == .errored { return .failed }
        return nil
    }
}

// MARK: - Needs you

/// One Needs you item as a row draws it.
struct BoardNeedRow: Identifiable, Equatable {
    let item: NeedsYouItem
    /// SF Symbol.
    var symbol: String
    var title: String
    var sub: String
    var at: Int64?

    var id: String { item.id }
}

// MARK: - Work

/// A workspace's work in the Work page's three lists (work.rs `work_lists`).
struct BoardWorkLists: Equatable {
    var running: [BotWork] = []
    var proposed: [BotWork] = []
    var finished: [BotWork] = []

    var isEmpty: Bool { running.isEmpty && proposed.isEmpty && finished.isEmpty }

    /// `work`, already in `WorkspaceData.work(of:)` order.
    init(_ work: [BotWork]) {
        for item in work {
            switch item.state {
            case .running: running.append(item)
            case .proposed: proposed.append(item)
            default: finished.append(item)
            }
        }
    }

    /// The overview's few finished items: cancelled proposals only show in
    /// the full list.
    func recent(_ count: Int) -> [BotWork] {
        Array(finished.filter { $0.state != .cancelled }.prefix(count))
    }
}

extension BotWork {
    /// Whether its page shows the request: there is one, and it says more
    /// than the title.
    var showsRequest: Bool {
        func norm(_ text: String) -> String {
            var text = text.trimmingCharacters(in: .whitespacesAndNewlines)
            while text.hasSuffix(".") { text.removeLast() }
            return text.lowercased()
        }
        return !instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && norm(instruction) != norm(title)
    }

    /// A finished item's one line: its result's first paragraph, else how it
    /// ended (work.rs `result_line`).
    func resultLine(now: Int64 = nowMs()) -> String {
        let paragraph = result?.components(separatedBy: "\n\n")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first { !$0.isEmpty }
        let line = paragraph.map { singleLine(plainMarkdown($0)) } ?? ""
        return line.isEmpty ? phaseLine(now: now) : line
    }

    /// The project (space id) every step names, named once over the steps.
    var commonProject: String? {
        guard let first = steps.first?.project, steps.allSatisfy({ $0.project == first }) else { return nil }
        return first
    }

    /// The SF Symbol its state wears (items.rs `work_mark`).
    var phaseSymbol: String {
        switch phase {
        case .running, .starting, .stopping: "circle.dotted"
        case .awaiting: "checklist"
        case .done: "checkmark"
        case .failed: "xmark.circle"
        case .stopped, .cancelled, .cancelling: "stop.circle"
        }
    }
}

// MARK: - Routines

/// What a routine's row says about it, most pressing first (schedule.rs
/// `JobState`).
enum BoardRoutineState: Equatable {
    case running
    /// Run now was pressed and the host has not started it yet.
    case starting
    case next(Int64)
    case manual
    /// A once-only routine that has had its run.
    case done
    /// On a schedule, but no next run is known.
    case unplanned
    case off

    init(_ routine: BotRoutine, now: Int64 = nowMs()) {
        let last = routine.lastRun
        let next = routine.enabled && routine.schedule != .manual ? routine.nextRunAt.flatMap { $0 > now ? $0 : nil } : nil
        if last?.outcome == .running {
            self = .running
        } else if let asked = routine.runRequestedAt, last.map({ $0.at < asked }) ?? true {
            self = .starting
        } else if case .once = routine.schedule, next == nil, last != nil {
            self = .done
        } else if !routine.enabled {
            self = .off
        } else if routine.schedule == .manual {
            self = .manual
        } else if let next {
            self = .next(next)
        } else {
            self = .unplanned
        }
    }

    /// The row's right-hand note.
    func note(now: Int64 = nowMs()) -> String {
        switch self {
        case .running: "running"
        case .starting: "starting…"
        case .next(let at): BotTime.next(at, now: now)
        case .manual: "manual"
        case .done: "done"
        case .unplanned: "—"
        case .off: "off"
        }
    }

    /// Drawn faded: switched off, or a reminder that has come.
    var dim: Bool { self == .off || self == .done }

    /// Running work first, then by next run, then the manual ones, then the
    /// done and the switched-off ones (schedule.rs `order_jobs`).
    var rank: (Int, Int64) {
        switch self {
        case .running: (0, 0)
        case .starting: (1, 0)
        case .next(let at): (2, at)
        case .manual, .unplanned: (3, 0)
        case .done: (4, 0)
        case .off: (5, 0)
        }
    }

    static func ordered(_ routines: [BotRoutine], now: Int64 = nowMs()) -> [BotRoutine] {
        routines.sorted { a, b in
            let (x, y) = (BoardRoutineState(a, now: now).rank, BoardRoutineState(b, now: now).rank)
            if x != y { return x < y }
            return a.name.lowercased() < b.name.lowercased()
        }
    }
}

extension BotRoutine {
    /// Where and when it runs: "This workspace · Daily 09:00",
    /// "cypher · Weekdays 18:30" (schedule.rs `job_sub`).
    func whereAndWhen(projectName: (String) -> String?) -> String {
        let lead = target.spaceId.map { projectName($0) ?? "A project" } ?? "This workspace"
        return "\(lead) · \(schedule.describe)"
    }
}

// MARK: - Projects

/// One granted project on the overview: its sessions in the workspace's
/// scope, and how many of them run or ask (board.rs `ProjectLoad`).
struct BoardProjectLoad: Identifiable, Equatable {
    var space: Space
    var sessions = 0
    var running = 0
    var asking = 0

    var id: String { space.id }

    /// "1 asking · 2 running", else how many sessions it holds.
    var note: String {
        var parts: [String] = []
        if asking > 0 { parts.append("\(asking) asking") }
        if running > 0 { parts.append("\(running) running") }
        if parts.isEmpty { return sessions == 1 ? "1 session" : "\(sessions) sessions" }
        return parts.joined(separator: " · ")
    }
}

// MARK: - Activity

/// An entry split for reading (activity.rs `Parts`): what was done (`verb`,
/// muted), what it was done to (`subject`, full strength) and the rest
/// (`detail`: where, why, how much; a quieter second line). Parsed from the
/// sentence the engine wrote; one that fits no pattern is all `verb`.
struct ActivityParts: Equatable {
    var verb: String
    var subject: String?
    var detail: String?

    /// Longest text taken as a subject; a longer one is a detail.
    static let subjectMax = 80

    init(verb: String, subject: String? = nil, detail: String? = nil) {
        self.verb = verb
        self.subject = subject
        self.detail = detail
    }

    init(_ text: String) {
        let text = singleLine(text).replacingOccurrences(of: "`", with: "")
            .trimmingCharacters(in: .whitespaces)
        // `"Morning brief" failed: why`, `"Ledger" (Dana's workspace) disconnected`
        if case let (subject, rest)? = Self.quoted(text[...]) {
            let rest = rest.trimmingCharacters(in: .whitespaces)
            let (word, more) = rest.firstIndex(of: " ").map { (rest[..<$0], rest[$0...]) } ?? (rest[...], ""[...])
            self.init(
                verb: Self.capitalized(String(word).trimmingCharacters(in: CharacterSet(charactersIn: ":"))),
                subject: subject, detail: Self.tail(more))
            return
        }
        let quote = text.firstIndex { $0 == "\"" || $0 == "“" }
        let colon = text.range(of: ": ")?.lowerBound
        // `Ran "Nightly tests"`, `Started "Fix it" in web on Mac mini`
        if let quote, colon.map({ quote < $0 }) ?? true, case let (subject, rest)? = Self.quoted(text[quote...]) {
            let verb = text[..<quote].trimmingCharacters(in: .whitespaces)
            self.init(
                verb: verb.hasSuffix(":") ? String(verb.dropLast()) : verb, subject: subject,
                detail: Self.tail(rest))
            return
        }
        if let colon {
            let verb = text[..<colon].trimmingCharacters(in: .whitespaces)
            let rest = text[text.index(colon, offsetBy: 2)...].trimmingCharacters(in: .whitespaces)
            // `Declined after review: "Ledger" (…) asked …`
            if case let (subject, more)? = Self.quoted(rest[...]) {
                self.init(verb: verb, subject: subject, detail: Self.tail(more))
                return
            }
            // A memory file: `Created: /workspace/x.md`,
            // `Added 3 lines, removed 1: /workspace/MEMORY.md`.
            if rest.hasPrefix("/"), !rest.contains(" ") {
                let (name, dir) = Self.pathParts(rest)
                let edit = ["Added ", "Removed ", "Changed "].contains { verb.hasPrefix($0) }
                let detail: String? =
                    edit ? (dir.map { "\(verb) · \($0)" } ?? verb) : dir
                self.init(verb: edit ? "Edited" : verb, subject: name, detail: detail)
                return
            }
            let short = rest.count <= Self.subjectMax && !rest.contains(". ")
            self.init(verb: verb, subject: short ? rest : nil, detail: short ? nil : rest)
            return
        }
        // `Created worktree /…/swift-onyx (cypher/swift-onyx) in invoice-demo`
        if let at = text.range(of: " /") {
            let pathStart = text.index(after: at.lowerBound)
            let pathEnd = text[pathStart...].firstIndex(of: " ") ?? text.endIndex
            self.init(
                verb: String(text[..<at.lowerBound]), subject: Self.pathParts(String(text[pathStart..<pathEnd])).name,
                detail: Self.tail(text[pathEnd...]))
            return
        }
        self.init(verb: text)
    }

    /// `"Name" (Owner's workspace)` (or in curly quotes) at the start of
    /// `text`: the subject ("Name · Owner") and what follows.
    private static func quoted(_ text: Substring) -> (String, Substring)? {
        guard let open = text.first, open == "\"" || open == "“" else { return nil }
        let closeMark: Character = open == "“" ? "”" : "\""
        let inner = text.dropFirst()
        guard let end = inner.firstIndex(of: closeMark) else { return nil }
        var name = String(inner[..<end])
        var rest = inner[inner.index(after: end)...]
        if rest.hasPrefix(" ("), let close = rest.firstIndex(of: ")") {
            let inside = rest[rest.index(rest.startIndex, offsetBy: 2)..<close]
            for suffix in ["'s workspace", "’s workspace"] where inside.hasSuffix(suffix) {
                name = "\(name) · \(inside.dropLast(suffix.count))"
                rest = rest[rest.index(after: close)...]
                break
            }
        }
        return (name, rest)
    }

    /// What follows a subject: its detail, without the joining colon.
    private static func tail(_ rest: Substring) -> String? {
        var rest = rest.trimmingCharacters(in: .whitespaces)
        while rest.hasPrefix(":") { rest.removeFirst() }
        rest = rest.trimmingCharacters(in: .whitespaces)
        return rest.isEmpty ? nil : rest
    }

    private static func capitalized(_ word: String) -> String {
        word.prefix(1).uppercased() + word.dropFirst()
    }

    /// A path's last part and the folder it is in.
    private static func pathParts(_ path: String) -> (name: String, dir: String?) {
        var path = Substring(path)
        while path.hasSuffix("/") { path.removeLast() }
        guard let slash = path.lastIndex(of: "/"), path.index(after: slash) < path.endIndex else {
            return (String(path), nil)
        }
        let dir = path[..<slash]
        return (String(path[path.index(after: slash)...]), dir.isEmpty ? nil : String(dir))
    }
}

extension BotActivity {
    var parts: ActivityParts { ActivityParts(text) }

    /// A failure: its verb shows red.
    var failed: Bool {
        kind == .workFailed || parts.verb.lowercased().contains("failed")
    }
}

/// What an Activity row opens.
enum ActivityLink: Equatable {
    case work(String)
    /// A session the session lists show, or the workspace's own chat.
    case chat(String)
    /// A hidden thread, shown read-only.
    case thread(String)
    case project(String)
    case none

    /// Its work first, then its session, then its project
    /// (activity.rs `link_of`); `chat` says how a chat id opens, nil when
    /// this replica doesn't list it, and `hasProject` whether a space is.
    static func of(
        _ entry: BotActivity, chat: (String) -> ActivityLink?, hasProject: (String) -> Bool
    ) -> ActivityLink {
        if let work = entry.workId { return .work(work) }
        if let id = entry.chatId, let link = chat(id) { return link }
        if let space = entry.spaceId, hasProject(space) { return .project(space) }
        return .none
    }
}

/// One day of the Activity log.
struct ActivityDay: Identifiable, Equatable {
    /// "Today", "Yesterday", "Mon, Nov 3", "Nov 3, 2025".
    var title: String
    var entries: [BotActivity]

    var id: String { title }

    /// `entries` (newest first) by local day, newest day first.
    static func group(_ entries: [BotActivity], now: Int64 = nowMs(), calendar: Calendar = .current) -> [ActivityDay] {
        var days: [ActivityDay] = []
        var current: Date?
        for entry in entries {
            let day = calendar.startOfDay(for: Date(timeIntervalSince1970: TimeInterval(entry.at) / 1000))
            if day == current, !days.isEmpty {
                days[days.count - 1].entries.append(entry)
            } else {
                current = day
                days.append(ActivityDay(title: title(day, now: now, calendar: calendar), entries: [entry]))
            }
        }
        return days
    }

    private static func title(_ day: Date, now: Int64, calendar: Calendar) -> String {
        let today = calendar.startOfDay(for: Date(timeIntervalSince1970: TimeInterval(now) / 1000))
        let distance = calendar.dateComponents([.day], from: day, to: today).day ?? 0
        if distance <= 0 { return "Today" }
        if distance == 1 { return "Yesterday" }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        let sameYear = calendar.component(.year, from: day) == calendar.component(.year, from: today)
        formatter.dateFormat = sameYear ? "EEE, MMM d" : "MMM d, yyyy"
        return formatter.string(from: day)
    }
}

// MARK: - Reads over the workspace data

extension WorkspaceData {
    func boardNotice(_ bot: Bot) -> BoardNotice? {
        BoardNotice.of(
            paused: bot.paused, hostOnline: botHostOnline(bot), host: botDeviceName(bot.hostDeviceId),
            chat: botChat(bot).map(indicator(for:)) ?? .idle)
    }

    /// The Needs you rows, most pressing first; an item whose record is gone
    /// is left out.
    func boardNeedRows(_ bot: Bot) -> [BoardNeedRow] {
        needsYou(botId: bot.id).compactMap { boardNeedRow($0, bot: bot) }
    }

    func boardNeedRow(_ item: NeedsYouItem, bot: Bot) -> BoardNeedRow? {
        switch item {
        case .chat(let chatId):
            let chat = chat(id: chatId)
            return BoardNeedRow(
                item: item, symbol: "bubble.left", title: "\(singleLine(bot.name)) asked you something",
                sub: chat?.lastMessagePreview.map(singleLine) ?? "In your conversation", at: chat?.lastMessageAt)
        case .plan(let id):
            guard let work = work(id: id) else { return nil }
            let steps = work.steps.count
            return BoardNeedRow(
                item: item, symbol: "checklist", title: singleLine(work.title),
                sub: "Plan to approve · \(steps) step\(steps == 1 ? "" : "s")", at: work.createdAt)
        case .approval(let id):
            guard let approval = approval(id: id) else { return nil }
            return BoardNeedRow(
                item: item, symbol: "checkmark.shield", title: singleLine(approval.action),
                sub: singleLine(approval.reason), at: approval.createdAt)
        case .asking(let chatId, _):
            guard let chat = chat(id: chatId) else { return nil }
            let project = chat.spaceId.flatMap { id in spaces.first { $0.id == id } }?.displayName
            return BoardNeedRow(
                item: item, symbol: "questionmark.circle", title: singleLine(chat.displayTitle),
                sub: project.map { "Asking · in \($0)" } ?? "Asking", at: chat.lastMessageAt)
        case .review(let id):
            guard let exchange = exchange(id: id) else { return nil }
            let peer = connection(id: exchange.connectionId)?.peer.name ?? "a connected workspace"
            return BoardNeedRow(
                item: item, symbol: "person.2", title: singleLine(exchange.question),
                sub: "Review the answer to \(peer)", at: exchange.askedAt)
        }
    }

    /// Each granted project with its sessions in the workspace's scope:
    /// live root sessions, not archived, not excluded.
    func boardProjects(_ bot: Bot) -> [BoardProjectLoad] {
        grantedSpaces(of: bot).map { space in
            var load = BoardProjectLoad(space: space)
            for chat in chats
            where chat.spaceId == space.id && chat.isListedSession && !chat.archived
                && !bot.scope.excludeSessions.contains(chat.id)
            {
                load.sessions += 1
                switch indicator(for: chat) {
                case .working: load.running += 1
                case .awaitingInput: load.asking += 1
                default: break
                }
            }
            return load
        }
    }

    /// How the workspace acts in a project: "works directly" in its host's
    /// projects when it may, else "via sessions" when it coordinates.
    func boardProjectMode(_ bot: Bot, space: Space) -> String {
        if bot.capabilities.workDirectly && space.deviceId == bot.hostDeviceId { return "works directly" }
        return bot.capabilities.coordinate ? "via sessions" : "reads only"
    }

    /// The Connections row: "3 connected · 1 open invite", or what waits for
    /// review (its exchange ids).
    func boardConnections(_ botId: String, now: Int64 = nowMs()) -> (text: String, reviews: [String]) {
        let reviews = exchanges(of: botId).filter(\.awaitingReview).sorted { $0.askedAt < $1.askedAt }.map(\.id)
        if !reviews.isEmpty { return ("\(reviews.count) to review", reviews) }
        let live = connections(of: botId).count
        let invites = openInvites(of: botId, now: now).count
        var parts = [live == 0 ? "None yet" : "\(live) connected"]
        if invites > 0 { parts.append("\(invites) open invite\(invites == 1 ? "" : "s")") }
        return (parts.joined(separator: " · "), [])
    }

    /// How an Activity row's chat opens; nil when this replica doesn't list
    /// it.
    func activityChatLink(_ chatId: String) -> ActivityLink? {
        guard let chat = chat(id: chatId) else { return nil }
        if chat.isListedSession || chat.bot?.isMain == true { return .chat(chatId) }
        return .thread(chatId)
    }
}

// A workspace's own tools as its transcript names them (crates/proto/src/
// view/tool_chips.rs `bot_tool_kind`, `bot_chip_content`, `BotAction`):
// a chip reads as what the agent did ("Searched memory", "Proposed a plan"),
// and a turn's summary counts its acts ("Proposed a plan · read 2
// sessions"). The doc drops a workspace tool's input, so a chip's detail is
// usually empty; whatever fields a call does carry are used.

import Foundation

/// A workspace tool (and the memory tool every run gets), by what it does.
enum BotToolKind: Hashable, CaseIterable {
    case listProjects, listSessions, sessionActivity, searchSessions, readSession, projectGit
    case proposePlan, listWork, getWork, stopWork, createWorktree, startSession, waitForSession
    case answerSession, steerSession, stopSession, updateStep, finishWork
    case listRoutines, saveRoutine, runRoutine, deleteRoutine, saveProjectTask, listActivity
    case listShortcuts, saveShortcut, deleteShortcut, listConnections, askBot
    /// `memory` by its command; `memory` while the command is unknown (the
    /// doc keeps none) or one this build does not name.
    case readMemory, searchMemory, createMemory, updateMemory, deleteMemory, renameMemory, memory
    case searchHistory, readHistory

    /// The tool `name` is, with its `memory` command when known.
    init?(name: String, command: String? = nil) {
        switch name {
        case "list_projects": self = .listProjects
        case "list_sessions": self = .listSessions
        case "session_activity": self = .sessionActivity
        case "search_sessions": self = .searchSessions
        case "read_session": self = .readSession
        case "project_git": self = .projectGit
        case "propose_plan": self = .proposePlan
        case "list_work": self = .listWork
        case "get_work": self = .getWork
        case "stop_work": self = .stopWork
        case "create_worktree": self = .createWorktree
        case "start_session": self = .startSession
        case "wait_for_session": self = .waitForSession
        case "answer_session": self = .answerSession
        case "steer_session": self = .steerSession
        case "stop_session": self = .stopSession
        case "update_step": self = .updateStep
        case "finish_work": self = .finishWork
        // `list_schedule` is its name from before routines were one kind.
        case "list_routines", "list_schedule": self = .listRoutines
        case "save_routine": self = .saveRoutine
        case "run_routine": self = .runRoutine
        case "delete_routine": self = .deleteRoutine
        case "list_activity": self = .listActivity
        case "save_project_task": self = .saveProjectTask
        case "list_shortcuts": self = .listShortcuts
        case "save_shortcut": self = .saveShortcut
        case "delete_shortcut": self = .deleteShortcut
        case "list_connections": self = .listConnections
        case "ask_bot": self = .askBot
        case "memory":
            switch command {
            case "view": self = .readMemory
            case "search": self = .searchMemory
            case "create": self = .createMemory
            case "str_replace", "insert": self = .updateMemory
            case "delete": self = .deleteMemory
            case "rename": self = .renameMemory
            default: self = .memory
            }
        case "search_history": self = .searchHistory
        case "read_history": self = .readHistory
        default: return nil
        }
    }

    /// Whether the call acted (started, steered, stopped, wrote) rather than
    /// read.
    var acts: Bool {
        switch self {
        case .listProjects, .listSessions, .sessionActivity, .searchSessions, .readSession, .projectGit,
            .listWork, .getWork, .listRoutines, .listActivity, .listShortcuts, .listConnections,
            .readMemory, .searchMemory, .memory, .searchHistory, .readHistory:
            return false
        default:
            return true
        }
    }

    /// A `memory` call, whatever its command.
    var isMemory: Bool {
        [.readMemory, .searchMemory, .createMemory, .updateMemory, .deleteMemory, .renameMemory, .memory]
            .contains(self)
    }

    /// The chip's label and detail. `arg` reads one of the call's input
    /// fields; `sinceHours` its `since_hours`.
    func chipContent(arg rawArg: (String) -> String?, sinceHours: Double? = nil) -> (label: String, detail: String) {
        func arg(_ key: String) -> String? {
            rawArg(key).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.flatMap { $0.isEmpty ? nil : $0 }
        }
        func inProject(_ text: String) -> String {
            guard let project = arg("project") else { return text }
            return text.isEmpty ? "in \(project)" : "\(text) in \(project)"
        }
        func quoted(_ key: String) -> String { arg(key).map { "\"\($0)\"" } ?? "" }
        // A memory path names its file: `/project/testing.md` reads as
        // `testing.md`.
        func file(_ key: String) -> String {
            guard let path = arg(key) else { return "" }
            let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
            return trimmed.split(separator: "/").last.map(String.init) ?? trimmed
        }
        let since = sinceHours.flatMap { $0.isFinite && $0 > 0 ? $0 : nil }
        let last = { (hours: Double) -> String in
            if hours >= 48, hours.truncatingRemainder(dividingBy: 24) == 0 { return "last \(Int(hours / 24))d" }
            return "last \(Int(hours.rounded(.up)))h"
        }
        switch self {
        case .listProjects: return ("List projects", "")
        case .listSessions:
            var filters: [String] = []
            if arg("status") == "active" { filters.append("active") }
            if let since { filters.append(last(since)) }
            let text = filters.isEmpty && arg("project") == nil ? "all" : filters.joined(separator: ", ")
            return ("List sessions", inProject(text))
        case .searchSessions: return ("Search sessions", inProject(quoted("query")))
        // The tool's default window is a day.
        case .sessionActivity: return ("Checked recent activity", inProject(last(min(since ?? 24, 336))))
        case .readSession: return ("Read session", arg("session_id") ?? "")
        case .projectGit: return ("Read git state", arg("project") ?? "")
        case .proposePlan: return ("Proposed a plan", arg("title") ?? "")
        case .listWork: return ("Checked its tasks", "")
        case .getWork: return ("Read task", arg("work_id") ?? "")
        case .stopWork: return ("Stopped task", "")
        case .createWorktree: return ("Created worktree", inProject(arg("branch") ?? ""))
        case .startSession: return ("Started session", inProject(arg("title") ?? ""))
        case .waitForSession: return ("Waited on session", arg("session_id") ?? "")
        case .answerSession: return ("Answered session", arg("session_id") ?? "")
        case .steerSession: return ("Steered session", arg("session_id") ?? "")
        case .stopSession: return ("Stopped session", arg("session_id") ?? "")
        case .updateStep: return ("Updated step", arg("step") ?? "")
        case .finishWork: return ("Finished task", arg("outcome") ?? "")
        case .listRoutines: return ("Checked routines", "")
        case .saveRoutine: return ("Saved routine", arg("name") ?? "")
        case .runRoutine: return ("Ran routine", "")
        case .deleteRoutine: return ("Deleted routine", "")
        case .listActivity: return ("Checked activity", "")
        case .saveProjectTask: return ("Saved project task", inProject(arg("name") ?? ""))
        case .listShortcuts: return ("Checked shortcuts", "")
        case .saveShortcut: return ("Saved a shortcut", arg("label") ?? "")
        case .deleteShortcut: return ("Removed a shortcut", "")
        case .listConnections: return ("Checked connections", "")
        case .askBot: return ("Asked workspace", arg("bot") ?? "")
        case .readMemory: return ("Read memory", file("path"))
        case .searchMemory: return ("Searched memory", quoted("query"))
        case .createMemory: return ("Created memory", file("path"))
        case .updateMemory: return ("Updated memory", file("path"))
        case .deleteMemory: return ("Deleted memory", file("path"))
        case .renameMemory:
            let (old, new) = (file("old_path"), file("new_path"))
            return ("Renamed memory", old.isEmpty || new.isEmpty ? old + new : "\(old) → \(new)")
        case .memory: return ("Memory", file("path"))
        case .searchHistory: return ("Searched history", quoted("query"))
        case .readHistory: return ("Read a past session", "")
        }
    }

    /// The chip's glyph.
    var symbol: String {
        switch self {
        case .listProjects: "folder"
        case .listSessions, .listWork, .listRoutines, .listActivity, .listShortcuts, .listConnections:
            "list.bullet"
        case .sessionActivity: "waveform.path.ecg"
        case .searchSessions, .searchMemory, .searchHistory: "magnifyingglass"
        case .readSession, .readHistory, .waitForSession: "text.bubble"
        case .projectGit, .createWorktree: "arrow.triangle.branch"
        case .proposePlan, .getWork, .updateStep, .finishWork: "checklist"
        case .stopWork, .stopSession: "stop.circle"
        case .startSession: "play.circle"
        case .answerSession, .steerSession: "arrowshape.turn.up.right"
        case .saveRoutine, .runRoutine, .deleteRoutine, .saveProjectTask: "clock.arrow.circlepath"
        case .saveShortcut, .deleteShortcut: "bolt"
        case .askBot: "person.2"
        case .readMemory, .createMemory, .updateMemory, .deleteMemory, .renameMemory, .memory: "book.closed"
        }
    }

    /// What the call did, for a turn's summary; nil for a read.
    var action: BotAction? {
        switch self {
        case .proposePlan: .plan
        case .createWorktree: .worktree
        case .startSession: .session
        case .answerSession: .answer
        case .steerSession: .steer
        case .stopSession: .stop
        case .updateStep: .step
        case .finishWork: .finish
        case .createMemory, .updateMemory, .deleteMemory, .renameMemory: .memory
        case .saveRoutine: .routine
        case .runRoutine: .runRoutine
        case .deleteRoutine: .deleteRoutine
        case .stopWork: .stopWork
        case .saveProjectTask: .task
        case .saveShortcut: .shortcut
        case .deleteShortcut: .unshortcut
        case .askBot: .ask
        default: nil
        }
    }

    /// How a read counts in a turn's summary; nil for an act.
    var read: BotRead? {
        switch self {
        case .searchSessions, .searchMemory, .searchHistory: .search
        case .readSession, .readHistory: .session
        case .readMemory, .memory: .memory
        case .listProjects, .listSessions, .sessionActivity, .listWork, .getWork, .listRoutines, .listActivity,
            .listShortcuts, .listConnections:
            .listing
        case .projectGit: .git
        default: nil
        }
    }
}

/// What a workspace's acting tool did, in the order a summary names them.
enum BotAction: Int, CaseIterable, Comparable {
    case plan, worktree, session, answer, steer, stop, step, finish, memory, routine, runRoutine
    case deleteRoutine, stopWork, task, shortcut, unshortcut, ask

    static func < (a: BotAction, b: BotAction) -> Bool { a.rawValue < b.rawValue }

    /// "proposed a plan", "started 2 sessions".
    func phrase(_ n: Int) -> String {
        let (one, many): (String, String) =
            switch self {
            case .plan: ("proposed a plan", "proposed {n} plans")
            case .worktree: ("created a worktree", "created {n} worktrees")
            case .session: ("started a session", "started {n} sessions")
            case .answer: ("answered a session", "answered {n} questions")
            case .steer: ("steered a session", "steered sessions {n} times")
            case .stop: ("stopped a session", "stopped {n} sessions")
            case .step: ("updated a step", "updated {n} steps")
            case .finish: ("finished the task", "finished {n} tasks")
            case .memory: ("updated memory", "updated memory {n} times")
            case .routine: ("saved a routine", "saved {n} routines")
            case .runRoutine: ("ran a routine", "ran {n} routines")
            case .deleteRoutine: ("deleted a routine", "deleted {n} routines")
            case .stopWork: ("stopped work", "stopped {n} pieces of work")
            case .task: ("saved a project task", "saved {n} project tasks")
            case .shortcut: ("saved a shortcut", "saved {n} shortcuts")
            case .unshortcut: ("removed a shortcut", "removed {n} shortcuts")
            case .ask: ("asked a workspace", "asked workspaces {n} times")
            }
        return n == 1 ? one : many.replacingOccurrences(of: "{n}", with: "\(n)")
    }
}

/// How a workspace's reading tool counts in a turn's summary.
enum BotRead: Hashable {
    case search, session, memory, listing, git
}

extension RenderToolCall {
    /// The workspace tool this call is, when it is one.
    var botToolKind: BotToolKind? {
        guard tag == "unknown", let name = string("name") else { return nil }
        return BotToolKind(name: name, command: string("command"))
    }

    /// A workspace tool's chip label and detail.
    var botChipContent: (label: String, detail: String)? {
        guard let kind = botToolKind else { return nil }
        let hours: Double? =
            (fields["since_hours"] as? Double) ?? (fields["since_hours"] as? Int64).map(Double.init)
        return kind.chipContent(arg: { string($0) }, sinceHours: hours)
    }
}

/// The workspace acts and reads of a tool group, as summary segments: the
/// acts first, in the vocabulary's order ("proposed a plan", "started 2
/// sessions"), then the reads ("looked over your work", "read memory",
/// "read 2 sessions", "read git state of 1 project"). Searches are counted
/// with the group's own.
struct BotToolSummary {
    var acts: [String] = []
    var reads: [String] = []
    /// Workspace searches, for the group's search count.
    var searches = 0
    /// Calls these segments account for.
    var counted = 0

    init(_ calls: [RenderToolCall]) {
        var acted: [BotAction: Int] = [:]
        var listings = 0
        var sessions = 0
        var memory = 0
        var git = 0
        for call in calls {
            guard let kind = call.botToolKind else { continue }
            counted += 1
            if let action = kind.action {
                acted[action, default: 0] += 1
                continue
            }
            switch kind.read {
            case .search: searches += 1
            case .session: sessions += 1
            case .memory: memory += 1
            case .listing: listings += 1
            case .git: git += 1
            case nil: break
            }
        }
        acts = acted.keys.sorted().map { $0.phrase(acted[$0] ?? 0) }
        if listings > 0 { reads.append("looked over your work") }
        if memory > 0 { reads.append("read memory") }
        if sessions > 0 { reads.append(sessions == 1 ? "read 1 session" : "read \(sessions) sessions") }
        if git > 0 { reads.append(git == 1 ? "read git state of 1 project" : "read git state of \(git) projects") }
    }
}

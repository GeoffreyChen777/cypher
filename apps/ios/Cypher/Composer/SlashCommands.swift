// Slash command menu — the desktop composer's `/` menu (slash_menu.rs, the
// grouping and glyphs of settings/commands.rs, composer.rs
// render_slash_popup) as a list above the phone's composer. Commands come
// from the chat's host (`ListCommands`), and what they control from its Pi
// session (`PiSessionModes`), both relay-forwardable. Picking a row only
// rewrites the draft. A slash command is sent as ordinary Run text — the Pi
// harness intercepts its built-ins (`/compact`, `/export-html`) and runs the
// rest itself.

import SwiftUI

/// proto agent.rs `SlashCommand`.
struct SlashCommand: Hashable, Decodable {
    var name: String
    var description: String?
    var inputHint: String?

    /// What the command does, "description · <hint>" or whichever exists
    /// (composer.rs render_slash_popup) — the menu row's VoiceOver hint.
    var detail: String? {
        let description = self.description.flatMap { $0.isEmpty ? nil : $0 }
        let hint = inputHint.flatMap { $0.isEmpty ? nil : "<\($0)>" }
        let parts = [description, hint].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// settings/commands.rs `CommandGroup`: what a command is for, in menu order.
enum SlashCommandGroup: CaseIterable {
    case conversation, agentModes, subagents, skills, other, settingsShortcuts

    var title: String {
        switch self {
        case .conversation: return "Conversation"
        case .agentModes: return "Agent modes"
        case .subagents: return "Subagents"
        case .skills: return "Skills"
        case .other: return "Other"
        case .settingsShortcuts: return "Settings shortcuts"
        }
    }
}

/// crates/engine/src/pi/session_modes.rs `PiSessionModes`: the Pi plugins' per-chat switches,
/// which they keep only in the chat's Pi session file, so the chat's host
/// reads them there.
struct PiSessionModes: Decodable, Equatable {
    struct Goal: Decodable, Equatable {
        /// pi-goal's status: `active`, `paused`, `blocked`, `usage_limited`,
        /// `budget_limited` or `complete`.
        var status: String
        var text: String
    }

    var fast = false
    var codemode = true
    var orchestrate = false
    /// The chat's goal, unless none was set or it was cleared.
    var goal: Goal?

    init(fast: Bool = false, codemode: Bool = true, orchestrate: Bool = false, goal: Goal? = nil) {
        self.fast = fast
        self.codemode = codemode
        self.orchestrate = orchestrate
        self.goal = goal
    }

    private enum CodingKeys: String, CodingKey { case fast, codemode, orchestrate, goal }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        fast = try c.decodeIfPresent(Bool.self, forKey: .fast) ?? false
        // Engines from before the Scripts switch send none, which reads as on.
        codemode = try c.decodeIfPresent(Bool.self, forKey: .codemode) ?? true
        orchestrate = try c.decodeIfPresent(Bool.self, forKey: .orchestrate) ?? false
        goal = try c.decodeIfPresent(Goal.self, forKey: .goal)
    }
}

/// slash_menu.rs `Choice`: one named choice of a command, what gets typed
/// after it. `takesText` choices need more typing after them.
struct SlashChoice: Hashable {
    var value: String
    var description: String
    var takesText = false
}

/// slash_menu.rs `Badge`: what a command's row says about what it controls.
struct SlashBadge: Equatable {
    enum Tone: Equatable {
        /// Something is switched on or running.
        case on
        /// Switched off.
        case off
        /// A plain reading.
        case neutral
        /// A reading worth acting on (a nearly full context).
        case warning
    }

    var label: String
    var tone: Tone
}

/// slash_menu.rs `Facts`: what the menu knows about the current chat.
struct SlashFacts {
    /// The Pi plugins' switches; nil until the host answered (or for a side
    /// chat, which has none of its own).
    var modes: PiSessionModes?
    var context: ContextUsage?
    var runningSubagents = 0
}

/// Which list the menu shows: the commands, or one command's choices.
enum SlashLevel: Equatable {
    case commands(query: String)
    case choices(command: String, query: String)
}

/// One heading's commands, in menu order.
struct SlashSection: Equatable {
    var group: SlashCommandGroup
    var commands: [SlashCommand]
}

enum SlashMenu {
    /// settings/commands.rs `SHOWN_BY_DEFAULT`: the commands whose row says
    /// what is in effect. The desktop shows any other command only once it is
    /// turned on in Settings → Commands; the phone has no such page, so a
    /// typed name finds the host's other commands too (`sections`).
    static let shownByDefault = ["compact", "goal", "fast", "scripts", "orchestrate", "subagent-status"]

    /// The filter text while the draft is a bare `/token`: the menu belongs
    /// to the command name only, so it closes at the first whitespace (the
    /// arguments) or a second `/` (a typed path, not a command). The phone's
    /// approximation of the desktop's caret-in-first-token rule.
    static func query(in draft: String) -> String? {
        guard draft.hasPrefix("/") else { return nil }
        let rest = draft.dropFirst()
        guard !rest.contains(where: \.isWhitespace), !rest.contains("/") else { return nil }
        return String(rest)
    }

    /// slash_menu.rs `choice_token` with the caret at the end: `/command
    /// partial`, at most one word after the command and nothing after that.
    /// `prefix` is the draft up to that word.
    static func choiceToken(in draft: String) -> (command: String, query: String, prefix: String)? {
        guard draft.hasPrefix("/"), let commandEnd = draft.firstIndex(where: \.isWhitespace) else { return nil }
        let command = draft[draft.index(after: draft.startIndex)..<commandEnd]
        guard !command.isEmpty, !command.contains("/") else { return nil }
        // The argument starts after the spaces; a line break ends the command.
        let argument = draft[commandEnd...].drop(while: { $0 == " " })
        guard argument.startIndex > commandEnd, !argument.contains(where: \.isWhitespace) else { return nil }
        return (String(command), String(argument), String(draft[..<argument.startIndex]))
    }

    /// The list for `draft`, or nil when the menu is closed. A command's
    /// choices open once it is typed with a space (or picked), if the host
    /// offers it; typing a word no choice starts with (a goal's text) closes
    /// the menu.
    static func level(in draft: String, commands: [SlashCommand]) -> SlashLevel? {
        if let query = query(in: draft) { return .commands(query: query) }
        guard let token = choiceToken(in: draft),
            commands.contains(where: { $0.name == token.command }),
            !choiceRows(command: token.command, query: token.query).isEmpty
        else { return nil }
        return .choices(command: token.command, query: token.query)
    }

    /// settings/commands.rs `placement`: the command's group, and whether it
    /// only configures what a desktop Settings page already covers.
    static func placement(_ name: String) -> (group: SlashCommandGroup, advanced: Bool) {
        let name = name.lowercased()
        switch name {
        case "compact", "export-html": return (.conversation, false)
        case "goal", "fast", "scripts", "orchestrate": return (.agentModes, false)
        case "subagent-config": return (.subagents, true)
        case "subagents": return (.subagents, false)
        case _ where name.hasPrefix("subagent-"): return (.subagents, false)
        case _ where name.hasPrefix("skill:"): return (.skills, false)
        case "provider", "login", "logout", "web-search-model", "mcp": return (.settingsShortcuts, true)
        case _
        where name.hasPrefix("mcp-") || name.hasPrefix("pi-mcp") || name.hasPrefix("newapi-")
            || name.hasPrefix("llama"):
            return (.settingsShortcuts, true)
        case _ where name.hasSuffix("-config"): return (.other, true)
        default: return (.other, false)
        }
    }

    /// settings/commands.rs `icon`: what the command acts on. Settings
    /// shortcuts never reach the phone's menu, so their glyphs aren't here.
    static func icon(for name: String) -> LineIcon {
        let name = name.lowercased()
        switch name {
        case "compact": return .foldVertical
        case "export-html": return .archiveUp
        case "goal": return .flag
        case "fast": return .bolt
        case "scripts": return .code
        case "orchestrate": return .hierarchy
        case "subagents": return .users
        case "subagent-status": return .pulse
        case "subagent-retry": return .restart
        case _ where name.hasPrefix("skill:"): return .book
        default: return .command
        }
    }

    /// slash_menu.rs `choices`. (`/provider add` opens desktop Settings, and
    /// settings shortcuts never reach the phone's menu.)
    static func choices(for command: String) -> [SlashChoice] {
        switch command {
        case "orchestrate":
            return [
                SlashChoice(value: "on", description: "Let the main agent delegate to subagents"),
                SlashChoice(value: "off", description: "Use subagents only when asked"),
                SlashChoice(value: "status", description: "Show the current setting"),
            ]
        case "goal":
            // pi-goal's own completions and wording.
            return [
                SlashChoice(value: "status", description: "Show the current goal"),
                SlashChoice(value: "pause", description: "Pause the active goal"),
                SlashChoice(value: "resume", description: "Resume a stopped or budget-limited goal"),
                SlashChoice(value: "clear", description: "Clear the current goal"),
                SlashChoice(value: "edit", description: "Edit the current goal objective", takesText: true),
            ]
        default:
            return []
        }
    }

    /// slash_menu.rs `command_level`: the commands that match `query`, under
    /// their group's heading in group order, the best matches first within a
    /// group (the host's order with nothing typed). With nothing typed only
    /// `shownByDefault` is listed; a typed name also finds the host's other
    /// commands, except the ones that configure desktop Settings.
    static func sections(_ commands: [SlashCommand], query: String) -> [SlashSection] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        let ranked = commands.enumerated().compactMap { ix, command -> (rank: Int, ix: Int, command: SlashCommand)? in
            guard !placement(command.name).advanced,
                !needle.isEmpty || shownByDefault.contains(command.name)
            else { return nil }
            let name = command.name.lowercased()
            if needle.isEmpty { return (1, ix, command) }
            if name.hasPrefix(needle) { return (0, ix, command) }
            return name.contains(needle) ? (1, ix, command) : nil
        }
        .sorted { ($0.rank, $0.ix) < ($1.rank, $1.ix) }
        .map(\.command)
        return SlashCommandGroup.allCases.compactMap { group in
            let members = ranked.filter { placement($0.name).group == group }
            return members.isEmpty ? nil : SlashSection(group: group, commands: members)
        }
    }

    /// slash_menu.rs `choice_level`: the choices of `command` that start with
    /// `query`.
    static func choiceRows(command: String, query: String) -> [SlashChoice] {
        let query = query.lowercased()
        return choices(for: command).filter { $0.value.hasPrefix(query) }
    }

    /// The draft after picking `command`: the token becomes `/name `, ready
    /// for arguments (or the command's choices). Nothing is sent.
    static func accept(_ command: SlashCommand) -> String {
        "/\(command.name) "
    }

    /// The draft after picking `choice`: the word after the command becomes
    /// the choice, and a space ends it. Nothing is sent.
    static func accept(_ choice: SlashChoice, in draft: String) -> String {
        guard let token = choiceToken(in: draft) else { return draft }
        return token.prefix + choice.value + " "
    }

    /// Context use at or above this share warns (slash_menu.rs
    /// CONTEXT_WARNING).
    static let contextWarning = 0.8

    /// slash_menu.rs `command_badge`.
    static func badge(for command: String, facts: SlashFacts) -> SlashBadge? {
        func onOff(_ on: Bool) -> SlashBadge {
            on ? SlashBadge(label: "On", tone: .on) : SlashBadge(label: "Off", tone: .off)
        }
        switch command {
        case "fast": return facts.modes.map { onOff($0.fast) }
        case "scripts": return facts.modes.map { onOff($0.codemode) }
        case "orchestrate": return facts.modes.map { onOff($0.orchestrate) }
        case "goal":
            guard let goal = facts.modes?.goal else { return nil }
            return SlashBadge(label: goalStatus(goal.status), tone: goal.status == "active" ? .on : .neutral)
        case "compact":
            guard let usage = facts.context, usage.size > 0 else { return nil }
            return SlashBadge(
                label: "\(Int((usage.fraction * 100).rounded()))% used",
                tone: usage.fraction >= contextWarning ? .warning : .neutral)
        case "subagent-status":
            guard facts.runningSubagents > 0 else { return nil }
            return SlashBadge(label: "\(facts.runningSubagents) running", tone: .on)
        default:
            return nil
        }
    }

    /// slash_menu.rs `choice_in_effect`: the check on a choice's row.
    static func choiceInEffect(_ choice: SlashChoice, of command: String, facts: SlashFacts) -> Bool {
        guard command == "orchestrate", let modes = facts.modes else { return false }
        return choice.value == (modes.orchestrate ? "on" : "off")
    }

    /// slash_menu.rs `choice_summary`: the line above a command's choices,
    /// what is in effect now.
    static func choiceSummary(of command: String, facts: SlashFacts) -> String? {
        guard let modes = facts.modes else { return nil }
        switch command {
        case "orchestrate":
            return "Adaptive orchestration is \(modes.orchestrate ? "on" : "off")"
        case "goal":
            guard let goal = modes.goal else { return "No goal yet. Type one to start it." }
            return "\(goalStatus(goal.status)): \(goal.text)"
        default:
            return nil
        }
    }

    /// pi-goal's status in words.
    static func goalStatus(_ status: String) -> String {
        switch status {
        case "active": return "Running"
        case "paused": return "Paused"
        case "blocked": return "Blocked"
        case "usage_limited": return "Usage limit"
        case "budget_limited": return "Budget limit"
        case "complete": return "Done"
        default: return status.replacingOccurrences(of: "_", with: " ")
        }
    }

    /// composer.rs `slash_error_message`, for relay failures.
    static func errorMessage(_ error: Error) -> String {
        if case RelayError.rpc(let message) = error, message.lowercased().contains("unknown method") {
            return "The session's device runs an older Cypher — update it to list commands"
        }
        switch error as? RelayError {
        case .notConnected, .hostOffline, .timeout:
            return "The session's device is unreachable"
        default:
            return "Couldn't load this agent's commands"
        }
    }
}

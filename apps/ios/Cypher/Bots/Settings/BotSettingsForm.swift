// The settings form's state and rules (crates/ui/src/shell/bots/dialog.rs
// `BotDialog`): the whole definition edited at once, a new workspace's
// starting look, the devices that may host one, and the access summary
// under the permissions.

import Foundation

/// Which projects the workspace is granted: the form's three-way pick.
enum ProjectAccess: String, CaseIterable, Identifiable {
    case none, all, selected

    var id: Self { self }

    var label: String {
        switch self {
        case .none: "None"
        case .all: "All projects"
        case .selected: "Selected"
        }
    }
}

/// A device the form offers as a host.
struct BotHostOption: Identifiable, Equatable {
    var id: String
    var name: String
    var online: Bool
    var laptop: Bool
}

struct BotSettingsDraft: Equatable {
    var name = ""
    var icon: String?
    var color: String?
    /// The host to create on; fixed (and only shown) when editing.
    var hostDeviceId: String?
    var access: ProjectAccess = .selected
    var selected: [String] = []
    /// Kept as they were: the form doesn't edit single-session overrides.
    var includeSessions: [String] = []
    var excludeSessions: [String] = []
    var includeArchived = false
    var transcripts = true
    var capabilities = BotCapabilities.newBot
    var instructions = ""
    /// A new workspace's chat model; nil = the host's default.
    var model: String?

    /// An existing workspace, as saved.
    init(bot: Bot) {
        name = bot.name
        icon = bot.icon
        color = bot.color
        hostDeviceId = bot.hostDeviceId
        switch bot.scope.projects {
        case .noProjects: access = .none
        case .all: access = .all
        case .selected(let ids):
            access = .selected
            selected = ids
        }
        includeSessions = bot.scope.includeSessions
        excludeSessions = bot.scope.excludeSessions
        includeArchived = bot.scope.includeArchived
        transcripts = bot.scope.transcripts
        capabilities = bot.capabilities
        instructions = bot.instructions ?? ""
    }

    /// A new workspace: the next robot face and colour, so a few made in a
    /// row tell apart without any picking; "Selected" with nothing chosen;
    /// a new workspace's capabilities.
    init(existingCount: Int, host: String?) {
        let faces = BotFace.allCases
        icon = faces[existingCount % faces.count].rawValue
        color = BotStyle.colors[existingCount % BotStyle.colors.count].key
        hostDeviceId = host
    }

    var scope: BotScope {
        BotScope(
            projects: {
                switch access {
                case .none: .noProjects
                case .all: .all
                case .selected: .selected(selected)
                }
            }(),
            includeSessions: includeSessions, excludeSessions: excludeSessions,
            includeArchived: includeArchived, transcripts: transcripts)
    }

    var trimmedName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The instructions to save: trimmed, nil when empty.
    var savedInstructions: String? {
        let text = instructions.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }

    /// Why it can't be saved yet, or nil.
    var problem: String? {
        if let error = Bot.nameError(name) { return error }
        if instructions.count > BotLimits.instructionsMaxChars {
            return "Instructions must be \(BotLimits.instructionsMaxChars) characters or fewer."
        }
        return scope.validationError
    }

    var createRequest: CreateBotRequest {
        CreateBotRequest(
            name: trimmedName, icon: icon, color: color, instructions: savedInstructions, scope: scope, model: model,
            capabilities: capabilities)
    }

    mutating func toggle(_ spaceId: String) {
        if let ix = selected.firstIndex(of: spaceId) {
            selected.remove(at: ix)
        } else {
            selected.append(spaceId)
        }
    }

    /// Nothing beyond reading.
    var readOnly: Bool { !(capabilities.workDirectly || capabilities.coordinate || capabilities.steer) }

    /// The sessions this scope reads (bot.rs `visible_chats`): listed root
    /// sessions of granted projects or named outright, minus excluded and
    /// (unless included) archived ones.
    static func visibleSessions(_ scope: BotScope, chats: [Chat]) -> [Chat] {
        chats.filter { chat in
            guard chat.isListedSession, !scope.excludeSessions.contains(chat.id) else { return false }
            guard scope.includeArchived || !chat.archived else { return false }
            return chat.spaceId.map(scope.seesSpace) == true || scope.includeSessions.contains(chat.id)
        }
    }

    /// "Access to 12 sessions in 2 projects. Read-only." (dialog.rs).
    func accessSummary(chats: [Chat], spaces: [Space]) -> String {
        let scope = self.scope
        let sessions = Self.visibleSessions(scope, chats: chats).count
        let projects = spaces.filter { scope.seesSpace($0.id) }.count
        if sessions == 0 && projects == 0 { return "No access to any sessions yet." }
        return
            "Access to \(MemoryFormat.count(sessions, "session", "sessions")) in \(MemoryFormat.count(projects, "project", "projects")).\(readOnly ? " Read-only." : "")"
    }

    /// The counter under the instructions, once they near the limit.
    var instructionsCounter: String? {
        let count = instructions.count
        guard count >= BotLimits.instructionsMaxChars - 500 else { return nil }
        return "\(count.formatted()) / \(BotLimits.instructionsMaxChars.formatted())"
    }

    /// Devices that can host a workspace: desktop engines, not phones;
    /// online ones first, then by name.
    static func hosts(_ devices: [DeviceRow], online: (String) -> Bool) -> [BotHostOption] {
        devices.filter { !["ios", "android"].contains($0.platform) }
            .map {
                BotHostOption(
                    id: $0.id, name: $0.name, online: online($0.id), laptop: ["macos", "darwin"].contains($0.platform))
            }
            .sorted { ($0.online ? 0 : 1, $0.name.lowercased()) < ($1.online ? 0 : 1, $1.name.lowercased()) }
    }
}

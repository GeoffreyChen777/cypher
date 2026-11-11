// The projects a workspace sees, as its Projects pages list them
// (crates/ui/src/shell/bots/projects.rs, board.rs `scoped_sessions`): each
// granted project's load (the board's `BoardProjectLoad`) with its last
// activity, grouped by the device the project lives on; and one project's
// sessions, work and routines.

import Foundation

/// One project on the Projects page: its load and last activity.
struct ProjectEntry: Identifiable, Equatable {
    var load: BoardProjectLoad
    /// Its newest session's last activity.
    var lastActivity: Int64?

    var id: String { load.id }
    var space: Space { load.space }

    /// "4 sessions", "No sessions yet".
    var sessionsLine: String {
        switch load.sessions {
        case 0: "No sessions yet"
        case 1: "1 session"
        default: "\(load.sessions) sessions"
        }
    }
}

/// The projects on one device.
struct ProjectDeviceGroup: Identifiable, Equatable {
    var deviceId: String
    var projects: [ProjectEntry]

    var id: String { deviceId }
}

enum ProjectLoads {
    /// A project's sessions `scope` reads, newest activity first: its live
    /// root sessions, without workspace threads, memory passes and the
    /// sessions the scope leaves out.
    static func sessions(_ scope: BotScope, chats: [Chat], spaceId: String) -> [Chat] {
        guard scope.seesSpace(spaceId) else { return [] }
        return chats.filter {
            $0.spaceId == spaceId && $0.isListedSession && !$0.archived && !scope.excludeSessions.contains($0.id)
        }
        .sorted { ($0.lastMessageAt ?? $0.createdAt, $0.id) > ($1.lastMessageAt ?? $1.createdAt, $1.id) }
    }

    /// `loads` (in project order) grouped by device, devices in the order
    /// their first project comes.
    static func groups(_ loads: [BoardProjectLoad], lastActivity: (String) -> Int64?) -> [ProjectDeviceGroup] {
        var groups: [ProjectDeviceGroup] = []
        for load in loads {
            let entry = ProjectEntry(load: load, lastActivity: lastActivity(load.space.id))
            if let ix = groups.firstIndex(where: { $0.deviceId == load.space.deviceId }) {
                groups[ix].projects.append(entry)
            } else {
                groups.append(ProjectDeviceGroup(deviceId: load.space.deviceId, projects: [entry]))
            }
        }
        return groups
    }

    /// The workspace's work that ran in a project, newest first, at most
    /// `limit` (cancelled proposals left out).
    static func work(_ work: [BotWork], spaceId: String, limit: Int = 5) -> [BotWork] {
        Array(
            work.filter { $0.state != .cancelled && $0.steps.contains { $0.project == spaceId } }
                .sorted { at($0) > at($1) }
                .prefix(limit))
    }

    /// The workspace's routines that run in a project, by name.
    static func routines(_ routines: [BotRoutine], spaceId: String) -> [BotRoutine] {
        routines.filter { $0.target.spaceId == spaceId }
            .sorted { ($0.name.lowercased(), $0.id) < ($1.name.lowercased(), $1.id) }
    }

    /// When a work item last moved: its end, else its start, else its
    /// proposal.
    static func at(_ work: BotWork) -> Int64 {
        work.endedAt ?? work.startedAt ?? work.createdAt
    }
}

extension WorkspaceData {
    /// The Projects page: the workspace's granted projects by device.
    func projectGroups(_ bot: Bot) -> [ProjectDeviceGroup] {
        ProjectLoads.groups(boardProjects(bot)) { spaceId in
            ProjectLoads.sessions(bot.scope, chats: chats, spaceId: spaceId).first
                .map { $0.lastMessageAt ?? $0.createdAt }
        }
    }
}

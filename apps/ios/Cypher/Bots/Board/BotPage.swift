// The board's pages: what a workspace's board can show, one value per page
// so a NavigationStack pushes them and a launch argument can name them.

import Foundation

enum BotPage: Hashable {
    /// The board's front: what needs you, work, routines, projects,
    /// connections and memory at a glance.
    case overview
    case needsYou
    /// Every plan and orchestration.
    case work
    /// One plan or orchestration (work id).
    case workItem(String)
    /// An action awaiting approval (approval id).
    case approval(String)
    /// A session the workspace started, asking.
    case asking(chatId: String, workId: String)
    case routines
    case routine(String)
    /// New (nil) or edit a routine.
    case routineForm(routineId: String?)
    /// The projects the workspace sees.
    case projects
    /// One project (space id) as the workspace sees it.
    case project(String)
    /// The audit log.
    case activity
    case connections
    case connection(String)
    case exchange(String)
    /// Create an invite code, or paste one.
    case connect
    case memory(MemoryScope)
    case memoryFile(MemoryScope, path: String)

    /// The page that settles a Needs you item; nil for the workspace's own
    /// question, which is answered in its chat.
    static func settling(_ item: NeedsYouItem) -> BotPage? {
        switch item {
        case .chat: nil
        case .approval(let id): .approval(id)
        case .asking(let chatId, let workId): .asking(chatId: chatId, workId: workId)
        case .plan(let id): .workItem(id)
        case .review(let id): .exchange(id)
        }
    }

    /// The launch rig's `-sheet board:<spec>` (with `-route bot:<id>`):
    /// `overview`, `needs`, `work`, `work:<id>`, `approval:<id>`,
    /// `asking:<workId>:<chatId>`, `routines`, `routine:<id>`,
    /// `routine-form[:<id>]`, `projects`, `project:<spaceId>`, `activity`,
    /// `connections`, `connection:<id>`, `exchange:<id>`, `connect`,
    /// `memory[:you|:project:<spaceId>]` (the workspace's own by default) and
    /// `memory-file:<path>` (in the workspace's memory). Takes the whole
    /// sheet value, `board:` included.
    init?(launchSpec: String, botId: String) {
        guard launchSpec.hasPrefix("board:") else { return nil }
        let parts = launchSpec.dropFirst("board:".count).split(separator: ":", maxSplits: 2).map(String.init)
        guard let head = parts.first else { return nil }
        let arg = parts.count > 1 ? parts[1] : nil
        let rest = parts.count > 2 ? parts[2] : nil
        switch (head, arg) {
        case ("overview", nil): self = .overview
        case ("needs", nil): self = .needsYou
        case ("work", nil): self = .work
        case ("work", let id?): self = .workItem(id)
        case ("approval", let id?): self = .approval(id)
        case ("asking", let workId?):
            guard let rest else { return nil }
            self = .asking(chatId: rest, workId: workId)
        case ("routines", nil): self = .routines
        case ("routine", let id?): self = .routine(id)
        case ("routine-form", let id): self = .routineForm(routineId: id)
        case ("projects", nil): self = .projects
        case ("project", let id?): self = .project(id)
        case ("activity", nil): self = .activity
        case ("connections", nil): self = .connections
        case ("connection", let id?): self = .connection(id)
        case ("exchange", let id?): self = .exchange(id)
        case ("connect", nil): self = .connect
        case ("memory", nil): self = .memory(.workspace(botId: botId))
        case ("memory", "you"): self = .memory(.you)
        case ("memory", "project"):
            guard let rest else { return nil }
            self = .memory(.project(spaceId: rest))
        case ("memory-file", let path?):
            // The split stops at two colons; the rest is the path's own.
            self = .memoryFile(.workspace(botId: botId), path: rest.map { "\(path):\($0)" } ?? path)
        default: return nil
        }
    }
}

extension BotPage: Identifiable {
    var id: Self { self }
}

// Notification wire and persisted types: APNs payload, badge, preferences and
// device registration state.

import Foundation

struct NotificationBadge: Decodable, Equatable {
    let scope: String
    let badgeCount: Int
    let badgeRevision: Int

    var valid: Bool {
        scope.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil && badgeCount >= 0
            && badgeCount <= Int(Int32.max) && badgeRevision >= 0 && badgeRevision <= 9_007_199_254_740_991
    }
    static func parse(_ userInfo: [AnyHashable: Any]) -> NotificationBadge? {
        guard let fields = userInfo["cypher"] as? [String: Any], fields["version"] as? Int == 1,
            let data = try? JSONSerialization.data(withJSONObject: fields),
            let badge = try? JSONDecoder().decode(Self.self, from: data), badge.valid
        else { return nil }
        return badge
    }
}

enum NotificationMode: String, Codable, CaseIterable, Identifiable {
    case smart, actionable, always, off
    var id: String { rawValue }
    var label: String {
        switch self {
        case .smart: return "Smart"
        case .actionable: return "Needs my attention"
        case .always: return "Always notify"
        case .off: return "Off"
        }
    }
}
struct NotificationPreferences: Codable, Equatable {
    var mode: NotificationMode = .always
    var completed = true
    var failed = true
    var input = true
    var subagents = false
    var mutedProjects: [String] = []

    func permits(_ payload: PushPayload) -> Bool {
        guard !mutedProjects.contains(payload.projectId) else { return false }
        switch payload.kind {
        case "completed": return completed
        case "failed": return failed
        case "input": return input
        default: return false
        }
    }
}

/// The Needs you item a workspace's `input` notice announces (the Worker's
/// `itemKind` / `itemId`).
enum PushItem: Equatable, Sendable {
    /// A plan awaiting approval (work id).
    case plan(String)
    /// An action awaiting approval (approval id).
    case approval(String)
    /// A peer's answer awaiting review (exchange id).
    case review(String)
    /// One of the workspace's hidden threads asking (its chat id): a plan's
    /// work thread, a routine's thread or a project routine's session; or a
    /// session one of its plans started, for its own question or one in a
    /// subagent's chat under it.
    case asking(String)

    /// nil for a kind this build doesn't know: the tap opens the workspace.
    init?(kind: String, id: String) {
        switch kind {
        case "plan": self = .plan(id)
        case "approval": self = .approval(id)
        case "review": self = .review(id)
        case "asking": self = .asking(id)
        default: return nil
        }
    }
}

struct PushPayload: Identifiable, Equatable, Sendable {
    let eventId: String
    let scope: String
    let chatId: String
    /// A session's project; for a workspace's own chat, the workspace (bot
    /// id), so it mutes like a project.
    let projectId: String
    let kind: String
    /// Set (a bot id) when the notice is a workspace's own chat's.
    var workspaceId: String? = nil
    /// What on the workspace's Needs you the notice is about.
    var item: PushItem? = nil
    var id: String { eventId }
    var title: String {
        switch kind {
        case "completed": return "Task completed"
        case "failed": return "Task failed"
        default: return "Your input is needed"
        }
    }
    static func parse(_ userInfo: [AnyHashable: Any]) -> PushPayload? {
        guard let data = userInfo["cypher"] as? [String: Any],
            data["version"] as? Int == 1,
            let scope = data["scope"] as? String, scope.range(of: "^[a-f0-9]{64}$", options: .regularExpression) != nil,
            let event = data["eventId"] as? String, UUID(uuidString: event) != nil,
            let chat = data["chatId"] as? String, validID(chat),
            let project = data["projectId"] as? String, validID(project),
            let kind = data["kind"] as? String, ["completed", "failed", "input"].contains(kind)
        else { return nil }
        // A workspace id that isn't a plain id is a foreign shape, not a
        // session notice: routing it as a session would open the wrong place.
        var workspace: String?
        if let value = data["workspaceId"] {
            guard let id = value as? String, validID(id) else { return nil }
            workspace = id
        }
        // The item only narrows where a workspace tap lands: a kind this
        // build doesn't know, or a malformed id, still opens the workspace.
        var item: PushItem?
        if workspace != nil, let kind = data["itemKind"] as? String, let id = data["itemId"] as? String, validID(id) {
            item = PushItem(kind: kind, id: id)
        }
        return PushPayload(
            eventId: event, scope: scope, chatId: chat, projectId: project, kind: kind,
            workspaceId: workspace, item: item)
    }
    private static func validID(_ id: String) -> Bool {
        id.range(of: "^[A-Za-z0-9_-]{1,128}$", options: .regularExpression) != nil
    }
}

struct PushBinding: Codable {
    var account: String
    var baseURL: URL
    var scope: String
    var bindingId: String
    var lease: String
    var epoch: Int
}
struct PushRevocation: Codable, Identifiable {
    var id = UUID()
    var binding: PushBinding
    var epoch: Int
}
struct PushRegistrationState: Codable {
    var installationId = UUID().uuidString.lowercased()
    var epoch = 0
    /// Optional for compatibility with installations saved before read ACKs.
    /// The server keys activity by installation, so sequence must survive
    /// relaunch/account rebind instead of restarting at one.
    var activitySequence: Int?
    var token: String?
    var binding: PushBinding?
    var revocations: [PushRevocation] = []

    mutating func nextEpoch() -> Int {
        epoch += 1
        return epoch
    }
    mutating func retire() {
        guard let binding else { return }
        let epoch = nextEpoch()
        revocations.append(PushRevocation(binding: binding, epoch: epoch))
        self.binding = nil
        // Revocations are capabilities, not auth tokens; keep them across
        // logout for retry without refreshing the signed-out account.
        revocations = Array(revocations.suffix(32))
    }
}

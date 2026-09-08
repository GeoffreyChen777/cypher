import Foundation

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
    var mode: NotificationMode = .smart
    var completed = true
    var failed = true
    var input = true
    var subagents = false
    var mutedProjects: [String] = []

    func permits(_ payload: PushPayload) -> Bool {
        guard mode != .off, !mutedProjects.contains(payload.projectId) else { return false }
        switch payload.kind {
        case "completed": return completed && mode != .actionable
        case "failed": return failed
        case "input": return input
        default: return false
        }
    }
}

struct PushPayload: Identifiable, Equatable, Sendable {
    let eventId: String
    let scope: String
    let chatId: String
    let projectId: String
    let kind: String
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
              let kind = data["kind"] as? String, ["completed", "failed", "input"].contains(kind) else { return nil }
        return PushPayload(eventId: event, scope: scope, chatId: chat, projectId: project, kind: kind)
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

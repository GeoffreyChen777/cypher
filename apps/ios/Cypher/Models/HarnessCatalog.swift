// Display metadata only. Available models always come from the target engine.
import Foundation
import Observation

struct HarnessInfo: Identifiable, Hashable {
    let id: String
    let label: String
}

struct ModelInfo: Identifiable, Hashable {
    let id: String
    let label: String
    let description: String?
    let reasoningLevels: [String]
}

enum HarnessCatalog {
    static let harnesses = [HarnessInfo(id: "pi", label: "Pi")]
    static func label(for harness: String) -> String {
        ["pi": "Pi", "claude-code": "Claude Code", "codex": "Codex"][harness] ?? harness
    }

    static func defaultReasoning(for model: ModelInfo) -> String? {
        if model.reasoningLevels.contains("xhigh") { return "xhigh" }
        if model.reasoningLevels.contains("high") { return "high" }
        return model.reasoningLevels.first
    }

    static func reasoningLabel(_ level: String) -> String {
        level == "xhigh" ? "X-High" : level.capitalized
    }

    static func modelLabel(harness: String, modelId: String?) -> String {
        modelId ?? "Select model"
    }

    // MARK: Providers (the desktop picker's rail, ported)

    /// One provider group of the catalog, in first-appearance order.
    struct ProviderGroup: Identifiable, Equatable {
        let id: String
        let name: String
        let models: [ModelInfo]
    }

    /// Model ids are `provider/model` (the engine prefixes them); a bare or
    /// `unknown` prefix files under "Other" — same rule as the desktop.
    static func providerId(of modelId: String) -> String {
        guard let slash = modelId.firstIndex(of: "/") else { return "other" }
        let provider = String(modelId[..<slash])
        return provider.isEmpty || provider == "unknown" ? "other" : provider
    }

    static func providerName(_ id: String) -> String {
        switch id {
        case "anthropic", "claude-code", "claude-bridge": return "Claude"
        case "openai-codex", "openai": return "ChatGPT"
        case "demo": return "Demo"
        case "other": return "Other"
        default: return id
        }
    }

    /// The harness key `HarnessBadge` draws a brand mark for; nil = no mark.
    static func providerBadgeHarness(_ id: String) -> String? {
        switch id {
        case "anthropic", "claude-code", "claude-bridge": return "claude-code"
        case "openai-codex", "openai": return "codex"
        default: return nil
        }
    }

    static func providerGroups(_ models: [ModelInfo]) -> [ProviderGroup] {
        var order: [String] = []
        var byProvider: [String: [ModelInfo]] = [:]
        for model in models {
            let id = providerId(of: model.id)
            if byProvider[id] == nil { order.append(id) }
            byProvider[id, default: []].append(model)
        }
        return order.map { ProviderGroup(id: $0, name: providerName($0), models: byProvider[$0] ?? []) }
    }

    /// The engine describes a model as "<provider> · <context>" (or just the
    /// provider). Inside a provider group only the context is news.
    static func contextLabel(_ model: ModelInfo) -> String? {
        guard let description = model.description else { return nil }
        guard let range = description.range(of: " · ") else {
            return description == providerId(of: model.id) ? nil : description
        }
        let rest = description[range.upperBound...].trimmingCharacters(in: .whitespaces)
        return rest.isEmpty ? nil : rest
    }

    /// Used only in explicitly offline demo mode, never a network fallback.
    static let demoModels = [
        ModelInfo(id: "demo/pi", label: "Pi demo model",
                  description: "Offline demonstration", reasoningLevels: ["low", "medium", "high"]),
    ]
}

enum PiCatalogError: Error, Equatable {
    case unavailable, runtimeUnavailable, noModels

    var message: String {
        switch self {
        case .unavailable:
            return "Couldn't load Pi from this device. Check its connection and retry."
        case .runtimeUnavailable:
            return "Pi isn't ready on this device. Open desktop Settings → Agents, select this device, and install or enable Pi Runtime."
        case .noModels:
            return "No Pi models are available. Open desktop Settings → Providers for this device to configure a provider, then retry."
        }
    }
}

struct PiHarnessDescriptor: Decodable {
    let id: String
    let installed: Bool?
    let enabled: Bool?

    var available: Bool {
        id == "pi" && installed == true && (enabled ?? true)
    }
}

/// One view's device-scoped catalog. A later load invalidates older replies,
/// including uncooperative/cancelled transports. No cross-device fallback.
@MainActor @Observable
final class RemotePiCatalog {
    private(set) var deviceId: String?
    private(set) var models: [ModelInfo] = []
    private(set) var loading = false
    private(set) var error: PiCatalogError?
    private var generation = UUID()

    func models(for deviceId: String) -> [ModelInfo] {
        self.deviceId == deviceId && !loading ? models : []
    }

    func load(deviceId: String, fetch: (String) async throws -> [ModelInfo]) async {
        let ticket = UUID()
        generation = ticket
        self.deviceId = deviceId
        models = []
        error = nil
        loading = true
        defer {
            if generation == ticket { loading = false }
        }
        do {
            let result = try await fetch(deviceId)
            guard generation == ticket, !Task.isCancelled else { return }
            models = result
            error = result.isEmpty ? .noModels : nil
        } catch {
            guard generation == ticket, !Task.isCancelled else { return }
            self.error = (error as? PiCatalogError) ?? .unavailable
        }
    }
}

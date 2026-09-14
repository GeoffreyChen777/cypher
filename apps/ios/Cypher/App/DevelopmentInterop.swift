#if CYPHER_DEVELOPMENT
import Foundation

/// Opt-in, bounded cloud interop probe in the actual Dev App. No production
/// build includes this runner or its remote command initiation.
@MainActor
enum DevelopmentInterop {
    static let chatId = "development-interop"
    static func run(model: AppModel) async {
        let resultURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("development-interop.json")
        func result(_ status: String, _ detail: String) {
            let body: [String: Any] = ["status": status, "detail": detail, "chatId": chatId,
                                      "userId": model.storedUserId, "orgId": model.storedOrgId]
            if let data = try? JSONSerialization.data(withJSONObject: body, options: .sortedKeys) {
                try? data.write(to: resultURL, options: .atomic)
            }
        }
        result("running", "waiting for desktop registry")
        guard await wait({ model.workspace?.chats.contains(where: { $0.id == chatId }) == true }),
              let chat = model.workspace?.chats.first(where: { $0.id == chatId }),
              let store = model.sessionStore(for: chat) else {
            result("failed", "desktop chat did not arrive"); return
        }
        model.launchRoute = .chat(chatId)
        do {
            let models = try await model.workspace!.listModels(deviceId: chat.deviceId, harness: "mock")
            guard !models.isEmpty else { result("failed", "desktop model catalog was empty"); return }
        } catch {
            result("failed", "iOS to desktop DeviceRoom RPC failed"); return
        }
        guard await wait({ store.entries.contains { $0.role == .assistant && !$0.parts.isEmpty } }) else {
            result("failed", "desktop transcript did not arrive"); return
        }
        result("desktop-received", "desktop transcript decoded by iOS")
        let assistants = store.entries.filter { $0.role == .assistant }.count
        // A nonce proves this run was not just the previous cached transcript.
        let marker = "ios-dev-interop-" + UUID().uuidString.lowercased()
        guard store.sendRun(prompt: marker, chat: chat) else {
            result("failed", "iOS command not enqueued"); return
        }
        guard await wait({ store.entries.filter { $0.role == .assistant && !$0.parts.isEmpty }.count > assistants }) else {
            result("failed", "desktop did not answer the iOS command"); return
        }
        result("passed", marker)
    }
    private static func wait(_ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(45)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 250_000_000)
        }
        return false
    }
}
#endif

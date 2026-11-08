// Dev App cloud interop probe: drives one desktop-hosted chat end to end and
// writes the outcome to Documents/development-interop.json.

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
                let body: [String: Any] = [
                    "status": status, "detail": detail, "chatId": chatId,
                    "userId": model.storedUserId, "orgId": model.storedOrgId,
                ]
                if let data = try? JSONSerialization.data(withJSONObject: body, options: .sortedKeys) {
                    try? data.write(to: resultURL, options: .atomic)
                }
            }
            result("running", "waiting for desktop registry")
            guard await wait({ model.workspace?.chats.contains(where: { $0.id == chatId }) == true }),
                let chat = model.workspace?.chats.first(where: { $0.id == chatId }),
                let store = model.sessionStore(for: chat)
            else {
                result("failed", "desktop chat did not arrive")
                return
            }
            model.launchRoute = .chat(chatId)
            do {
                let models = try await model.workspace!.listModels(deviceId: chat.deviceId, harness: "mock")
                guard !models.isEmpty else {
                    result("failed", "desktop model catalog was empty")
                    return
                }
            } catch {
                result("failed", "iOS to desktop DeviceRoom RPC failed")
                return
            }
            guard await wait({ store.entries.contains { $0.role == .assistant && !$0.parts.isEmpty } }) else {
                result("failed", "desktop transcript did not arrive")
                return
            }
            result("desktop-received", "desktop transcript decoded by iOS")
            // A nonce proves this run was not just the previous cached transcript.
            let marker = "ios-dev-interop-" + UUID().uuidString.lowercased()
            guard store.sendRun(prompt: marker, chat: chat) else {
                result("failed", "iOS command not enqueued")
                return
            }
            // An answer after the prompt, not a count: the transcript is a
            // window on the session's tail, which can keep its size.
            guard await wait({ answered(marker, in: store.entries) }) else {
                result("failed", "desktop did not answer the iOS command")
                return
            }
            result("passed", marker)
        }
        private static func answered(_ prompt: String, in entries: [MessageEntry]) -> Bool {
            let asked = entries.lastIndex { entry in
                entry.role == .user
                    && entry.parts.contains { part in
                        if case .text(_, let text, _) = part { return text == prompt }
                        return false
                    }
            }
            guard let asked else { return false }
            return entries[(asked + 1)...].contains { $0.role == .assistant && !$0.parts.isEmpty }
        }

        private static func wait(_ condition: () -> Bool, seconds: TimeInterval = 45) async -> Bool {
            let deadline = Date().addingTimeInterval(seconds)
            while Date() < deadline {
                if condition() { return true }
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
            return false
        }
    }

    /// Opt-in device-trust probe (`-dev-trust-interop`): join the account's
    /// trust group with the code in `CYPHER_DEV_PAIRING_CODE`, wait for the
    /// desktop to approve, then call the desktop `CYPHER_DEV_TRUST_HOST` over
    /// the relay — sealed, now that both are paired. Writes the outcome to
    /// Documents/development-trust-interop.json.
    @MainActor
    enum DevelopmentTrustInterop {
        static func run(model: AppModel) async {
            let resultURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("development-trust-interop.json")
            func result(_ status: String, _ detail: String) {
                let body: [String: Any] = [
                    "status": status, "detail": detail,
                    "deviceId": model.deviceId,
                    "fingerprint": model.config?.trust?.fingerprint ?? "",
                ]
                if let data = try? JSONSerialization.data(withJSONObject: body, options: .sortedKeys) {
                    try? data.write(to: resultURL, options: .atomic)
                }
            }
            let environment = ProcessInfo.processInfo.environment
            guard let code = environment["CYPHER_DEV_PAIRING_CODE"], let host = environment["CYPHER_DEV_TRUST_HOST"],
                let trust = model.config?.trust, let workspace = model.workspace
            else {
                result("failed", "missing pairing code, host or session")
                return
            }
            result("running", "joining")
            await trust.sync()
            trust.join(code: code)
            guard await wait({ trust.phase == .paired || (!trust.joining && trust.error != nil) }, seconds: 120),
                trust.phase == .paired
            else {
                result("failed", "not paired: \(trust.error ?? "timed out")")
                return
            }
            result("paired", "calling the desktop")
            do {
                let models = try await workspace.listModels(deviceId: host, harness: "mock")
                result("passed", "\(models.count) models from \(host) over a sealed session")
            } catch {
                result("failed", "relay call failed: \(error.localizedDescription)")
            }
        }

        private static func wait(_ condition: () -> Bool, seconds: TimeInterval) async -> Bool {
            let deadline = Date().addingTimeInterval(seconds)
            while Date() < deadline {
                if condition() { return true }
                try? await Task.sleep(nanoseconds: 250_000_000)
            }
            return false
        }
    }
#endif

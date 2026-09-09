import Foundation
import Observation
import UIKit
import UserNotifications

@MainActor @Observable
final class NotificationController {
    private(set) var settings = NotificationPreferences()
    private(set) var available = false
    private(set) var scope: String?
    private(set) var permission = "Not requested"
    private(set) var busy = false
    private(set) var registered = false
    private(set) var badgeCount = 0
    private var badgeRevision = -1
    var error: String?
    var banner: PushPayload?
    var pendingNavigation: PushPayload?
    var navigationError: String?
    private(set) var currentChat: String?
    private var foreground = true
    private var generation = UUID()
    private var config: AppConfig?
    private var saved = PushRegistrationState()
    private var loaded = false
    private var registering = false
    private var seenEvents = Set<String>()
    private var deferredTap: PushPayload?
    private var settingsRevision = 0
    private var activityClientId = ""
    @ObservationIgnored private var heartbeat: Task<Void, Never>?
    @ObservationIgnored private var revokeTask: Task<Void, Never>?
    @ObservationIgnored private var badgeTask: Task<Void, Never>?
    // Injectable boundaries keep permission/network/keychain behavior testable
    // without registering a real token or modifying system settings.
    @ObservationIgnored var readRegistration: () -> String? = { Keychain.load(key: NotificationController.storageKey) }
    @ObservationIgnored var writeRegistration: (String) -> Bool = {
        Keychain.save($0, key: NotificationController.storageKey, thisDeviceOnly: true)
        return Keychain.load(key: NotificationController.storageKey) == $0
    }
    @ObservationIgnored var perform: (URLRequest) async throws -> (Data, URLResponse) = {
        try await URLSession.shared.data(for: $0)
    }
    @ObservationIgnored var authorization: () async -> UNAuthorizationStatus = {
        await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }
    @ObservationIgnored var requestPermission: () async throws -> Bool = {
        try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
    }
    @ObservationIgnored var registerWithOS: () -> Void = { UIApplication.shared.registerForRemoteNotifications() }
    @ObservationIgnored var clearDelivered: () -> Void = { UNUserNotificationCenter.current().removeAllDeliveredNotifications() }
    @ObservationIgnored var setBadge: (Int) async -> Void = { count in
        try? await UNUserNotificationCenter.current().setBadgeCount(count)
    }
    private static let storageKey = "push-registration-v1"

    private func load() {
        guard !loaded else { return }
        loaded = true
        if let raw = readRegistration(),
           let state = try? JSONDecoder().decode(PushRegistrationState.self, from: Data(raw.utf8)) {
            saved = state
        }
    }
    @discardableResult private func persist() -> Bool {
        guard let data = try? JSONEncoder().encode(saved),
              let text = String(data: data, encoding: .utf8) else { return false }
        let ok = writeRegistration(text)
        if !ok { error = "Couldn't save the notification registration securely." }
        return ok
    }
    private func account(_ config: AppConfig) -> String {
        "\(config.edgeURL.absoluteString)|\(config.orgId)|\(config.userId)"
    }

    func bind(_ config: AppConfig) {
        load()
        generation = UUID()
        resetBadge()
        busy = false
        activityClientId = UUID().uuidString.lowercased()
        banner = nil
        pendingNavigation = nil
        currentChat = nil
        if saved.binding?.account != account(config) {
            saved.retire()
            persist()
        }
        self.config = config
        activityClientId = saved.installationId
        scope = saved.binding?.scope
        registered = false // permission + server lease are revalidated below
        available = false
        settings = NotificationPreferences()
        seenEvents = []
        completeDeferredTap()
        heartbeat?.cancel()
        heartbeat = Task { [weak self] in
            await self?.refresh()
            while !Task.isCancelled {
                self?.reportActivity()
                try? await Task.sleep(for: .seconds(15))
            }
        }
        drainRevocations()
    }

    func disconnect() {
        load()
        generation = UUID()
        resetBadge()
        busy = false
        heartbeat?.cancel()
        heartbeat = nil
        config = nil
        saved.retire()
        persist()
        registered = false
        available = false
        scope = nil
        banner = nil
        pendingNavigation = nil
        deferredTap = nil
        currentChat = nil
        clearDelivered()
        drainRevocations()
    }

    func setForeground(_ active: Bool) {
        foreground = active
        if active {
            Task { await refresh() }
            drainRevocations()
        }
        reportActivity()
    }
    func viewing(_ chatId: String?) {
        currentChat = chatId
        reportActivity()
    }
    private func request(_ config: AppConfig, action: String, method: String = "GET", body: Data? = nil) async throws -> Data {
        guard self.config === config, !Task.isCancelled else { throw RelayError.notConnected }
        guard let token = await config.currentToken() else { throw RelayError.notConnected }
        guard self.config === config, !Task.isCancelled else { throw RelayError.notConnected }
        var req = URLRequest(url: config.edgeURL.appending(path: "registry/\(config.orgId)/notifications/\(action)"))
        req.httpMethod = method
        req.httpBody = body
        req.timeoutInterval = 15
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let (data, response) = try await perform(req)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
              data.count < 32_768 else { throw RelayError.notConnected }
        return data
    }
    func refresh() async {
        guard let config, !busy else { return }
        let ticket = generation
        settingsRevision += 1
        let revision = settingsRevision
        struct Reply: Decodable {
            let available: Bool; let scope: String; let settings: NotificationPreferences
            var badgeCount: Int?; var badgeRevision: Int?
        }
        do {
            let reply = try JSONDecoder().decode(Reply.self, from: await request(config, action: "settings"))
            guard ticket == generation, revision == settingsRevision else { return }
            available = reply.available
            scope = reply.scope
            if let count = reply.badgeCount, let revision = reply.badgeRevision {
                receiveBadge(NotificationBadge(scope: reply.scope, badgeCount: count, badgeRevision: revision))
            }
            completeDeferredTap()
            settings = reply.settings
            reportActivity()
            error = available ? nil : "Push notifications aren't configured on this server yet."
            let status = await authorization()
            guard ticket == generation, revision == settingsRevision else { return }
            updatePermission(status)
            if status == .denied, saved.binding != nil {
                saved.retire()
                registered = false
                persist()
                drainRevocations()
            }
            if available &&
                [.authorized, .provisional, .ephemeral].contains(status) {
                registerWithOS()
                await registerToken()
            }
        } catch {
            guard ticket == generation, revision == settingsRevision else { return }
            available = false
            self.error = "Notifications are unavailable. Check the server connection and retry."
        }
    }
    private func updatePermission(_ status: UNAuthorizationStatus) {
        switch status {
        case .authorized, .provisional, .ephemeral: permission = "Allowed"
        case .denied: permission = "Disabled in iOS Settings"
        default: permission = "Not requested"
        }
    }
    func enable() async {
        guard config != nil, available, !busy else { return }
        busy = true
        let ticket = generation
        defer { if ticket == generation { busy = false } }
        do {
            let granted = try await requestPermission()
            guard ticket == generation else { return }
            permission = granted ? "Allowed" : "Disabled in iOS Settings"
            if granted { registerWithOS(); await registerToken() }
        } catch { if ticket == generation { self.error = "Couldn't request notification permission." } }
    }
    func updateSettings(_ next: NotificationPreferences) async {
        guard let config, available, !busy else { return }
        busy = true
        settingsRevision += 1
        let ticket = generation
        defer { if ticket == generation { busy = false } }
        do {
            let reply = try await request(config, action: "settings", method: "PUT", body: JSONEncoder().encode(next))
            guard ticket == generation else { return }
            if let badge = try? JSONDecoder().decode(NotificationBadge.self, from: reply) { receiveBadge(badge) }
            settings = next
            error = nil
            if permission == "Allowed" {
                registerWithOS()
                await registerToken()
            }
        } catch { if ticket == generation { self.error = "Couldn't save notification settings. Please retry." } }
    }
    func receivedToken(_ data: Data) {
        load()
        let token = data.map { String(format: "%02x", $0) }.joined()
        guard (16...128).contains(data.count) else { error = "Unexpected APNs registration response."; return }
        if saved.token != token {
            saved.retire()
            registered = false
            saved.token = token
            guard persist() else { return }
        }
        drainRevocations()
        Task { await registerToken() }
    }
    func registrationFailed() { error = "APNs registration failed. Check Push signing and the network." }

    private func registerToken() async {
        guard available, !registering, let config, let token = saved.token else { return }
        guard let environment = Bundle.main.object(forInfoDictionaryKey: "CypherAPNSEnvironment") as? String,
              ["development", "production"].contains(environment) else {
            error = "The build has no valid APNs environment."; return
        }
        let ticket = generation
        registering = true
        defer {
            registering = false
            if self.config != nil && (ticket != generation || saved.token != token) {
                Task { await registerToken() }
            }
        }
        let existing = saved.binding?.account == account(config) ? saved.binding : nil
        let epoch = existing?.epoch ?? saved.nextEpoch()
        guard persist() else { return }
        struct Reply: Decodable { let scope: String; let bindingId: String; let lease: String }
        do {
            let body = try JSONSerialization.data(withJSONObject: [
                "token": token, "environment": environment, "installationId": saved.installationId, "epoch": epoch
            ])
            let reply = try JSONDecoder().decode(Reply.self, from: await request(config, action: "register", method: "POST", body: body))
            let binding = PushBinding(account: account(config), baseURL: config.edgeURL,
                                      scope: reply.scope, bindingId: reply.bindingId, lease: reply.lease, epoch: epoch)
            guard ticket == generation, saved.token == token else {
                saved.epoch = max(saved.epoch, epoch)
                let revokeEpoch = saved.nextEpoch()
                saved.revocations.append(PushRevocation(binding: binding, epoch: revokeEpoch))
                persist()
                drainRevocations()
                return
            }
            saved.binding = binding
            guard persist() else { return }
            scope = reply.scope
            registered = true
        } catch {
            if ticket == generation {
                registered = false
                // Revalidation failure can mean a server-invalidated lease.
                // Next retry uses a newer epoch; don't busy-loop registration.
                if existing != nil { saved.retire(); persist(); drainRevocations() }
                self.error = "Couldn't register this device for notifications. Retry when connected."
            }
        }
    }
    private func drainRevocations() {
        guard revokeTask == nil, !saved.revocations.isEmpty else { return }
        revokeTask = Task { [weak self] in
            guard let self else { return }
            defer { revokeTask = nil }
            for revoke in saved.revocations {
                do {
                    var req = URLRequest(url: revoke.binding.baseURL.appending(path: "notifications/revoke"))
                    req.httpMethod = "POST"
                    req.timeoutInterval = 10
                    req.setValue("application/json", forHTTPHeaderField: "Content-Type")
                    req.httpBody = try JSONSerialization.data(withJSONObject: [
                        "scope": revoke.binding.scope, "bindingId": revoke.binding.bindingId,
                        "lease": revoke.binding.lease, "epoch": revoke.epoch
                    ])
                    // No expired account token is refreshed after logout.
                    let (_, response) = try await perform(req)
                    if let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) {
                        saved.revocations.removeAll { $0.id == revoke.id }
                        persist()
                    }
                } catch { /* retry on the next foreground/connect */ }
            }
        }
    }
    private func reportActivity() {
        guard let config, available else { return }
        let ticket = generation
        // Capture navigation/foreground state synchronously. Two quick
        // navigation actions must not both report the later page when their
        // asynchronous tasks eventually start.
        let sequence = max(saved.activitySequence ?? 0, Int(nowMs())) + 1
        saved.activitySequence = sequence
        guard persist() else { return }
        let body: [String: Any] = [
            "clientId": activityClientId, "sequence": sequence, "platform": "ios", "foreground": foreground,
            "interactionAgeMs": 0, "chatId": currentChat as Any? ?? NSNull()
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body), ticket == generation else { return }
        Task {
            // Old servers omit this additive response; ordinary activity
            // reporting remains compatible during a rolling upgrade.
            struct Reply: Decodable {
                var scope: String?; var readEventIds: [String]?
                var badgeCount: Int?; var badgeRevision: Int?
            }
            guard let response = try? await request(config, action: "activity", method: "POST", body: data),
                  ticket == generation, self.config === config,
                  let reply = try? JSONDecoder().decode(Reply.self, from: response),
                  let responseScope = reply.scope, responseScope == scope else { return }
            if let count = reply.badgeCount, let revision = reply.badgeRevision {
                receiveBadge(NotificationBadge(scope: responseScope, badgeCount: count, badgeRevision: revision))
            }
            let ids = Array((reply.readEventIds ?? []).filter { UUID(uuidString: $0) != nil }.prefix(256))
            if seenEvents.count + ids.count > 512 { seenEvents = [] }
            seenEvents.formUnion(ids)
            if let banner, ids.contains(banner.eventId) { self.banner = nil }
        }
    }
    func receiveBadge(_ badge: NotificationBadge) {
        guard config != nil, badge.scope == scope, badge.valid, badge.badgeRevision > badgeRevision else { return }
        badgeRevision = badge.badgeRevision
        badgeCount = badge.badgeCount
        applySystemBadge()
    }
    private func resetBadge() {
        badgeRevision = -1
        badgeCount = 0
        applySystemBadge()
    }
    private func applySystemBadge() {
        let ticket = generation, count = badgeCount, previous = badgeTask
        // Serialize OS writes too: a delayed old-account setBadgeCount must
        // finish before logout's zero or the next account's count is applied.
        badgeTask = Task {
            await previous?.value
            guard ticket == generation, count == badgeCount else { return }
            await setBadge(count)
        }
    }
    func receive(_ payload: PushPayload, tapped: Bool) {
        if tapped && (config == nil || scope == nil) { deferredTap = payload; return }
        guard config != nil, payload.scope == scope else { return }
        if tapped { pendingNavigation = payload; return }
        guard foreground, settings.permits(payload),
              seenEvents.insert(payload.eventId).inserted else { return }
        if seenEvents.count > 512 { seenEvents = [payload.eventId] }
        guard currentChat != payload.chatId else { return }
        banner = payload
        Task {
            try? await Task.sleep(for: .seconds(6))
            if banner?.id == payload.id { banner = nil }
        }
    }
    private func completeDeferredTap() {
        guard config != nil, let scope, let pending = deferredTap else { return }
        deferredTap = nil
        if pending.scope == scope { pendingNavigation = pending }
    }
}

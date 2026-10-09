// App session root: sign-in state machine, workspace connection, and the
// per-chat session store cache. Also hosts demo mode — an offline in-memory
// dataset so the UI can be exercised without an edge deployment.

import Foundation
import Network
import Observation
import SwiftUI
import os

@MainActor
@Observable
final class AppModel {
    static let defaultPersonalOrganizationName = "Personal"

    enum Phase {
        case signedOut
        case pickingOrg(AuthTokens, [AuthOrg])
        case ready
    }

    var phase: Phase = .signedOut
    /// `restore()` has run: until then `phase` is only the initial value, not
    /// a decision (the boot splash waits on it).
    private(set) var restored = false
    var workspace: WorkspaceStore?
    var demo: DemoDataset?
    let notifications = NotificationController()
    var sessionStores: [String: SessionStore] = [:]
    /// Chats with a session store, most recently opened first. Only the
    /// first `liveSessionLimit` keep their rooms; the rest are released.
    @ObservationIgnored var recentSessionIds: [String] = []
    /// Every live store holds a chat room WebSocket, and each (re)connect is a
    /// billed DO request plus a catch-up read. Warming every chat made each
    /// foreground a 100+ request reconnect storm; the sidebar needs none of
    /// it (registry rows carry previews, activity and unread state).
    static let liveSessionLimit = 8
    var config: AppConfig?
    @ObservationIgnored var pathMonitor: NWPathMonitor?
    @ObservationIgnored var lastPathKey: String?

    // Persisted connection settings.
    @ObservationIgnored @AppStorage("edgeURL") var edgeURLString = "https://edge.letscypher.app"
    @ObservationIgnored @AppStorage("authMode") var authModeRaw = AppConfig.Mode.workos.rawValue
    @ObservationIgnored @AppStorage("userId") var storedUserId = ""
    @ObservationIgnored @AppStorage("orgId") var storedOrgId = ""
    @ObservationIgnored @AppStorage("deviceId") var storedDeviceId = ""

    var deviceId: String {
        if storedDeviceId.isEmpty {
            storedDeviceId = "ios-" + UUID().uuidString.lowercased().prefix(8)
        }
        return storedDeviceId
    }

    var deviceName: String {
        UIDevice.current.name
    }

    /// Deep-link target applied by HomeView on first appearance (set by launch
    /// args in demo mode; simulator-driven screenshots use it).
    var launchRoute: Route?
    /// Screenshot rig: "newsession" / "newspace" presents that sheet on arrival.
    var launchSheet: String?
    /// Screenshot rig: auto-send a canned prompt from the new-session canvas.
    var launchAutosend = false
    /// Screenshot rig: the session composer takes keyboard focus after ~1.5s,
    /// to drive the keyboard-up transcript states headless.
    var launchFocusComposer = false

    /// Fork retries reuse their request id (the host dedupes on it); a
    /// definite answer retires it (shell.rs fork_request_ids).
    @ObservationIgnored var forkRequestIds: [String: String] = [:]
    /// Drafts waiting for a session's composer (a fork's edited prompt).
    @ObservationIgnored var pendingDrafts: [String: String] = [:]

    func restore() {
        defer { restored = true }
        if demo != nil { return }
        DocDisk.prune(keep: 80)
        let args = ProcessInfo.processInfo.arguments
        #if CYPHER_DEVELOPMENT
        if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil { return }
        // A separate bundle owns preferences, document cache and Keychain.
        // Ignore production saved state and legacy credential launch arguments.
        if !args.contains("-demo") {
            if let token = ProcessInfo.processInfo.environment["CYPHER_DEV_ACCESS_TOKEN"],
               DevelopmentProfile.validToken(token) {
                Keychain.save(token, key: "developmentToken", thisDeviceOnly: true)
            }
            if connectDevelopment(secret: Keychain.load(key: "developmentToken")) {
                if args.contains("-dev-interop") { Task { await DevelopmentInterop.run(model: self) } }
            }
            return
        }
        #endif
        // Hard cutover: both prior production edge URLs (the old mvp-lab
        // default and the interim workers.dev default) are migrated to the
        // new canonical endpoint. Only the exact old production values
        // migrate; custom/self-hosted URLs are preserved.
        if edgeURLString == "https://cypher-edge.mvp-lab.ai"
            || edgeURLString == "https://cypher-edge.geoffreychen777.workers.dev" {
            edgeURLString = "https://edge.letscypher.app"
        }
        // Debug-rig config overrides (cfprefsd caching defeats external
        // defaults writes; the app applying them itself always sticks).
        func override(_ flag: String, _ apply: (String) -> Void) {
            if let ix = args.firstIndex(of: flag), ix + 1 < args.count {
                apply(args[ix + 1])
            }
        }
        override("-setedge") { edgeURLString = $0 }
        override("-setmode") { authModeRaw = $0 }
        override("-setuser") { storedUserId = $0 }
        override("-setorg") { storedOrgId = $0 }
        // Simulator rig: seed WorkOS tokens straight into the keychain (the
        // ASWebAuthenticationSession flow can't be driven headlessly).
        override("-setaccess") { Keychain.save($0, key: "accessToken") }
        override("-setrefresh") { Keychain.save($0, key: "refreshToken") }
        if args.contains("-demo") {
            applyDemoLaunchArguments(args)
            return
        }
        startPathMonitor()
        guard let url = URL(string: edgeURLString), !storedUserId.isEmpty, !storedOrgId.isEmpty else {
            return
        }
        let mode = AppConfig.Mode(rawValue: authModeRaw) ?? .workos
        switch mode {
        case .dev:
            connect(url: url, mode: .dev, userId: storedUserId, orgId: storedOrgId,
                    tokens: nil, devBearer: devBearer(userId: storedUserId, orgId: storedOrgId))
        case .workos:
            guard let access = Keychain.load(key: "accessToken"),
                  let refresh = Keychain.load(key: "refreshToken") else { return }
            connect(url: url, mode: .workos, userId: storedUserId, orgId: storedOrgId,
                    tokens: AuthTokens(accessToken: access, refreshToken: refresh), devBearer: nil)
        }
    }
}

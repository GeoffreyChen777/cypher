// AppModel sign-in flows: WorkOS and development connect, org selection,
// sign-out.

import Foundation
import SwiftUI

extension AppModel {
    // MARK: Sign-in flows

    #if CYPHER_DEVELOPMENT
    /// Connect to the development Edge. A loopback Edge needs no secret; a
    /// remote one needs its 64-hex secret, which is kept in the Keychain.
    @discardableResult
    func connectDevelopment(secret: String?) -> Bool {
        guard let bearer = DevelopmentProfile.bearer(secret: secret) else { return false }
        if let secret, DevelopmentProfile.validToken(secret) {
            Keychain.save(secret, key: "developmentToken", thisDeviceOnly: true)
        }
        edgeURLString = DevelopmentProfile.edge.absoluteString
        authModeRaw = AppConfig.Mode.dev.rawValue
        storedUserId = DevelopmentProfile.user
        storedOrgId = DevelopmentProfile.org
        startPathMonitor()
        connect(url: DevelopmentProfile.edge, mode: .dev, userId: DevelopmentProfile.user,
                orgId: DevelopmentProfile.org, tokens: nil, devBearer: bearer)
        return true
    }
    #endif

    /// WorkOS paste-code exchange. Multiple organizations use the picker,
    /// exactly one is selected automatically, and a first personal
    /// organization is provisioned automatically when none exists.
    func signIn(edgeURL: URL, code: String, codeVerifier: String) async throws {
        let client = AuthClient(baseURL: edgeURL)
        let (user, tokens) = try await client.exchange(code: code, codeVerifier: codeVerifier)
        try await finishSignIn(edgeURL: edgeURL, user: user, tokens: tokens)
    }

    func verifyEmail(edgeURL: URL, pendingAuthenticationToken: String, code: String) async throws {
        let client = AuthClient(baseURL: edgeURL)
        let (user, tokens) = try await client.verifyEmail(
            pendingAuthenticationToken: pendingAuthenticationToken,
            code: code
        )
        try await finishSignIn(edgeURL: edgeURL, user: user, tokens: tokens)
    }

    private func finishSignIn(edgeURL: URL, user: AuthUser, tokens: AuthTokens) async throws {
        let client = AuthClient(baseURL: edgeURL)
        edgeURLString = edgeURL.absoluteString
        authModeRaw = AppConfig.Mode.workos.rawValue
        storedUserId = user.id
        let orgs = try await client.orgs(accessToken: tokens.accessToken)
        switch OrgSelection.route(for: orgs) {
        case .autoSelect(let only):
            try await selectOrg(only, tokens: tokens)
        case .pick:
            phase = .pickingOrg(tokens, orgs)
        case .autoCreate:
            try await createOrg(name: Self.defaultPersonalOrganizationName, tokens: tokens)
        }
    }

    /// Provision the hidden personal organization, then re-scope into it via
    /// the same refresh/select path as a picked organization.
    private func createOrg(name: String, tokens: AuthTokens) async throws {
        guard let url = URL(string: edgeURLString) else { return }
        let client = AuthClient(baseURL: url)
        let org = try await client.createOrg(name: name, accessToken: tokens.accessToken)
        try await selectOrg(org, tokens: tokens)
    }

    func selectOrg(_ org: AuthOrg, tokens: AuthTokens) async throws {
        guard let url = URL(string: edgeURLString) else { return }
        // Re-scope the access token to the org (adds the org_id claim).
        let client = AuthClient(baseURL: url)
        let scoped = try await client.refresh(refreshToken: tokens.refreshToken,
                                              organizationId: org.organizationId)
        Keychain.save(scoped.accessToken, key: "accessToken")
        Keychain.save(scoped.refreshToken, key: "refreshToken")
        storedOrgId = org.organizationId
        connect(url: url, mode: .workos, userId: storedUserId, orgId: org.organizationId,
                tokens: scoped, devBearer: nil)
    }

    func enterDemoMode() {
        demo = DemoDataset.standard()
        phase = .ready
    }

    func signOut() {
        config?.invalidate()
        notifications.disconnect()
        workspace?.stop()
        workspace = nil
        sessionStores.values.forEach { $0.stop() }
        sessionStores.removeAll()
        recentSessionIds.removeAll()
        config = nil
        demo = nil
        Keychain.delete(key: "accessToken")
        Keychain.delete(key: "refreshToken")
        #if CYPHER_DEVELOPMENT
        Keychain.delete(key: "developmentToken")
        #endif
        DocDisk.wipeAll()  // local doc state belongs to the signed-in identity
        storedUserId = ""
        storedOrgId = ""
        phase = .signedOut
    }

    func devBearer(userId: String, orgId: String) -> String {
        orgId.isEmpty ? userId : "\(userId)@\(orgId)"
    }

    func connect(url: URL, mode: AppConfig.Mode, userId: String, orgId: String,
                         tokens: AuthTokens?, devBearer: String?) {
        self.config?.invalidate()
        let config = AppConfig(edgeURL: url, mode: mode, userId: userId, orgId: orgId,
                               deviceId: deviceId, deviceName: deviceName,
                               tokens: tokens, devBearer: devBearer)
        self.config = config
        if !DevelopmentProfile.enabled { notifications.bind(config) }
        let store = WorkspaceStore(config: config,
                                   pendingActivity: { [notifications] in notifications.pendingActivity })
        workspace = store
        store.start()
        phase = .ready
    }
}

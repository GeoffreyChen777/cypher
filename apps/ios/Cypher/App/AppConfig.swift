// Session-wide connection config: edge base URL, identity, token minting for
// native v3 header-authenticated sockets. Thread-safe across room actors.

import Foundation

final class AppConfig: @unchecked Sendable {
    enum Mode: String {
        case workos
        case dev
    }

    let edgeURL: URL
    let mode: Mode
    let userId: String
    let orgId: String
    let deviceId: String
    let deviceName: String

    private let lock = NSLock()
    private var tokens: AuthTokens?
    private var devBearer: String?
    private var invalidated = false
    private let refreshGate = RefreshGate()
    @MainActor private var control: Workspace3Context?

    var isActive: Bool {
        lock.lock(); defer { lock.unlock() }
        return !invalidated
    }
    @MainActor func workspaceContext() throws -> Workspace3Context {
        guard isActive else { throw RelayError.notConnected }
        if let control { return control }
        let context = try Workspace3Context(config: self)
        control = context
        return context
    }
    @MainActor func cancelWorkspaceCalls(owner: String) { control?.rpc.cancel(owner: owner) }

    /// Injectable for tests: how a fresh AuthClient is built. The production
    /// default rides URLSession.shared; tests substitute a recording client
    /// to count /auth/refresh calls deterministically.
    var makeClient: (URL) -> AuthClient = { AuthClient(baseURL: $0) }

    init(edgeURL: URL, mode: Mode, userId: String, orgId: String,
         deviceId: String, deviceName: String,
         tokens: AuthTokens? = nil, devBearer: String? = nil) {
        self.edgeURL = edgeURL
        self.mode = mode
        self.userId = userId
        self.orgId = orgId
        self.deviceId = deviceId
        self.deviceName = deviceName
        self.tokens = tokens
        self.devBearer = devBearer
    }

    /// Current bearer, refreshing the WorkOS access token when needed.
    /// Single-flight: concurrent callers sharing one expired token await the
    /// same in-flight `/auth/refresh` (a refresh token is single-use — the
    /// race this removes would rotate it N times and invalidate it on the
    /// second), and rotated tokens persist exactly once. A failed refresh
    /// never yields the known-expired bearer: transient failures return nil
    /// (refresh token preserved), permanent rejection clears every
    /// credential.
    func currentToken() async -> String? {
        switch mode {
        case .dev:
            return readDevBearer()
        case .workos:
            // Fast path: a still-fresh token needs no refresh.
            if let current = readTokens(), !Self.isExpired(jwt: current.accessToken) {
                return current.accessToken
            }
            let refreshed = await refreshGate.refresh { [self] in
                await performRefresh()
            }
            guard refreshed != nil else { return nil }
            return readTokens()?.accessToken
        }
    }

    /// One refresh attempt under the gate. Re-checks freshness (the refresh
    /// we queued behind may have done the work), rotates via `/auth/refresh`,
    /// persists once, and classifies failures: a permanent 401 clears every
    /// credential; transient failures (429/5xx/transport) preserve the
    /// refresh token and yield nil for this attempt.
    private func performRefresh() async -> AuthTokens? {
        // Re-check under the gate: the refresh we queued behind may already
        // have rotated the tokens.
        if let current = readTokens(), !Self.isExpired(jwt: current.accessToken) {
            return current
        }
        guard let current = readTokens() else { return nil }
        let client = makeClient(edgeURL)
        do {
            let refreshed = try await client.refresh(refreshToken: current.refreshToken,
                                                     organizationId: orgId)
            return persist(refreshed) ? refreshed : nil
        } catch let error as AuthError {
            if error.isPermanent {
                // The session is dead (revoked/deleted) and can never
                // recover: drop in-memory + keychain credentials so neither
                // this process nor a relaunch keeps dialing with them.
                roomLog.error("auth: refresh permanently rejected; clearing session")
                clearTokens()
            } else {
                // Transient (429/5xx/transport): keep the refresh token for a
                // later attempt; this attempt simply has no bearer.
                roomLog.error("auth: refresh failed transiently (\(error.localizedDescription))")
            }
            return nil
        } catch {
            roomLog.error("auth: refresh failed: \(error.localizedDescription)")
            return nil
        }
    }

    private func readTokens() -> AuthTokens? {
        lock.lock(); defer { lock.unlock() }
        return invalidated ? nil : tokens
    }
    private func readDevBearer() -> String? {
        lock.lock(); defer { lock.unlock() }
        return invalidated ? nil : devBearer
    }

    /// In-flight notification/sync refreshes from an old account must not
    /// repopulate Keychain after logout or overwrite a newly signed-in account.
    func invalidate() {
        lock.lock(); defer { lock.unlock() }
        invalidated = true
        tokens = nil
        devBearer = nil
        Task { @MainActor [weak self] in self?.control?.retire(); self?.control = nil }
    }

    private func persist(_ new: AuthTokens) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !invalidated else { return false }
        tokens = new
        Keychain.save(new.accessToken, key: "accessToken")
        Keychain.save(new.refreshToken, key: "refreshToken")
        return true
    }

    /// Permanent rejection: wipe in-memory and stored credentials.
    private func clearTokens() {
        lock.lock(); defer { lock.unlock() }
        guard !invalidated else { return }
        tokens = nil
        Keychain.delete(key: "accessToken")
        Keychain.delete(key: "refreshToken")
    }

    private var wsBase: URL {
        var components = URLComponents(url: edgeURL, resolvingAgainstBaseURL: false)!
        components.scheme = components.scheme == "http" ? "ws" : "wss"
        return components.url!
    }
    func workspace3Scope() throws -> Workspace3Scope {
        let scope = Workspace3Scope(endpoint: edgeURL.absoluteString, org: orgId, user: userId, actor: deviceId)
        try scope.validate()
        return scope
    }
    func workspace3Request() async throws -> URLRequest {
        _ = try workspace3Scope()
        guard let token = await currentToken() else { try Workspace3Wire.fail("reauth_required") }
        let url = wsBase.appending(path: "workspace3/\(orgId)/ws")
        guard url.scheme == "wss" || (url.scheme == "ws" && ["127.0.0.1", "localhost", "::1"].contains(url.host)) else {
            try Workspace3Wire.fail("insecure_endpoint")
        }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    /// The v3 typed event room. v3 is the only session transport; the old
    /// chat2/Loro relay is intentionally not used by new clients.
    func sync3Request(chatId: String, socket: Bool) async throws -> URLRequest {
        _ = try workspace3Scope()
        _ = try Sync3Wire.identifier(.string(orgId))
        _ = try Sync3Wire.identifier(.string(chatId))
        guard let token = await currentToken() else { throw Sync3Error.protocolError("reauth_required") }
        let base = socket ? wsBase : edgeURL
        let url = base.appending(path: "sync3/\(orgId)/chats/\(chatId)/\(socket ? "ws" : "exchange")")
        guard ["https", "wss"].contains(url.scheme) ||
                (["http", "ws"].contains(url.scheme) && ["127.0.0.1", "localhost", "::1"].contains(url.host)) else {
            throw Sync3Error.protocolError("insecure_endpoint")
        }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(userId, forHTTPHeaderField: "x-cypher-expected-user")
        return request
    }

    /// Decode the JWT payload's `exp` (60s early-refresh margin). Unparseable
    /// tokens read as non-expired — the server is the arbiter.
    private static func isExpired(jwt: String) -> Bool {
        let segments = jwt.split(separator: ".")
        guard segments.count == 3 else { return false }
        var base64 = String(segments[1]).replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        while base64.count % 4 != 0 { base64 += "=" }
        guard let data = Data(base64Encoded: base64),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let exp = obj["exp"] as? TimeInterval else { return false }
        return Date().timeIntervalSince1970 > exp - 60
    }

}

/// Single-flight refresh gate. Concurrent `currentToken` calls that all see
/// one expired token share exactly one in-flight `/auth/refresh` and await
/// its result — a refresh token is single-use, so the lock/read/unlock race
/// this replaces would rotate it once per caller and invalidate it on the
/// second. Rotated tokens therefore persist exactly once per refresh.
private actor RefreshGate {
    private var inFlight: Task<AuthTokens?, Never>?

    func refresh(_ run: @escaping @Sendable () async -> AuthTokens?) async -> AuthTokens? {
        if let inFlight {
            return await inFlight.value
        }
        let task = Task { await run() }
        inFlight = task
        defer { inFlight = nil }
        return await task.value
    }
}

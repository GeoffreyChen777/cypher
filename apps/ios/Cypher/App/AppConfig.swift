// Session-wide connection config: edge base URL, identity, token minting for
// room sockets (WS auth rides the URL query — sockets can't set headers), and
// the durable-nudge POST. Thread-safe (rooms call in from their actors).

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
    private let refreshGate = RefreshGate()

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
            lock.lock(); defer { lock.unlock() }
            return devBearer
        case .workos:
            // Fast path: a still-fresh token needs no refresh.
            if let current = readTokens(), !Self.isExpired(jwt: current.accessToken) {
                return current.accessToken
            }
            let refreshed = await refreshGate.refresh { [self] in
                await performRefresh()
            }
            return refreshed?.accessToken
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
            persist(refreshed)
            return refreshed
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
        return tokens
    }

    private func updateTokens(_ new: AuthTokens) {
        lock.lock(); defer { lock.unlock() }
        tokens = new
    }

    private func persist(_ new: AuthTokens) {
        updateTokens(new)
        Keychain.save(new.accessToken, key: "accessToken")
        Keychain.save(new.refreshToken, key: "refreshToken")
    }

    /// Permanent rejection: wipe in-memory and stored credentials.
    private func clearTokens() {
        lock.lock(); defer { lock.unlock() }
        tokens = nil
        Keychain.delete(key: "accessToken")
        Keychain.delete(key: "refreshToken")
    }

    private var wsBase: URL {
        var components = URLComponents(url: edgeURL, resolvingAgainstBaseURL: false)!
        components.scheme = components.scheme == "http" ? "ws" : "wss"
        return components.url!
    }

    /// The workspace registry room (docs/registry-sync.md) — the row-table
    /// replacement for the old ws Loro workspace doc.
    func registrySocketURL() async -> URL? {
        guard let token = await currentToken() else { return nil }
        var url = wsBase.appending(path: "registry/\(orgId)/ws")
        url.append(queryItems: [URLQueryItem(name: "token", value: token),
                                URLQueryItem(name: "device", value: deviceId)])
        return url
    }

    /// The chat2 log-relay room (docs/chat2-sync.md B) — replaces the s2
    /// session rooms, which mobile no longer dials at all. `device` rides the
    /// URL so the DO can attribute sockets and honor excludeOwn backfills.
    func chat2SocketURL(chatId: String) async -> URL? {
        guard let token = await currentToken() else { return nil }
        var url = wsBase.appending(path: "chat2/\(chatId)/ws")
        url.append(queryItems: [URLQueryItem(name: "token", value: token),
                                URLQueryItem(name: "device", value: deviceId)])
        return url
    }

    /// GET /chat2/{chatId}/checkpoint — the Range-resumable doc snapshot
    /// (auth via bearer header; the caller adds Range on resume).
    func chat2CheckpointRequest(chatId: String) async -> URLRequest? {
        guard let token = await currentToken() else { return nil }
        var request = URLRequest(url: edgeURL.appending(path: "chat2/\(chatId)/checkpoint"))
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    /// GET /chat2/{chatId}/rows?after= — the HTTPS pull twin of the Chat2
    /// backfill. Authentication stays in the Bearer header; unlike the
    /// WebSocket URL, the token never enters the query string.
    func chat2RowsRequest(chatId: String, after: UInt64) async -> URLRequest? {
        guard let token = await currentToken() else { return nil }
        var url = edgeURL.appending(path: "chat2/\(chatId)/rows")
        url.append(queryItems: [
            URLQueryItem(name: "after", value: String(after)),
            URLQueryItem(name: "device", value: deviceId)
        ])
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    /// POST /chat2/{chatId}/rows?batchId= — raw update push with server-side
    /// batch dedupe, used when the WebSocket upgrade is unavailable.
    func chat2PushRequest(chatId: String, batchId: String) async -> URLRequest? {
        guard let token = await currentToken() else { return nil }
        var url = edgeURL.appending(path: "chat2/\(chatId)/rows")
        url.append(queryItems: [
            URLQueryItem(name: "batchId", value: batchId),
            URLQueryItem(name: "device", value: deviceId)
        ])
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        return request
    }

    /// GET /registry/{orgId}/rows?since= — delta pull and HTTP presence beat.
    func registryRowsRequest(since: UInt64?) async -> URLRequest? {
        guard let token = await currentToken() else { return nil }
        var url = edgeURL.appending(path: "registry/\(orgId)/rows")
        var query = [
            URLQueryItem(name: "device", value: deviceId),
            URLQueryItem(name: "beat", value: "1")
        ]
        if let since {
            query.append(URLQueryItem(name: "since", value: String(since)))
        }
        url.append(queryItems: query)
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    /// POST /registry/{orgId}/push — JSON op batch push with LWW-safe retries.
    func registryPushRequest() async -> URLRequest? {
        guard let token = await currentToken() else { return nil }
        var url = edgeURL.appending(path: "registry/\(orgId)/push")
        url.append(queryItems: [URLQueryItem(name: "device", value: deviceId)])
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
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

    /// GET /device/{deviceId}/status → whether the device's relay HOST socket
    /// is currently attached (distinct from workspace presence).
    func deviceStatus(deviceId: String) async -> String {
        guard let token = await currentToken() else { return "no-token" }
        var request = URLRequest(url: edgeURL.appending(path: "device/\(deviceId)/status"))
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse else { return "unreachable" }
        return "http=\(http.statusCode) body=\(String(data: data, encoding: .utf8) ?? "")"
    }

    /// POST /device/{deviceId}/nudge {chatId} — wake a cold host to drain the
    /// command queue.
    func nudge(deviceId: String, chatId: String) async {
        guard let token = await currentToken() else { return }
        var request = URLRequest(url: edgeURL.appending(path: "device/\(deviceId)/nudge"))
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["chatId": chatId])
        _ = try? await URLSession.shared.data(for: request)
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

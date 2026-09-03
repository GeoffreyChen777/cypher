// Edge auth client — /auth/exchange, /auth/refresh, /auth/orgs
// (edge/src/auth-routes.ts). Two modes, mirroring the engine:
// - WorkOS: paste-code exchange → access/refresh tokens; refresh scoped to an
//   org adds the org_id claim the workspace room requires.
// - Dev (AUTH_MODE=dev edge): the bearer string IS the user id; "user@org"
//   supplies a fake org claim.

import Foundation

struct AuthUser: Codable, Equatable {
    var id: String
    var email: String?
    var firstName: String?
    var lastName: String?
    var profilePictureUrl: String?
}

struct AuthOrg: Codable, Identifiable, Equatable {
    var id: String
    var organizationId: String
    var name: String
}

struct AuthTokens: Codable, Equatable {
    var accessToken: String
    var refreshToken: String
}

enum AuthError: LocalizedError, Equatable {
    case emailVerificationRequired(pendingAuthenticationToken: String, email: String?)

    /// Non-2xx from the edge. `code`/`retryable` come from the edge's typed
    /// JSON envelope `{error, code, retryable}` when it was parseable; a raw
    /// status with an unparseable body carries `nil` for both.
    case http(Int, String, code: String?, retryable: Bool?)
    case invalidResponse
    case transport(String)

    /// A permanent auth rejection (revoked session, deleted user) is the ONLY
    /// case a client may clear its stored credentials. This depends on the
    /// edge's EXPLICIT permanent semantics — never on a bare 401:
    ///  - an explicit `retryable: false` is permanent;
    ///  - no explicit signal, but the machine `code` is `invalid_grant`
    ///    (WorkOS's explicit credential rejection) → permanent;
    ///  - a raw/ambiguous 401 with neither is treated as transient, so a
    ///    misbehaving gateway can never log a user out.
    var isPermanent: Bool {
        guard case .http(let status, _, let code, let retryable) = self else { return false }
        if let retryable { return !retryable }
        return status == 401 && code == "invalid_grant"
    }

    var isTransient: Bool { !isPermanent }

    var errorDescription: String? {
        switch self {
        case .emailVerificationRequired:
            return "Email verification required"
        case .http(let code, let body, _, _): return "Auth failed (\(code)): \(body)"
        case .invalidResponse: return "Unexpected auth response"
        case .transport(let detail): return "Auth request failed: \(detail)"
        }
    }
}

struct AuthClient {
    var baseURL: URL

    /// Injectable request transport (tests substitute a recording stub); the
    /// production default rides URLSession.shared. Non-2xx statuses surface
    /// as `.http`, transport errors as `.transport` (transient).
    var perform: @Sendable (URLRequest) async throws -> (Data, HTTPURLResponse) = { request in
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw AuthError.transport(error.localizedDescription)
        }
        guard let http = response as? HTTPURLResponse else {
            throw AuthError.invalidResponse
        }
        return (data, http)
    }

    /// Paste-code exchange. The RFC 7636 `codeVerifier` belongs to the same
    /// attempt that published its S256 `code_challenge` on the authorize URL;
    /// it is sent exactly once, with the code, and is never logged.
    func exchange(code: String, codeVerifier: String? = nil) async throws -> (AuthUser, AuthTokens) {
        struct Response: Codable {
            var user: AuthUser
            var accessToken: String
            var refreshToken: String
        }
        var body: [String: String] = ["code": code]
        if let codeVerifier { body["codeVerifier"] = codeVerifier }
        let r: Response = try await post("auth/exchange", body: body)
        return (r.user, AuthTokens(accessToken: r.accessToken, refreshToken: r.refreshToken))
    }

    func verifyEmail(pendingAuthenticationToken: String, code: String) async throws -> (AuthUser, AuthTokens) {
        struct Response: Codable {
            var user: AuthUser
            var accessToken: String
            var refreshToken: String
        }
        let r: Response = try await post(
            "auth/verify-email",
            body: [
                "pendingAuthenticationToken": pendingAuthenticationToken,
                "code": code
            ]
        )
        return (r.user, AuthTokens(accessToken: r.accessToken, refreshToken: r.refreshToken))
    }

    func refresh(refreshToken: String, organizationId: String? = nil) async throws -> AuthTokens {
        var body: [String: String] = ["refreshToken": refreshToken]
        if let organizationId { body["organizationId"] = organizationId }
        return try await post("auth/refresh", body: body)
    }

    func orgs(accessToken: String) async throws -> [AuthOrg] {
        struct Response: Codable { var orgs: [AuthOrg] }
        var request = URLRequest(url: baseURL.appending(path: "auth/orgs"))
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        let (data, http) = try await perform(request)
        try Self.check(statusCode: http.statusCode, data: data)
        return try JSONDecoder().decode(Response.self, from: data).orgs
    }

    /// POST /auth/orgs — create a workspace and make the caller its first
    /// (admin) member (the edge's createOrg: WorkOS org + membership).
    func createOrg(name: String, accessToken: String) async throws -> AuthOrg {
        struct Response: Codable { var organizationId: String }
        var request = URLRequest(url: baseURL.appending(path: "auth/orgs"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        request.httpBody = try JSONEncoder().encode(["name": name])
        let (data, http) = try await perform(request)
        try Self.check(statusCode: http.statusCode, data: data)
        let orgId = try JSONDecoder().decode(Response.self, from: data).organizationId
        return AuthOrg(id: orgId, organizationId: orgId, name: name)
    }

    private func post<T: Decodable>(_ path: String, body: [String: String]) async throws -> T {
        var request = URLRequest(url: baseURL.appending(path: path))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)
        let (data, http) = try await perform(request)
        try Self.check(statusCode: http.statusCode, data: data)
        return try JSONDecoder().decode(T.self, from: data)
    }

    /// The edge's typed error envelope (Phase C): `{error, code, retryable}`.
    /// `code` is the stable machine-readable code; `retryable` is the explicit
    /// transient/permanent classification. Either may be absent on a body the
    /// edge did not write.
    private struct ErrorEnvelope: Codable {
        var error: String?
        var code: String?
        var retryable: Bool?
        var pendingAuthenticationToken: String?
        var email: String?
    }

    private static func check(statusCode: Int, data: Data) throws {
        guard (200..<300).contains(statusCode) else {
            let envelope = try? JSONDecoder().decode(ErrorEnvelope.self, from: data)
            if envelope?.code == "email_verification_required",
               let token = envelope?.pendingAuthenticationToken {
                throw AuthError.emailVerificationRequired(
                    pendingAuthenticationToken: token,
                    email: envelope?.email
                )
            }
            throw AuthError.http(
                statusCode,
                envelope?.error ?? String(data: data, encoding: .utf8) ?? "",
                code: envelope?.code,
                retryable: envelope?.retryable
            )
        }
    }
}

// MARK: - Keychain storage

enum Keychain {
    private static let service = "ai.mvp-lab.cypher.ios"

    static func save(_ value: String, key: String) {
        let data = Data(value.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(query as CFDictionary)
        var add = query
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        SecItemAdd(add as CFDictionary, nil)
    }

    static func load(key: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func delete(key: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

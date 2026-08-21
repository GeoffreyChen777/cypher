// Phase B auth tests: RFC 7636 PKCE helpers, exchange body wiring, sign-in
// org routing, workspace-name validation, AuthError permanence, and the
// single-flight refresh guarantee (exactly one /auth/refresh per expired
// token, rotated tokens persist once, transient failures preserve the refresh
// token, permanent rejections clear every credential).

import XCTest
@testable import Cypher

/// A syntactically-valid JWT whose payload claims an `exp` far in the past —
/// AppConfig.isExpired reads the payload's `exp` (signature is not checked).
private func expiredJWT(exp: TimeInterval = 1_000) -> String {
    let payload = "{\"exp\":\(exp)}"
    let data = Data(payload.utf8)
    let b64 = data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
    return "header.\(b64).sig"
}

/// A recording HTTP transport for AuthClient: returns canned status/body and
/// records every request for inspection.
private final class RecordingTransport {
    var requests: [URLRequest] = []
    var status = 200
    var body = Data()

    @Sendable
    func perform(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request)
        let http = HTTPURLResponse(url: request.url!,
                                   statusCode: status,
                                   httpVersion: "HTTP/1.1",
                                   headerFields: ["content-type": "application/json"])!
        return (body, http)
    }
}

private func tokensJSON(access: String = "new-access", refresh: String = "new-refresh") -> Data {
    Data("{\"accessToken\":\"\(access)\",\"refreshToken\":\"\(refresh)\"}".utf8)
}

private func userTokensJSON() -> Data {
    Data(#"{"user":{"id":"u1","email":"a@b.c","firstName":"A","lastName":"B"},"accessToken":"at","refreshToken":"rt"}"#.utf8)
}

final class AuthPkceTests: XCTestCase {
    func testVerifierIsFreshUrlSafeAndInRange() {
        let a = PKCE.newVerifier()
        XCTAssertTrue(PKCE.isValidVerifier(a))
        XCTAssertTrue((43...128).contains(a.count))
        // Two attempts never draw the same verifier (32 CSPRNG bytes each).
        XCTAssertNotEqual(a, PKCE.newVerifier())
        XCTAssertNotEqual(a, PKCE.newVerifier())
    }

    func testChallengeMatchesIndependentSha256Base64url() {
        let verifier = PKCE.newVerifier()
        let challenge = PKCE.s256Challenge(for: verifier)
        // Independent check: base64url(SHA256(verifier)) without padding.
        let digest = SHA256.hash(data: Data(verifier.utf8))
        let expected = Data(digest).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        XCTAssertEqual(challenge, expected)
        XCTAssertFalse(challenge.contains("="))
        XCTAssertFalse(challenge.contains("+"))
        XCTAssertFalse(challenge.contains("/"))
    }

    func testIsValidVerifierRejectsMalformedShapes() {
        XCTAssertFalse(PKCE.isValidVerifier(""))
        XCTAssertFalse(PKCE.isValidVerifier(String(repeating: "a", count: 42)))
        XCTAssertFalse(PKCE.isValidVerifier(String(repeating: "a", count: 129)))
        XCTAssertFalse(PKCE.isValidVerifier("abc+defghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-"))
        XCTAssertFalse(PKCE.isValidVerifier("abc/defghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-"))
        XCTAssertFalse(PKCE.isValidVerifier("abc=defghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-"))
        // The full RFC 7636 unreserved alphabet is accepted.
        let ok = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~"
        XCTAssertTrue(PKCE.isValidVerifier(ok))
    }
}

final class AuthExchangeTests: XCTestCase {
    func testExchangeSendsVerifierInBody() async throws {
        let transport = RecordingTransport()
        transport.body = userTokensJSON()
        var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
        client.perform = transport.perform

        let verifier = PKCE.newVerifier()
        let (user, tokens) = try await client.exchange(code: "the-code", codeVerifier: verifier)

        XCTAssertEqual(user.id, "u1")
        XCTAssertEqual(tokens.accessToken, "at")
        XCTAssertEqual(tokens.refreshToken, "rt")
        XCTAssertEqual(transport.requests.count, 1)
        let body = try XCTUnwrap(transport.requests.first?.httpBody)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: String])
        XCTAssertEqual(json["code"], "the-code")
        XCTAssertEqual(json["codeVerifier"], verifier)
    }

    func testRefreshSendsOrgScopeWhenGiven() async throws {
        let transport = RecordingTransport()
        transport.body = tokensJSON()
        var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
        client.perform = transport.perform

        _ = try await client.refresh(refreshToken: "rt", organizationId: "org_1")
        let json = try XCTUnwrap(JSONSerialization.jsonObject(
            with: try XCTUnwrap(transport.requests.first?.httpBody)) as? [String: String])
        XCTAssertEqual(json["refreshToken"], "rt")
        XCTAssertEqual(json["organizationId"], "org_1")
    }

    func testCreateOrgPostsBearerAndName() async throws {
        let transport = RecordingTransport()
        transport.body = Data(#"{"organizationId":"org_new"}"#.utf8)
        var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
        client.perform = transport.perform

        let org = try await client.createOrg(name: "My Workspace", accessToken: "bearer-xyz")
        XCTAssertEqual(org.organizationId, "org_new")
        XCTAssertEqual(org.id, "org_new")
        XCTAssertEqual(org.name, "My Workspace")
        let request = try XCTUnwrap(transport.requests.first)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer bearer-xyz")
        let json = try XCTUnwrap(JSONSerialization.jsonObject(
            with: try XCTUnwrap(request.httpBody)) as? [String: String])
        XCTAssertEqual(json["name"], "My Workspace")
    }

    func testHttp401IsPermanentOtherStatusesAreTransient() async {
        // Explicit permanent semantics from the edge's `{error, code, retryable}`
        // envelope — NOT every raw 401.
        XCTAssertTrue(AuthError.http(401, "dead", code: "invalid_grant", retryable: false).isPermanent)
        XCTAssertFalse(AuthError.http(401, "dead", code: "invalid_grant", retryable: false).isTransient)
        // An explicit transient signal wins even over invalid_grant.
        XCTAssertFalse(AuthError.http(401, "x", code: "invalid_grant", retryable: true).isPermanent)
        // A bare/ambiguous 401 (no code, no retryable) is NOT permanent — a
        // misbehaving gateway must never clear the session.
        XCTAssertFalse(AuthError.http(401, "x", code: nil, retryable: nil).isPermanent)
        XCTAssertTrue(AuthError.http(401, "x", code: nil, retryable: nil).isTransient)
        // Machine code alone (no retryable) is the explicit rejection.
        XCTAssertTrue(AuthError.http(401, "x", code: "invalid_grant", retryable: nil).isPermanent)
        for code in [400, 403, 404, 429, 500, 502, 503] {
            XCTAssertFalse(AuthError.http(code, "x", code: "upstream", retryable: true).isPermanent, "status \(code)")
            XCTAssertTrue(AuthError.http(code, "x", code: "upstream", retryable: true).isTransient, "status \(code)")
        }
        XCTAssertTrue(AuthError.transport("network down").isTransient)
        XCTAssertFalse(AuthError.transport("network down").isPermanent)
        XCTAssertTrue(AuthError.invalidResponse.isTransient)
    }

    func testRefreshParsesEdgeErrorEnvelope() async {
        // 429 retryable: the client must classify it transient even though the
        // status is not 401.
        do {
            let transport = RecordingTransport()
            transport.status = 429
            transport.body = Data(#"{"error":"rate limited","code":"rate_limited","retryable":true}"#.utf8)
            var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
            client.perform = transport.perform
            _ = try await client.refresh(refreshToken: "rt")
            XCTFail("expected a 429 error")
        } catch let error as AuthError {
            guard case .http(let status, let message, let code, let retryable) = error else {
                return XCTFail("expected .http, got \(error)")
            }
            XCTAssertEqual(status, 429)
            XCTAssertEqual(message, "rate limited")
            XCTAssertEqual(code, "rate_limited")
            XCTAssertEqual(retryable, true)
            XCTAssertTrue(error.isTransient)
            XCTAssertFalse(error.isPermanent)
        } catch {
            XCTFail("unexpected error \(error)")
        }
    }

    func testRefreshParsesPermanentInvalidGrant() async {
        do {
            let transport = RecordingTransport()
            transport.status = 401
            transport.body = Data(#"{"error":"refresh token expired or revoked","code":"invalid_grant","retryable":false}"#.utf8)
            var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
            client.perform = transport.perform
            _ = try await client.refresh(refreshToken: "rt")
            XCTFail("expected a 401 error")
        } catch let error as AuthError {
            guard case .http(let status, let message, let code, let retryable) = error else {
                return XCTFail("expected .http, got \(error)")
            }
            XCTAssertEqual(status, 401)
            XCTAssertEqual(message, "refresh token expired or revoked")
            XCTAssertEqual(code, "invalid_grant")
            XCTAssertEqual(retryable, false)
            XCTAssertTrue(error.isPermanent)
        } catch {
            XCTFail("unexpected error \(error)")
        }
    }

    func testRefreshWithUnparseableBodyIsAmbiguousTransient() async {
        // A gateway answering HTML is not an explicit credential rejection.
        do {
            let transport = RecordingTransport()
            transport.status = 401
            transport.body = Data("<html>bad gateway</html>".utf8)
            var client = AuthClient(baseURL: URL(string: "https://edge.test")!)
            client.perform = transport.perform
            _ = try await client.refresh(refreshToken: "rt")
            XCTFail("expected a 401 error")
        } catch let error as AuthError {
            XCTAssertFalse(error.isPermanent, "ambiguous 401 must be treated as transient")
            XCTAssertTrue(error.isTransient)
        } catch {
            XCTFail("unexpected error \(error)")
        }
    }
}

final class AuthOrgRoutingTests: XCTestCase {
    private func org(_ id: String) -> AuthOrg {
        AuthOrg(id: id, organizationId: id, name: id)
    }

    func testZeroOrgsRoutesToCreate() {
        XCTAssertEqual(OrgSelection.route(for: []), .createOrg)
    }

    func testOneOrgAutoSelectsIt() {
        let only = org("org_1")
        XCTAssertEqual(OrgSelection.route(for: [only]), .autoSelect(only))
    }

    func testMultipleOrgsShowThePicker() {
        let orgs = [org("org_1"), org("org_2")]
        XCTAssertEqual(OrgSelection.route(for: orgs), .pick(orgs))
    }

    func testOrgNameValidatorTrimsAndEnforcesRange() {
        XCTAssertEqual(OrgNameValidator.normalize("  My Workspace  "), "My Workspace")
        XCTAssertNil(OrgNameValidator.normalize("   "))
        XCTAssertNil(OrgNameValidator.normalize(""))
        XCTAssertEqual(OrgNameValidator.normalize(String(repeating: "a", count: 80)).map(\.count), 80)
        XCTAssertNil(OrgNameValidator.normalize(String(repeating: "a", count: 81)))
    }
}

final class AuthRefreshSingleFlightTests: XCTestCase {
    /// Build an AppConfig in workos mode whose tokens are already expired and
    /// whose AuthClient transport is a recording stub returning `tokensJSON`.
    private func makeConfig(refreshCount: RecordingTransport,
                            tokens: AuthTokens) -> AppConfig {
        let config = AppConfig(edgeURL: URL(string: "https://edge.test")!,
                               mode: .workos,
                               userId: "u1", orgId: "org_1",
                               deviceId: "dev-1", deviceName: "Test",
                               tokens: tokens)
        var client = AuthClient(baseURL: config.edgeURL)
        client.perform = refreshCount.perform
        config.makeClient = { _ in client }
        return config
    }

    func testConcurrentCallsPerformExactlyOneRefresh() async {
        let transport = RecordingTransport()
        transport.body = tokensJSON()
        let expired = AuthTokens(accessToken: expiredJWT(), refreshToken: "rt-1")
        let config = makeConfig(refreshCount: transport, tokens: expired)

        // Hammer currentToken from many concurrent callers while the token is
        // expired — the single-flight gate must coalesce them onto one
        // /auth/refresh and hand every caller the same rotated token.
        let results = await withTaskGroup(of: String?.self, returning: [String?].self) { group in
            for _ in 0..<32 {
                group.addTask { await config.currentToken() }
            }
            var out: [String?] = []
            for await value in group { out.append(value) }
            return out
        }

        XCTAssertEqual(transport.requests.count, 1, "exactly one refresh for one expired token")
        XCTAssertEqual(results.count, 32)
        for token in results {
            XCTAssertEqual(token, "new-access")
        }
        // A follow-up call sees the rotated (fresh) token without another refresh.
        let after = await config.currentToken()
        XCTAssertEqual(after, "new-access")
        XCTAssertEqual(transport.requests.count, 1)
    }

    func testTransientFailureReturnsNilAndPreservesRefreshToken() async {
        let transport = RecordingTransport()
        transport.status = 503  // transient
        transport.body = Data(#"{"error":"overloaded","code":"upstream","retryable":true}"#.utf8)
        let expired = AuthTokens(accessToken: expiredJWT(), refreshToken: "rt-1")
        let config = makeConfig(refreshCount: transport, tokens: expired)

        let token = await config.currentToken()
        // Never yield the known-expired bearer.
        XCTAssertNil(token)
        XCTAssertEqual(transport.requests.count, 1)
        // Refresh token preserved for the next attempt: a later success works.
        transport.status = 200
        transport.body = tokensJSON()
        let retried = await config.currentToken()
        XCTAssertEqual(retried, "new-access")
        XCTAssertEqual(transport.requests.count, 2)
    }

    func testPermanentFailureClearsTokens() async {
        let transport = RecordingTransport()
        transport.status = 401  // dead session — explicit permanent machine code
        transport.body = Data(#"{"error":"invalid_grant","code":"invalid_grant","retryable":false}"#.utf8)
        let expired = AuthTokens(accessToken: expiredJWT(), refreshToken: "rt-1")
        let config = makeConfig(refreshCount: transport, tokens: expired)

        let token = await config.currentToken()
        XCTAssertNil(token)
        XCTAssertEqual(transport.requests.count, 1)
        // Credentials are gone: no further attempt can dial with the dead
        // session, and another call does not even re-read them.
        let again = await config.currentToken()
        XCTAssertNil(again)
        XCTAssertEqual(transport.requests.count, 1, "cleared tokens stop further refreshes")
    }
}

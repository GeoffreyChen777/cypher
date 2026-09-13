import XCTest
@testable import Cypher

final class NotificationTests: XCTestCase {
    private var info: [AnyHashable: Any] {
        ["cypher": ["version": 1, "scope": String(repeating: "a", count: 64),
                    "eventId": UUID().uuidString, "chatId": "chat", "projectId": "project", "kind": "completed"]]
    }
    func testStrictPayloadParsingNeverAcceptsURLsOrForeignShapes() throws {
        XCTAssertNotNil(PushPayload.parse(info))
        XCTAssertNil(PushPayload.parse(["aps": ["alert": "arbitrary"]]))
        var bad = info["cypher"] as! [String: Any]
        bad["chatId"] = "https://example.com"
        XCTAssertNil(PushPayload.parse(["cypher": bad]))
        bad["chatId"] = "chat"
        bad["scope"] = "other-account"
        XCTAssertNil(PushPayload.parse(["cypher": bad]))
        bad["scope"] = String(repeating: "a", count: 64)
        bad["kind"] = "tool-output"
        XCTAssertNil(PushPayload.parse(["cypher": bad]))
    }
    func testDefaultPolicyIsSmartAndDoesNotEnableIndependentSubagentResults() throws {
        var prefs = NotificationPreferences()
        let payload = try XCTUnwrap(PushPayload.parse(info))
        XCTAssertEqual(prefs.mode, .always)
        XCTAssertFalse(prefs.subagents)
        XCTAssertTrue(prefs.permits(payload))
        prefs.mutedProjects = ["project"]
        XCTAssertFalse(prefs.permits(payload))
        prefs.mutedProjects = []
        XCTAssertTrue(prefs.permits(payload))
    }
    func testLeaseEpochsAndOfflineRevocationsSurviveSerialization() throws {
        var state = PushRegistrationState()
        let first = state.nextEpoch()
        state.binding = PushBinding(account: "account-a", baseURL: URL(string: "https://edge.test")!,
            scope: String(repeating: "a", count: 64), bindingId: String(repeating: "b", count: 64),
            lease: UUID().uuidString, epoch: first)
        state.retire()
        XCTAssertNil(state.binding)
        XCTAssertEqual(state.revocations.count, 1)
        XCTAssertGreaterThan(state.revocations[0].epoch, first)
        let next = state.nextEpoch()
        XCTAssertGreaterThan(next, state.revocations[0].epoch)
        let restored = try JSONDecoder().decode(PushRegistrationState.self, from: JSONEncoder().encode(state))
        XCTAssertEqual(restored.installationId, state.installationId)
        XCTAssertEqual(restored.epoch, next)
        XCTAssertEqual(restored.revocations.first?.binding.account, "account-a")
    }
    func testAPNsEnvironmentMatchesBuildConfiguration() {
        #if DEBUG
        XCTAssertEqual(Bundle.main.object(forInfoDictionaryKey: "CypherAPNSEnvironment") as? String, "development")
        #else
        XCTAssertEqual(Bundle.main.object(forInfoDictionaryKey: "CypherAPNSEnvironment") as? String, "production")
        #endif
    }
    func testInvalidatedAuthConfigCannotRefreshOrClearANewAccount() async {
        let config = AppConfig(edgeURL: URL(string: "https://edge.test")!, mode: .workos,
            userId: "old", orgId: "org", deviceId: "phone", deviceName: "Test",
            tokens: AuthTokens(accessToken: "old-access", refreshToken: "old-refresh"))
        config.invalidate()
        let previous = Keychain.load(key: "accessToken")
        let previousRefresh = Keychain.load(key: "refreshToken")
        defer {
            if let previous { Keychain.save(previous, key: "accessToken") }
            else { Keychain.delete(key: "accessToken") }
            if let previousRefresh { Keychain.save(previousRefresh, key: "refreshToken") }
            else { Keychain.delete(key: "refreshToken") }
        }
        let saveStatus = Keychain.save("new-account", key: "accessToken")
        XCTAssertEqual(saveStatus, 0, "Keychain fixture setup must succeed before testing invalidation")
        let token = await config.currentToken()
        XCTAssertNil(token)
        XCTAssertTrue(Keychain.load(key: "accessToken") == "new-account")
    }

    func testLateRefreshCannotRestoreSignedOutCredentials() async {
        let gate = NotificationRefreshProbe()
        let config = AppConfig(edgeURL: URL(string: "https://edge.test")!, mode: .workos,
            userId: "old", orgId: "org", deviceId: "phone", deviceName: "Test",
            tokens: AuthTokens(accessToken: "a.eyJleHAiOjF9.b", refreshToken: "expired"))
        var client = AuthClient(baseURL: config.edgeURL)
        client.perform = { request in await gate.wait(request) }
        config.makeClient = { _ in client }
        let task = Task { await config.currentToken() }
        while !(await gate.entered) { await Task.yield() }
        config.invalidate()
        let previous = Keychain.load(key: "accessToken")
        let previousRefresh = Keychain.load(key: "refreshToken")
        defer {
            if let previous { Keychain.save(previous, key: "accessToken") }
            else { Keychain.delete(key: "accessToken") }
            if let previousRefresh { Keychain.save(previousRefresh, key: "refreshToken") }
            else { Keychain.delete(key: "refreshToken") }
        }
        let saveStatus = Keychain.save("new-account", key: "accessToken")
        XCTAssertEqual(saveStatus, 0, "Keychain fixture setup must succeed before testing late refresh")
        await gate.finish()
        let result = await task.value
        XCTAssertNil(result)
        XCTAssertTrue(Keychain.load(key: "accessToken") == "new-account")
    }
}

private actor NotificationRefreshProbe {
    var entered = false
    private var continuation: CheckedContinuation<(Data, HTTPURLResponse), Never>?
    func wait(_ request: URLRequest) async -> (Data, HTTPURLResponse) {
        entered = true
        return await withCheckedContinuation { continuation = $0 }
    }
    func finish() {
        continuation?.resume(returning: (
            Data(#"{"accessToken":"late-old-access","refreshToken":"late-old-refresh"}"#.utf8),
            HTTPURLResponse(url: URL(string: "https://edge.test/auth/refresh")!,
                            statusCode: 200, httpVersion: nil, headerFields: nil)!))
        continuation = nil
    }
}

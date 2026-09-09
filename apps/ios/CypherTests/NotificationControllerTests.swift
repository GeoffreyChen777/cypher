import XCTest
import UserNotifications
@testable import Cypher

@MainActor
private final class NotificationFixture {
    let controller = NotificationController()
    var storage: String?
    var permission: UNAuthorizationStatus = .notDetermined
    var preferences = NotificationPreferences()
    var registrations: [[String: Any]] = []
    var revocations: [URLRequest] = []
    var activities: [[String: Any]] = []
    var readEventIds: [String] = []
    var leases: [String: String] = [:]
    init() {
        controller.readRegistration = { [weak self] in self?.storage }
        controller.writeRegistration = { [weak self] text in self?.storage = text; return self != nil }
        controller.authorization = { [weak self] in self?.permission ?? .notDetermined }
        controller.requestPermission = { [weak self] in self?.permission = .authorized; return true }
        controller.registerWithOS = { [weak self] in self?.controller.receivedToken(Data(repeating: 7, count: 32)) }
        controller.clearDelivered = {}
        controller.perform = { [weak self] request in
            guard let self, let url = request.url else { throw RelayError.notConnected }
            let scope = String(repeating: request.value(forHTTPHeaderField: "Authorization")?.contains("alice") == true ? "a" : "b", count: 64)
            var body: [String: Any] = ["ok": true]
            if url.path.hasSuffix("/settings") {
                if request.httpMethod == "PUT", let data = request.httpBody {
                    preferences = try JSONDecoder().decode(NotificationPreferences.self, from: data)
                }
                let settings = try JSONSerialization.jsonObject(with: JSONEncoder().encode(preferences))
                body = ["scope": scope, "available": true, "settings": settings]
            } else if url.path.hasSuffix("/register"), let data = request.httpBody {
                let registration = try JSONSerialization.jsonObject(with: data) as! [String: Any]
                registrations.append(registration)
                let key = "\(scope)/\(registration["epoch"]!)"
                let lease = leases[key] ?? UUID().uuidString.lowercased()
                leases[key] = lease
                body = ["scope": scope, "bindingId": String(repeating: "c", count: 64), "lease": lease]
            } else if url.path.hasSuffix("/revoke") {
                revocations.append(request)
            } else if url.path.hasSuffix("/activity"), let data = request.httpBody {
                let activity = try JSONSerialization.jsonObject(with: data) as! [String: Any]
                activities.append(activity)
                body = ["ok": true, "scope": scope,
                        "readEventIds": activity["foreground"] as? Bool == true && activity["chatId"] as? String == "chat"
                            ? readEventIds : []]
            }
            return (try JSONSerialization.data(withJSONObject: body),
                    HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: nil)!)
        }
    }
    func bind(_ user: String = "alice") {
        controller.bind(AppConfig(edgeURL: URL(string: "https://edge.test")!, mode: .dev,
            userId: user, orgId: "org", deviceId: "phone", deviceName: "Test", devBearer: "\(user)@org"))
    }
}

@MainActor
final class NotificationControllerTests: XCTestCase {
    private func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<100 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("Notification operation did not settle")
        throw RelayError.timeout
    }
    private func payload(_ character: Character = "a") -> PushPayload {
        PushPayload(eventId: UUID().uuidString, scope: String(repeating: String(character), count: 64),
                    chatId: "chat", projectId: "project", kind: "completed")
    }

    func testRegistrationRequiresConsentAndRevalidationIsIdempotent() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        f.bind()
        try await wait { f.controller.available }
        XCTAssertTrue(f.registrations.isEmpty)
        await f.controller.enable()
        try await wait { f.controller.registered }
        let epoch = try XCTUnwrap(f.registrations.first?["epoch"] as? Int)
        await f.controller.refresh()
        XCTAssertTrue(f.controller.registered)
        XCTAssertTrue(f.registrations.allSatisfy { $0["epoch"] as? Int == epoch })
        XCTAssertEqual(f.leases.count, 1, "Foreground refresh must not invalidate pending recipients")
    }

    func testLogoutRevokesWithoutRefreshingOrSendingAccountAuthorization() async throws {
        let f = NotificationFixture()
        f.bind()
        try await wait { f.controller.available }
        await f.controller.enable()
        try await wait { f.controller.registered }
        f.controller.disconnect()
        try await wait { !f.revocations.isEmpty }
        XCTAssertNil(f.revocations[0].value(forHTTPHeaderField: "Authorization"))
        XCTAssertNil(f.controller.scope)
        f.bind("bob")
        defer { f.controller.disconnect() }
        try await wait { f.controller.scope == String(repeating: "b", count: 64) }
        f.controller.receive(payload("a"), tapped: true)
        XCTAssertNil(f.controller.pendingNavigation)
    }

    func testColdStartTapWaitsForTheMatchingAuthenticatedWorkspace() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        let push = payload()
        f.controller.receive(push, tapped: true)
        XCTAssertNil(f.controller.pendingNavigation)
        f.bind()
        try await wait { f.controller.pendingNavigation != nil }
        XCTAssertEqual(f.controller.pendingNavigation, push)
    }

    func testForegroundSameChatIsQuietAndOtherPagesReceiveOnlyOneInAppNotice() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        f.bind()
        try await wait { f.controller.scope != nil }
        f.controller.viewing("chat")
        let viewed = payload()
        f.controller.receive(viewed, tapped: false)
        XCTAssertNil(f.controller.banner)
        f.controller.viewing(nil)
        f.controller.receive(viewed, tapped: false)
        XCTAssertNil(f.controller.banner, "Already viewed events must not reappear")
        let other = payload()
        f.controller.receive(other, tapped: false)
        XCTAssertEqual(f.controller.banner, other)
        f.controller.banner = nil
        f.controller.receive(other, tapped: false)
        XCTAssertNil(f.controller.banner)
    }

    func testReadReceiptStaysSilentAfterLeavingAndDoesNotHideAFutureEvent() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        f.bind()
        try await wait { f.controller.scope != nil }
        let read = payload()
        f.readEventIds = [read.eventId]
        // No transcript data is supplied: entering alone is the chosen policy.
        f.controller.viewing("chat")
        try await wait { f.activities.contains { $0["chatId"] as? String == "chat" } }
        // Give the activity response a turn to apply its concrete event IDs.
        await Task.yield()
        f.controller.viewing(nil)
        f.controller.receive(read, tapped: false)
        XCTAssertNil(f.controller.banner)
        let future = payload()
        f.controller.receive(future, tapped: false)
        XCTAssertEqual(f.controller.banner, future)
    }

    func testQuickNavigationCapturesDistinctActivityStatesWithIncreasingSequences() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        f.bind()
        try await wait { f.controller.available }
        let baseline = f.activities.count
        f.controller.viewing("chat")
        f.controller.viewing(nil)
        try await wait { f.activities.count >= baseline + 2 }
        let reports = f.activities.suffix(2)
        XCTAssertEqual(reports.first?["chatId"] as? String, "chat")
        XCTAssertTrue(reports.last?["chatId"] is NSNull)
        XCTAssertLessThan(try XCTUnwrap(reports.first?["sequence"] as? Int),
                          try XCTUnwrap(reports.last?["sequence"] as? Int))
    }

    func testReadReceiptFromOldAccountCannotSilenceTheNewAccount() async throws {
        let f = NotificationFixture()
        defer { f.controller.disconnect() }
        f.bind()
        try await wait { f.controller.scope != nil }
        let oldPerform = f.controller.perform
        let event = payload("b")
        var release: CheckedContinuation<Void, Never>?
        f.controller.perform = { request in
            if request.url?.path.hasSuffix("/activity") == true,
               let data = request.httpBody,
               let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               body["chatId"] as? String == "chat" {
                await withCheckedContinuation { release = $0 }
                return (try JSONSerialization.data(withJSONObject: [
                    "scope": String(repeating: "b", count: 64), "readEventIds": [event.eventId]
                ]), HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
            }
            return try await oldPerform(request)
        }
        f.controller.viewing("chat")
        try await wait { release != nil }
        f.bind("bob")
        try await wait { f.controller.scope == String(repeating: "b", count: 64) }
        XCTAssertNil(f.controller.currentChat)
        release?.resume()
        await Task.yield()
        f.controller.receive(event, tapped: false)
        XCTAssertEqual(f.controller.banner, event)
    }

    func testActivitySequenceSurvivesControllerRecreation() async throws {
        let first = NotificationFixture()
        first.bind()
        try await wait { !first.activities.isEmpty }
        let sequence = try XCTUnwrap(first.activities.last?["sequence"] as? Int)
        let storage = first.storage
        first.controller.disconnect()
        let second = NotificationFixture()
        defer { second.controller.disconnect() }
        second.storage = storage
        second.bind()
        try await wait { !second.activities.isEmpty }
        XCTAssertGreaterThan(try XCTUnwrap(second.activities.first?["sequence"] as? Int), sequence)
        XCTAssertEqual(first.activities.first?["clientId"] as? String, second.activities.first?["clientId"] as? String)
    }
}

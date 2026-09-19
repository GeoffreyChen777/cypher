import XCTest
@preconcurrency import UserNotifications
@testable import Cypher

/// Regression for a TestFlight crash reported twice by the same tester
/// (0.1.5 build 2 on 2026-09-10, 0.2.0 build 13 on 2026-09-19): tapping a
/// notification killed the app with SIGABRT 0.28s into the cold launch it
/// had just triggered.
///
/// `PushAppDelegate`'s delegate methods were `nonisolated ... async`, so the
/// compiler-generated ObjC thunk invoked UIKit's completion handler from the
/// cooperative pool. `-[UIApplication _performBlockAfterCATransactionCommit
/// Synchronizes:]` asserts that handler runs on the main thread. Note the
/// early `guard ... else { return }` returned off-main too, so a tap crashed
/// whether or not the payload parsed — both are covered below.
///
/// These drive the REAL ObjC entry points that UNUserNotificationCenter calls,
/// from a background thread, and assert the completion handler comes back on
/// the main thread.
final class PushDelegateIsolationTests: XCTestCase {
    private final class Box: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: Bool?
        var value: Bool? {
            get { lock.lock(); defer { lock.unlock() }; return stored }
            set { lock.lock(); stored = newValue; lock.unlock() }
        }
    }

    private typealias DidReceive = @convention(c) (
        AnyObject, Selector, UNUserNotificationCenter, UNNotificationResponse,
        @escaping @convention(block) () -> Void
    ) -> Void
    private typealias WillPresent = @convention(c) (
        AnyObject, Selector, UNUserNotificationCenter, UNNotification,
        @escaping @convention(block) (UNNotificationPresentationOptions) -> Void
    ) -> Void

    private static let validPayload: [AnyHashable: Any] = [
        "cypher": [
            "version": 1,
            "scope": String(repeating: "a", count: 64),
            "eventId": UUID().uuidString,
            "chatId": "chat-veil",
            "projectId": "space-cypher",
            "kind": "input",
        ]
    ]

    @MainActor
    private func tapCompletesOnMainThread(userInfo: [AnyHashable: Any]) async throws {
        let delegate = PushAppDelegate()
        let response = try XCTUnwrap(NotificationStub.response(userInfo: userInfo),
                                     "Could not build a UNNotificationResponse stub")
        let selector = NSSelectorFromString(
            "userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:")
        XCTAssertTrue(delegate.responds(to: selector),
                      "PushAppDelegate no longer exports the ObjC tap entry point")

        let call = unsafeBitCast(delegate.method(for: selector), to: DidReceive.self)
        let onMain = Box()
        let finished = expectation(description: "tap completion handler")
        DispatchQueue.global().async {
            XCTAssertFalse(Thread.isMainThread, "the call must start off the main thread")
            call(delegate, selector, .current(), response) {
                onMain.value = Thread.isMainThread
                finished.fulfill()
            }
        }
        await fulfillment(of: [finished], timeout: 5)
        XCTAssertEqual(onMain.value, true,
                       "UIKit asserts the tap completion handler runs on the main thread")
    }

    @MainActor
    func testTapWithAParsablePayloadCompletesOnTheMainThread() async throws {
        try await tapCompletesOnMainThread(userInfo: Self.validPayload)
    }

    /// The `guard ... else { return }` path — an alert from another account or
    /// an older schema. This one crashed too.
    @MainActor
    func testTapWithAnUnparsablePayloadCompletesOnTheMainThread() async throws {
        try await tapCompletesOnMainThread(userInfo: ["aps": ["alert": "hi"]])
    }

    @MainActor
    func testForegroundPresentationCompletesOnTheMainThread() async throws {
        let delegate = PushAppDelegate()
        let notification = try XCTUnwrap(NotificationStub.notification(userInfo: Self.validPayload),
                                         "Could not build a UNNotification stub")
        let selector = NSSelectorFromString(
            "userNotificationCenter:willPresentNotification:withCompletionHandler:")
        XCTAssertTrue(delegate.responds(to: selector),
                      "PushAppDelegate no longer exports the ObjC willPresent entry point")

        let call = unsafeBitCast(delegate.method(for: selector), to: WillPresent.self)
        let onMain = Box()
        let finished = expectation(description: "willPresent completion handler")
        DispatchQueue.global().async {
            call(delegate, selector, .current(), notification) { _ in
                onMain.value = Thread.isMainThread
                finished.fulfill()
            }
        }
        await fulfillment(of: [finished], timeout: 5)
        XCTAssertEqual(onMain.value, true,
                       "UIKit asserts the presentation completion handler runs on the main thread")
    }
}

/// `UNNotification` / `UNNotificationResponse` have no public initializer, but
/// both are `NSSecureCoding`. This feeds the archived keys they look for.
private enum NotificationStub {
    static func notification(userInfo: [AnyHashable: Any]) -> UNNotification? {
        let content = UNMutableNotificationContent()
        content.userInfo = userInfo
        let request = UNNotificationRequest(identifier: "regression", content: content, trigger: nil)
        return UNNotification(coder: StubCoder(["date": Date(), "request": request]))
    }

    static func response(userInfo: [AnyHashable: Any]) -> UNNotificationResponse? {
        guard let notification = notification(userInfo: userInfo) else { return nil }
        return UNNotificationResponse(coder: StubCoder([
            "notification": notification,
            "actionIdentifier": UNNotificationDefaultActionIdentifier,
            "sourceIdentifier": "",
        ]))
    }
}

private final class StubCoder: NSCoder {
    private let values: [String: Any]
    init(_ values: [String: Any]) {
        self.values = values
        super.init()
    }
    override var allowsKeyedCoding: Bool { true }
    override var decodingFailurePolicy: NSCoder.DecodingFailurePolicy { .setErrorAndReturn }
    override func containsValue(forKey key: String) -> Bool { values[key] != nil }
    override func decodeObject(forKey key: String) -> Any? { values[key] }
    override func decodeBool(forKey key: String) -> Bool { values[key] as? Bool ?? false }
    override func decodeInt32(forKey key: String) -> Int32 { values[key] as? Int32 ?? 0 }
    override func decodeInt64(forKey key: String) -> Int64 { values[key] as? Int64 ?? 0 }
    override func decodeInteger(forKey key: String) -> Int { values[key] as? Int ?? 0 }
    override func decodeDouble(forKey key: String) -> Double { values[key] as? Double ?? 0 }
    override func decodeFloat(forKey key: String) -> Float { values[key] as? Float ?? 0 }
    override func failWithError(_ error: Error) {}
}

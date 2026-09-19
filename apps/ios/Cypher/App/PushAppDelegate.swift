import UIKit
import UserNotifications

final class PushAppDelegate: NSObject, UIApplicationDelegate, @preconcurrency UNUserNotificationCenterDelegate {
    @MainActor var controller: NotificationController? {
        didSet {
            if let token { controller?.receivedToken(token); self.token = nil }
            if let pendingTap {
                self.pendingTap = nil
                controller?.receive(pendingTap, tapped: true)
            }
        }
    }
    @MainActor private var token: Data?
    @MainActor private var pendingTap: PushPayload?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }
    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        if let controller { controller.receivedToken(deviceToken) } else { token = deviceToken }
    }
    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        controller?.registrationFailed()
    }
    // Both delegate methods must stay @MainActor. Swift bridges them to their
    // ObjC `...withCompletionHandler:` selectors, and the generated thunk calls
    // UIKit's completion handler on whichever executor the method returns on.
    // Under `nonisolated` that is the cooperative pool, and UIKit's handler
    // asserts main thread: tapping a notification cold-launched the app and
    // killed it with SIGABRT (TestFlight 0.1.5 b2, 0.2.0 b13).
    @MainActor func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        let info = notification.request.content.userInfo
        if let badge = NotificationBadge.parse(info) { controller?.receiveBadge(badge) }
        if let payload = PushPayload.parse(info) { controller?.receive(payload, tapped: false) }
        // Foreground notifications use our small in-app banner, never a
        // second system banner/sound (including when viewing the same chat).
        return []
    }
    @MainActor func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           didReceive response: UNNotificationResponse) async {
        guard let payload = PushPayload.parse(response.notification.request.content.userInfo) else { return }
        if let controller { controller.receive(payload, tapped: true) }
        else { pendingTap = payload }
        // Tapping an old delivered alert is not an authoritative unread
        // count. Refresh from the authenticated server after navigation.
        if let controller { Task { await controller.refresh() } }
    }
}

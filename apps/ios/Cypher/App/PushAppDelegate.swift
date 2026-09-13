import UIKit
import UserNotifications

final class PushAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
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
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        let info = notification.request.content.userInfo
        let badge = NotificationBadge.parse(info)
        let payload = PushPayload.parse(info)
        await MainActor.run {
            if let badge { self.controller?.receiveBadge(badge) }
            if let payload { self.controller?.receive(payload, tapped: false) }
        }
        // Foreground notifications use our small in-app banner, never a
        // second system banner/sound (including when viewing the same chat).
        return []
    }
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           didReceive response: UNNotificationResponse) async {
        guard let payload = PushPayload.parse(response.notification.request.content.userInfo) else { return }
        await MainActor.run {
            if let controller = self.controller { controller.receive(payload, tapped: true) }
            else { self.pendingTap = payload }
            // Tapping an old delivered alert is not an authoritative unread
            // count. Refresh from the authenticated server after navigation.
            if let controller { Task { await controller.refresh() } }
        }
    }
}

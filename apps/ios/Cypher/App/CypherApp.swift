// Cypher for iOS — a viewport onto the cypher mesh. The phone is a peer
// device: it joins the workspace and session doc rooms and drives remote
// engines through the durable command queue.

import SwiftUI

@main
struct CypherApp: App {
    @UIApplicationDelegateAdaptor(PushAppDelegate.self) private var pushDelegate
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // Navigation titles in the app's type. Only the text attributes: a
        // UINavigationBarAppearance override would replace the system's
        // Liquid Glass bar treatment.
        let bar = UINavigationBar.appearance()
        if let large = UIFont(name: "Geist-SemiBold", size: 32) {
            bar.largeTitleTextAttributes = [.font: UIFontMetrics(forTextStyle: .largeTitle).scaledFont(for: large)]
        }
        if let inline = UIFont(name: "Geist-SemiBold", size: 17) {
            bar.titleTextAttributes = [.font: UIFontMetrics(forTextStyle: .headline).scaledFont(for: inline)]
        }
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                // Monochrome controls: glass buttons, toolbar icons, and
                // toggles follow the text color — accent stays paint
                // for status/markdown, never chrome.
                .tint(Theme.text)
                .background(Theme.bg)
                .onAppear { pushDelegate.controller = model.notifications }
                .overlay(alignment: .top) {
                    InAppNotificationBanner(controller: model.notifications)
                }
                .modifier(AppAppearanceModifier())
                .onChange(of: scenePhase) { _, phase in
                    model.notifications.setForeground(phase == .active)
                    if phase == .background {
                        model.flushDocs()
                    } else if phase == .active {
                        // Suspension kills sockets without running any
                        // failure path — without this kick the workspace
                        // room stays dead after foregrounding while chat
                        // views reconnect on open (frozen sidebar/Working
                        // indicators against live transcripts, 2026-08-04).
                        model.foregrounded()
                    }
                }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
            switch model.phase {
            case .signedOut:
                SignInView()
            case .pickingOrg(let tokens, let orgs):
                OrgPickerView(tokens: tokens, orgs: orgs)
            case .ready:
                HomeView()
            }
        }
        .task { model.restore() }
    }
}

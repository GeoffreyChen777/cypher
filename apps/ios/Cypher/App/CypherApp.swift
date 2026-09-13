// Cypher for iOS — a viewport onto the cypher mesh. The phone is a peer
// device: it joins the workspace and session doc rooms and drives remote
// engines through the durable command queue.

import SwiftUI

@main
struct CypherApp: App {
    @UIApplicationDelegateAdaptor(PushAppDelegate.self) private var pushDelegate
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

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
                    .safeAreaInset(edge: .bottom) {
                        if let problem = model.workspace?.error ?? model.workspace?.connectionError {
                            Text("Sync unavailable: \(problem)")
                                .font(.caption)
                                .foregroundStyle(.red)
                                .padding(8)
                                .frame(maxWidth: .infinity)
                                .background(Theme.bg)
                        }
                    }
            }
        }
        .task { model.restore() }
    }
}

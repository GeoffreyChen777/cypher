// A session in a window of its own — iPad's "Open in New Window", the
// nearest thing to the desktop's tiled sessions: Stage Manager or Split View
// puts two side by side. The window shows the session's stack and inspector
// without Home, against the app's one shared model.

import SwiftUI

struct SessionWindow: View {
    static let id = "session"

    @Environment(AppModel.self) private var model
    @Environment(\.dismissWindow) private var dismissWindow
    @Environment(\.openWindow) private var openWindow
    let chatId: String?
    @State private var path: [Route] = []

    var body: some View {
        Group {
            if case .ready = model.phase, let chatId {
                DetailColumn(path: $path) {
                    // The session left its screen (archived, deleted).
                    ContentUnavailableView {
                        Label("Session Closed", systemImage: "rectangle.on.rectangle.slash")
                    } actions: {
                        Button("Close Window") { dismissWindow() }
                            .buttonStyle(.glass)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Theme.bg.ignoresSafeArea())
                }
                .task(id: chatId) {
                    if path.first != .chat(chatId) { path = [.chat(chatId)] }
                }
            } else {
                ContentUnavailableView(
                    "Session Unavailable", systemImage: "bubble.left.and.exclamationmark.bubble.right",
                    description: Text("Sign in to Cypher in its main window."))
            }
        }
        // The system may restore this window before (or without) the main one.
        .task { model.restore() }
        .onAppear {
            // Rigs (and their UI tests) start from Home alone: a window
            // restored from an earlier run closes, opening Home (iPadOS may
            // have brought back only this one). People keep theirs.
            if ProcessInfo.processInfo.arguments.contains("-demo"),
                let chatId, !model.openedSessionWindows.contains(chatId)
            {
                openWindow(id: CypherApp.mainWindowID)
                dismissWindow()
            }
        }
    }
}

/// "Open in New Window" for a session's menus, where the device has
/// windows (iPad); nothing elsewhere.
struct OpenInNewWindowItem: View {
    @Environment(AppModel.self) private var model
    @Environment(\.supportsMultipleWindows) private var supportsMultipleWindows
    @Environment(\.openWindow) private var openWindow
    let chatId: String

    var body: some View {
        if supportsMultipleWindows {
            Button("Open in New Window", systemImage: "macwindow.badge.plus") {
                model.openedSessionWindows.insert(chatId)
                openWindow(id: SessionWindow.id, value: chatId)
            }
        }
    }
}

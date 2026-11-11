// Home's two lists: the sessions, or the workspaces (the desktop's
// "Projects | Workspaces" switch, shell/bots.rs). The switch is the first
// button at the top left; while the sessions show, it carries an amber
// count of what the workspaces need, as the desktop logo's does.

import SwiftUI

enum HomeMode: String, CaseIterable, Identifiable {
    case sessions
    case workspaces

    /// The choice, kept across launches (`-homeMode <mode>` sets it for a
    /// launch; demo launches start on Sessions without it).
    static let storageKey = "homeMode"

    var id: String { rawValue }

    var title: String {
        switch self {
        case .sessions: "Sessions"
        case .workspaces: "Workspaces"
        }
    }

    var symbol: String {
        switch self {
        case .sessions: "bubble.left.and.bubble.right"
        case .workspaces: "square.grid.2x2"
        }
    }
}

/// The switch: the current list's glyph, a menu of both.
struct HomeModeSwitch: View {
    @Binding var mode: HomeMode
    /// What the workspaces need, shown while the sessions show.
    let needs: Int

    var body: some View {
        Menu {
            Picker("Show", selection: $mode) {
                ForEach(HomeMode.allCases) { mode in
                    Label(mode.title, systemImage: mode.symbol).tag(mode)
                }
            }
            .pickerStyle(.inline)
        } label: {
            Image(systemName: mode.symbol)
                .overlay(alignment: .topTrailing) {
                    if mode == .sessions, needs > 0 {
                        Text("\(needs)")
                            .font(.system(size: 10, weight: .bold))
                            .monospacedDigit()
                            .foregroundStyle(Theme.bg)
                            .padding(.horizontal, 4)
                            .frame(minWidth: 15, minHeight: 15)
                            .background(Theme.warning, in: Capsule())
                            .offset(x: 9, y: -8)
                    }
                }
        }
        .accessibilityLabel("Show")
        .accessibilityValue(
            mode == .sessions && needs > 0
                ? "\(mode.title), workspaces: \(WorkspaceChatText.needsYou(needs))" : mode.title
        )
        .accessibilityIdentifier("home-mode")
    }
}

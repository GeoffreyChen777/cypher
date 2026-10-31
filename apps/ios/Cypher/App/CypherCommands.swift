// Hardware-keyboard commands (the iPad menu bar and the ⌘ overlay): new
// session or project, search, previous / next session. Home publishes what
// they do for its window; a window without Home (a session window) leaves
// them disabled. The composer's ⌘↩ / ⌘. are its text view's own
// (ComposerUITextView).

import SwiftUI

/// Home's actions for the menu bar, from the window's HomeView.
struct HomeCommands {
    /// nil when there's no project to start a session in.
    var newSession: (() -> Void)?
    var newProject: () -> Void
    var search: () -> Void
    /// Step through the listed sessions: -1 previous, 1 next.
    var step: (Int) -> Void
}

extension FocusedValues {
    @Entry var homeCommands: HomeCommands?
}

struct CypherCommands: Commands {
    @FocusedValue(\.homeCommands) private var home

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Session") { home?.newSession?() }
                .keyboardShortcut("n")
                .disabled(home?.newSession == nil)
            Button("New Project…") { home?.newProject() }
                .keyboardShortcut("n", modifiers: [.command, .shift])
                .disabled(home == nil)
        }
        CommandMenu("Go") {
            Button("Search Sessions") { home?.search() }
                .keyboardShortcut("f")
                .disabled(home == nil)
            Divider()
            // Arrows, not ⌘⇧[ ]: shift turns `]` into `}`, and the shortcut
            // then never matches.
            Button("Previous Session") { home?.step(-1) }
                .keyboardShortcut(.upArrow, modifiers: [.command, .option])
                .disabled(home == nil)
            Button("Next Session") { home?.step(1) }
                .keyboardShortcut(.downArrow, modifiers: [.command, .option])
                .disabled(home == nil)
        }
    }
}

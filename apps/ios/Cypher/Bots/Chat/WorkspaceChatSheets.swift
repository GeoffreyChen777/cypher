// What the session screen presents for a workspace: its board as a sheet
// with detents on the phone, its settings, Memory settings (a memory pass
// outside any workspace) and the delete prompt, plus the environment its
// transcript cards and links reach the screen through.

import SwiftUI

/// A workspace's board at one page.
struct BoardTarget: Identifiable, Hashable {
    var botId: String
    var page: BotPage

    var id: String { "\(botId)|\(page)" }
}

struct WorkspaceChatSheets: ViewModifier {
    @Binding var boardSheet: BoardTarget?
    @Binding var botSettings: BoardTarget?
    @Binding var deletingBot: Bot?
    @Binding var showMemorySettings: Bool
    let memoryCards: Set<String>
    let openChat: (String) -> Void
    /// Opens a route on the screen's path (the board's "Open project").
    let openRoute: (Route) -> Void
    let seedComposer: (String) -> Void
    /// The board's sheet is gone: open what it asked for.
    let boardDismissed: () -> Void
    let delete: (Bot) -> Void
    /// The settings sheet deleted the workspace: leave its screen.
    let deleted: () -> Void
    /// The launch rig's sheet, once the screen is up.
    let launch: () async -> Void

    func body(content: Content) -> some View {
        content
            .environment(\.openBotChat, openChat)
            .environment(\.openBotRoute, openRoute)
            .environment(\.seedComposer, seedComposer)
            .environment(\.transcriptMemoryCards, memoryCards)
            .sheet(item: $boardSheet, onDismiss: boardDismissed) { target in
                BoardSheet(target: target, openChat: openChat)
                    .environment(\.openBotChat, openChat)
                    .environment(\.openBotRoute, openRoute)
            }
            .sheet(item: $botSettings) { target in
                BotSettingsSheet(botId: target.botId, onDeleted: deleted)
            }
            .sheet(isPresented: $showMemorySettings) {
                MemorySettingsView()
            }
            .botDeletePrompt($deletingBot, delete: delete)
            .task { await launch() }
    }
}

extension EnvironmentValues {
    /// Told each board page as it shows (the phone's sheet grows for a
    /// page that needs the room).
    @Entry var boardPageShown: ((BotPage) -> Void)? = nil
}

extension BotPage {
    /// A page that holds a form, or a decision with content under it (an
    /// approval, a plan, a drafted answer, an invite): it needs the whole
    /// height. Lists and the overview open at half height.
    var wantsFullHeight: Bool {
        switch self {
        case .overview, .needsYou, .work, .routines, .projects, .activity, .connections, .memory: false
        case .workItem, .approval, .asking, .routine, .routineForm, .project, .connection, .exchange, .connect,
            .memoryFile:
            true
        }
    }
}

/// The board as the phone's sheet: half height for the overview and lists,
/// full height for a page with a form or decision, and growing to it when
/// such a page is pushed (it never shrinks on its own).
private struct BoardSheet: View {
    let target: BoardTarget
    let openChat: (String) -> Void
    @State private var detent: PresentationDetent

    init(target: BoardTarget, openChat: @escaping (String) -> Void) {
        self.target = target
        self.openChat = openChat
        _detent = State(initialValue: target.page.wantsFullHeight ? .large : .medium)
    }

    var body: some View {
        BotBoard(botId: target.botId, initial: target.page, openChat: openChat)
            .environment(\.boardPageShown) { page in
                if page.wantsFullHeight, detent != .large {
                    withAnimation(Motion.fadeQuick) { detent = .large }
                }
            }
            .presentationDetents([.medium, .large], selection: $detent)
            .presentationBackground(Theme.surface)
    }
}

extension View {
    /// The confirmation before a workspace is deleted, in the desktop's
    /// words; `delete` runs once it's confirmed.
    func botDeletePrompt(_ bot: Binding<Bot?>, delete: @escaping (Bot) -> Void) -> some View {
        alert(
            "Delete workspace?",
            isPresented: Binding(get: { bot.wrappedValue != nil }, set: { if !$0 { bot.wrappedValue = nil } }),
            presenting: bot.wrappedValue
        ) { bot in
            Button("Cancel", role: .cancel) {}
            Button("Delete", role: .destructive) {
                UINotificationFeedbackGenerator().notificationOccurred(.warning)
                delete(bot)
            }
        } message: { bot in
            Text("“\(bot.name)” and its conversation will be permanently deleted. Your sessions are not affected.")
        }
    }
}

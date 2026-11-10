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
                BotBoard(botId: target.botId, initial: target.page, openChat: openChat)
                    .presentationDetents([.medium, .large])
                    .presentationBackground(Theme.surface)
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

// A workspace: its chat (the ordinary session screen and send path), with
// its board one tap away. Stub: the chat track gives it the workspace's
// header, access note, shortcuts and inline cards.

import SwiftUI

struct BotView: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @Binding var path: [Route]
    @State private var board: BotPage?

    var body: some View {
        if let bot = model.bot(id: botId) {
            SessionView(chatId: bot.chatId, path: $path)
                .id(bot.chatId)
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button {
                            board = .overview
                        } label: {
                            Label("Board", systemImage: "square.grid.2x2")
                        }
                        .accessibilityIdentifier("workspace-board")
                    }
                }
                .sheet(item: $board) { page in
                    BotBoard(botId: botId, page: page)
                }
                .task {
                    // Launch rig: `-sheet board:<page>` opens the board there,
                    // once the push has settled (a sheet presented
                    // mid-transition is dropped).
                    guard let spec = model.launchSheet, let page = BotPage(launchSpec: spec, botId: botId) else {
                        return
                    }
                    model.launchSheet = nil
                    try? await Task.sleep(for: .milliseconds(700))
                    board = page
                }
        } else {
            ContentUnavailableView(
                "No Workspace", systemImage: "questionmark.square.dashed",
                description: Text("It may have been deleted on another device."))
        }
    }
}

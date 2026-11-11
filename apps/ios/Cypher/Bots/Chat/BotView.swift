// A workspace (`Route.bot`): its chat, on the ordinary session screen,
// which adapts to a workspace's chat — its title, board, what needs you,
// shortcuts and start screen (SessionView's Workspace section and
// Bots/Chat/).

import SwiftUI

struct BotView: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @Binding var path: [Route]

    var body: some View {
        if let bot = model.bot(id: botId) {
            SessionView(chatId: bot.chatId, path: $path, route: .bot(botId))
                .id(bot.chatId)
        } else {
            ContentUnavailableView(
                "No Workspace", systemImage: "questionmark.square.dashed",
                description: Text("It may have been deleted on another device."))
        }
    }
}

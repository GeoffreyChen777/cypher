// A workspace's settings (the desktop's bot palette): name and avatar,
// permissions (projects, conversations, archived sessions; work directly,
// coordinate, steer), instructions and, when creating, the device it runs
// on. Saves through `createBot` / `updateBot`; Delete when editing. Stub.

import SwiftUI

struct BotSettingsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// nil: create a new workspace.
    let botId: String?

    var body: some View {
        NavigationStack {
            List {
                if let bot = botId.flatMap(model.bot(id:)) {
                    BotPageHeader(bot: bot)
                } else {
                    Text("New workspace")
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(botId == nil ? "New workspace" : "Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

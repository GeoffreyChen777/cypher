// The projects a workspace sees: the list by device, and one project's
// sessions in scope, the workspace's work there and its routines there.
// Stubs.

import SwiftUI

struct BotProjectListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        let spaces = model.bot(id: botId).flatMap { model.data?.grantedSpaces(of: $0) } ?? []
        BotStubPage(title: "Projects") {
            ForEach(spaces) { space in
                NavigationLink(value: BotPage.project(space.id)) {
                    LabeledContent(space.displayName, value: model.deviceName(space.deviceId))
                }
            }
        }
    }
}

struct BotProjectPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let spaceId: String

    var body: some View {
        let space = model.spaces.first { $0.id == spaceId }
        BotStubPage(title: space?.displayName ?? "Project") {
            ForEach(model.chats(in: spaceId)) { chat in
                Text(chat.displayTitle)
            }
        }
    }
}

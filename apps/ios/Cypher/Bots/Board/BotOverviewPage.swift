// The board's front page: one row per card (Needs you, Work, Routines,
// Projects, Connections, Memory, Activity), each opening its page. Stub.

import SwiftUI

struct BotOverviewPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        let bot = model.bot(id: botId)
        BotStubPage(title: bot?.name ?? "Workspace") {
            if let bot {
                Section { BotPageHeader(bot: bot) }
            }
            Section {
                link("Needs you", .needsYou, count: model.data?.needsYouCount(botId: botId))
                link("Work", .work, count: model.data?.work(of: botId).count)
                link("Routines", .routines, count: model.data?.routines(of: botId).count)
                link("Projects", .projects, count: bot.flatMap { model.data?.grantedSpaces(of: $0).count })
                link("Connections", .connections, count: model.data?.connections(of: botId).count)
                link("Memory", .memory(.workspace(botId: botId)), count: nil)
                link("Activity", .activity, count: nil)
            }
        }
    }

    private func link(_ title: String, _ page: BotPage, count: Int?) -> some View {
        NavigationLink(value: page) {
            LabeledContent(title, value: count.map(String.init) ?? "")
        }
    }
}

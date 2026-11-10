// What needs the owner: the Needs you list, an approval, and a session the
// workspace started that is asking. Stubs.

import SwiftUI

struct NeedsYouPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        BotStubPage(title: "Needs you") {
            ForEach(model.data?.needsYou(botId: botId) ?? []) { item in
                if let page = BotPage.settling(item) {
                    NavigationLink(item.id, value: page)
                } else {
                    Text(item.id)
                }
            }
        }
    }
}

struct ApprovalPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let approvalId: String

    var body: some View {
        let approval = model.data?.approval(id: approvalId)
        BotStubPage(title: "Approval") {
            if let approval {
                BotFact(label: "Action", value: approval.action)
                BotFact(label: "Why", value: approval.reason)
                BotFact(status: approval.status)
            }
        }
    }
}

struct AskingPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let chatId: String
    let workId: String

    var body: some View {
        BotStubPage(title: "Asking") {
            BotFact(label: "Session", value: model.chat(id: chatId)?.displayTitle ?? chatId)
            BotFact(label: "Work", value: model.data?.work(id: workId)?.title ?? workId)
        }
    }
}

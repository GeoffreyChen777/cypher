// A workspace's work: every plan and orchestration, and one in full (its
// status, request, steps with their sessions, result; Approve, Cancel,
// Stop, Delete). Stubs.

import SwiftUI

struct WorkListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        BotStubPage(title: "Work") {
            ForEach(model.data?.work(of: botId) ?? []) { work in
                NavigationLink(value: BotPage.workItem(work.id)) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(work.title)
                        BotStatusText(work.status(), size: 13)
                    }
                }
            }
        }
    }
}

struct WorkItemPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let workId: String

    var body: some View {
        let work = model.data?.work(id: workId)
        BotStubPage(title: work?.title ?? "Work") {
            if let work {
                BotFact(status: work.status())
                ForEach(work.steps) { step in
                    BotFact(label: step.title, status: step.state.status)
                }
            }
        }
    }
}

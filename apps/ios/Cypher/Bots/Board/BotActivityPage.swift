// A workspace's audit log, newest first, filterable as Plans, Sessions,
// Routines, Memory or Connections. Stub.

import SwiftUI

struct BotActivityPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        BotStubPage(title: "Activity") {
            ForEach(model.data?.activity(of: botId) ?? []) { entry in
                VStack(alignment: .leading, spacing: 2) {
                    Text(entry.text)
                    Text("\(entry.kind.group.label) · \(BotTime.ago(entry.at))")
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                }
            }
        }
    }
}

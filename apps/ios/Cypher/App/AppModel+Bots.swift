// AppModel's workspace accessors: one path for the screens over the demo or
// the live registry (the actions and derived lists are WorkspaceData's).

import Foundation

extension AppModel {
    /// Every workspace, oldest first.
    var bots: [Bot] { data?.bots ?? [] }

    var botRecords: BotRecords { data?.botRecords ?? BotRecords() }

    func bot(id: String) -> Bot? {
        data?.bot(id: id)
    }

    /// The active workspace data for an action, or why there is none.
    func workspaceData() throws -> any WorkspaceData {
        guard let data else { throw BotActionError("Not connected.") }
        return data
    }
}

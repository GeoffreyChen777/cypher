// What a workspace can see and do, in a few words: "Billing · reads
// conversations", "3 projects · works directly · starts sessions". One
// wording, `WorkspaceChatText.accessSummary` (chat.rs `access_summary`);
// these are its shorthands over a scope and over this replica's projects.

import Foundation

enum BotAccess {
    /// `spaceName` names a granted project; a project this replica doesn't
    /// list is left out.
    static func summary(
        scope: BotScope, capabilities: BotCapabilities, spaceName: (String) -> String?
    ) -> String {
        var bot = Bot(id: "", name: "", hostDeviceId: "", chatId: "", createdAt: 0)
        bot.scope = scope
        bot.capabilities = capabilities
        return WorkspaceChatText.accessSummary(bot, spaceName: spaceName)
    }
}

extension WorkspaceData {
    /// The summary over this replica's projects.
    func accessSummary(of bot: Bot) -> String {
        WorkspaceChatText.accessSummary(bot) { id in spaces.first { $0.id == id }?.displayName }
    }
}

// Navigation routes shared by every screen, and the stack-building rules.

import Foundation

enum Route: Hashable {
    case space(String)
    case chat(String)
    case newSession(spaceId: String)
    /// A project-less session on a device (its folder is made on send).
    case quickChat(deviceId: String)
}

enum SessionNavigation {
    /// Reuse an existing ancestor instead of building duplicate/cyclic stacks.
    static func opening(_ chatId: String, in path: [Route]) -> [Route] {
        if let index = path.lastIndex(of: .chat(chatId)) {
            return Array(path.prefix(index + 1))
        }
        return path + [.chat(chatId)]
    }

    /// Notification taps may arrive while that chat is already presented, or
    /// while the stack is empty. Rebuilding `[space, parent, chat]` from
    /// scratch re-inserts an on-screen destination at a new index and
    /// NavigationStack crashes. Only pop-to-existing or append the chat.
    static func openingNotification(_ chatId: String, in path: [Route]) -> [Route] {
        opening(chatId, in: path)
    }
}

// How a workspace chat's pieces reach the screen around them: open a
// session on the main path (a step's session, a routine's run, a
// `cypher://session/<id>` link, a row on the board), put text in the
// composer (a plan's Edit), and which `memory` calls are cards.

import SwiftUI

extension EnvironmentValues {
    /// Opens a session by chat id on the screen's navigation path (a hidden
    /// thread opens read-only). The board's pages use it too: from a sheet
    /// it closes the sheet first. nil where nothing can be opened.
    @Entry var openBotChat: ((String) -> Void)? = nil
    /// Replaces the composer's draft with the text, caret at its end.
    @Entry var seedComposer: ((String) -> Void)? = nil
    /// The `memory` calls (tool part ids) drawn as held-write cards.
    @Entry var transcriptMemoryCards: Set<String> = []
}

/// In-app session links: `cypher://session/<chat id>` in a reply (a
/// workspace cites the sessions an answer draws on this way;
/// markdown/session_link.rs).
enum SessionLink {
    static let prefix = "cypher://session/"

    /// The chat id a link names, when `url` is a well-formed session link.
    static func chatId(_ url: String) -> String? {
        guard url.hasPrefix(prefix) else { return nil }
        var id = Substring(url.dropFirst(prefix.count))
        while id.hasSuffix("/") { id = id.dropLast() }
        guard !id.isEmpty, id.count <= 128,
            id.unicodeScalars.allSatisfy({
                ($0.isASCII && CharacterSet.alphanumerics.contains($0)) || $0 == "-" || $0 == "_"
            })
        else { return nil }
        return String(id)
    }

    static func chatId(_ url: URL) -> String? {
        chatId(url.absoluteString)
    }

    /// A session link, well-formed or not: never handed to the system.
    static func isSessionLink(_ url: URL) -> Bool {
        url.absoluteString.hasPrefix(prefix)
    }
}

extension BotRecords {
    /// The `memory` calls a held write was made by, or decided: drawn as
    /// cards (memory_card.rs `MemoryCards`).
    var memoryCardIds: Set<String> {
        var ids = Set<String>()
        for store in memoryStores {
            ids.formUnion(store.held.compactMap(\.toolCallId))
            ids.formUnion(store.decided.compactMap(\.toolCallId))
        }
        return ids
    }

    /// The card state of the write `toolCallId` made: still held in some
    /// store, else decided; nil once neither remembers it.
    func memoryCard(_ toolCallId: String) -> MemoryCardState? {
        for store in memoryStores {
            if let write = store.heldForCall(toolCallId) { return .held(store: store, write: write) }
        }
        for store in memoryStores {
            if let decision = store.decidedForCall(toolCallId) { return .decided(saved: decision.saved) }
        }
        return nil
    }

    /// The plan behind a `propose_plan` call: by the call's id, else the
    /// plan of that title in the chat written before the id was known
    /// (bot_plan.rs `find_plan`).
    func plan(forCall callId: String, chatId: String?, title: String = "") -> BotWork? {
        if let work = work.first(where: { $0.toolCallId == callId }) { return work }
        guard let chatId else { return nil }
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return work.first {
            $0.chatId == chatId && $0.toolCallId == nil
                && $0.title.trimmingCharacters(in: .whitespacesAndNewlines) == title
        }
    }
}

/// Where a held-write card stands.
enum MemoryCardState: Equatable {
    case held(store: MemoryStore, write: HeldWrite)
    case decided(saved: Bool)
}

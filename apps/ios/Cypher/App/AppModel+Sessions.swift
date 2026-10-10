// AppModel session-store cache: at most `liveSessionLimit` chats keep their
// rooms; the rest are released.

import Foundation

extension AppModel {
    // MARK: Session stores

    func sessionStore(for chat: Chat) -> SessionStore? {
        if let demo { return demo.sessionStore(for: chat.id) }
        let store = warmSessionStore(for: chat)
        if store != nil, recentSessionIds.first != chat.id {
            recentSessionIds.removeAll { $0 == chat.id }
            recentSessionIds.insert(chat.id, at: 0)
        }
        return store
    }

    /// The chat's store, created (hydrated from disk, room started) when
    /// missing. Doesn't touch recency.
    func warmSessionStore(for chat: Chat) -> SessionStore? {
        guard let config else { return nil }
        if let existing = sessionStores[chat.id] {
            existing.hostDeviceId = chat.deviceId
            return existing
        }
        let store = SessionStore(chatId: chat.id, config: config)
        store.hostDeviceId = chat.deviceId
        sessionStores[chat.id] = store
        if !recentSessionIds.contains(chat.id) { recentSessionIds.append(chat.id) }
        store.start()
        return store
    }

    func releaseSessionStore(chatId: String) {
        trimSessionStores()
    }

    /// Warm the most recently active sessions, up to the live limit: stores
    /// hydrate from disk instantly and keep their rooms syncing, so opening
    /// one never shows a loading state. Any other chat hydrates from its disk
    /// snapshot on open and catches up over one connection.
    func preloadSessions() {
        let all = overviewChats + projectlessChats + quickChats
        if let demo {
            all.forEach { _ = demo.sessionStore(for: $0.id) }  // offline, no rooms
            return
        }
        let active =
            all
            .sorted { ($0.lastMessageAt ?? $0.createdAt) > ($1.lastMessageAt ?? $1.createdAt) }
        for chat in active where sessionStores.count < Self.liveSessionLimit {
            _ = warmSessionStore(for: chat)
        }
    }

    /// Stop and drop stores beyond the live limit. A store whose local writes
    /// haven't reached the room yet stays until a later trim — its push queue
    /// is in memory only.
    func trimSessionStores() {
        for id in recentSessionIds.dropFirst(Self.liveSessionLimit) {
            guard let store = sessionStores[id] else {
                recentSessionIds.removeAll { $0 == id }
                continue
            }
            Task { @MainActor [weak self] in
                guard await !store.hasUnpushedUpdates(), let self,
                    self.sessionStores[id] === store,
                    let rank = self.recentSessionIds.firstIndex(of: id),
                    rank >= Self.liveSessionLimit
                else { return }
                self.sessionStores.removeValue(forKey: id)
                self.recentSessionIds.remove(at: rank)
                store.stop()
            }
        }
    }
}

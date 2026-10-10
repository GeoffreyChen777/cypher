// Device-scoped data behind the slash menu: the host's command list and the
// chat's Pi session modes (composer.rs fetch_slash_modes).

import Foundation
import Observation

/// One composer's device-scoped command list. Loaded once per host device
/// (prefetched with the model catalog, so the first `/` is instant); a
/// later load invalidates older replies.
@MainActor @Observable
final class RemoteCommandCatalog {
    private(set) var deviceId: String?
    private(set) var commands: [SlashCommand] = []
    private(set) var loading = false
    private(set) var error: String?
    private var generation = UUID()

    func load(deviceId: String, force: Bool = false,
              fetch: (String) async throws -> [SlashCommand]) async {
        if !force, self.deviceId == deviceId, loading || (error == nil && !commands.isEmpty) { return }
        let ticket = UUID()
        generation = ticket
        self.deviceId = deviceId
        commands = []
        error = nil
        loading = true
        defer {
            if generation == ticket { loading = false }
        }
        do {
            let result = try await fetch(deviceId)
            guard generation == ticket, !Task.isCancelled else { return }
            commands = result
        } catch {
            guard generation == ticket, !Task.isCancelled else { return }
            self.error = SlashMenu.errorMessage(error)
        }
    }
}

/// The chat's Pi switches for the menu's badges, asked of its host each time
/// the menu opens (composer.rs `fetch_slash_modes`). A failed ask keeps the
/// last reading for the same chat; a host too old to answer, or one that
/// never has, leaves the badges off.
@MainActor @Observable
final class SlashModesCatalog {
    private(set) var chatId: String?
    private(set) var modes: PiSessionModes?
    private var generation = UUID()

    func load(chatId: String, fetch: (String) async throws -> PiSessionModes) async {
        let ticket = UUID()
        generation = ticket
        if self.chatId != chatId {
            self.chatId = chatId
            modes = nil
        }
        guard let result = try? await fetch(chatId), generation == ticket, !Task.isCancelled else { return }
        modes = result
    }
}

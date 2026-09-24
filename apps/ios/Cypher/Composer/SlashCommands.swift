// Slash command menu — the desktop composer's `/` popup (composer.rs
// slash_token / refilter_slash / accept_slash) as a list above the phone's
// composer. Commands come from the chat's host (`ListCommands`, relay-
// forwardable); picking one only rewrites the draft. A slash command is sent
// as ordinary Run text — the Pi harness intercepts its built-ins
// (`/compact`, `/export-html`) and runs the rest itself.

import SwiftUI

/// proto agent.rs `SlashCommand`.
struct SlashCommand: Hashable, Decodable {
    var name: String
    var description: String?
    var inputHint: String?

    /// "description · hint", or whichever exists.
    var detail: String? {
        let parts = [description, inputHint].compactMap { $0?.isEmpty == false ? $0 : nil }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

enum SlashMenu {
    /// The filter text while the draft is a bare `/token`: the menu belongs
    /// to the command name only, so it closes at the first whitespace (the
    /// arguments) or a second `/` (a typed path, not a command). The phone's
    /// approximation of the desktop's caret-in-first-token rule.
    static func query(in draft: String) -> String? {
        guard draft.hasPrefix("/") else { return nil }
        let rest = draft.dropFirst()
        guard !rest.contains(where: \.isWhitespace), !rest.contains("/") else { return nil }
        return String(rest)
    }

    /// settings/commands.rs `default_hides`: skills, llama, NewAPI
    /// providers, compact-ui and MCP plumbing. Hidden commands still run
    /// when typed. The desktop's customized list is a local file there, so
    /// the phone applies the defaults.
    static func hiddenByDefault(_ name: String) -> Bool {
        let name = name.lowercased()
        return name.hasPrefix("skill:") || name.hasPrefix("llama") || name.hasPrefix("newapi-")
            || name.hasPrefix("compact-ui") || name == "mcp" || name.hasPrefix("mcp-")
            || name.hasPrefix("pi-mcp")
    }

    /// popover.rs `filter_indices`: case-insensitive on the name; prefix
    /// matches, then substring matches, each in the harness's order.
    static func filter(_ commands: [SlashCommand], query: String) -> [SlashCommand] {
        let visible = commands.filter { !hiddenByDefault($0.name) }
        let needle = query.lowercased()
        guard !needle.isEmpty else { return visible }
        let prefix = visible.filter { $0.name.lowercased().hasPrefix(needle) }
        let substring = visible.filter {
            let name = $0.name.lowercased()
            return !name.hasPrefix(needle) && name.contains(needle)
        }
        return prefix + substring
    }

    /// The draft after picking `command`: the token becomes `/name `, ready
    /// for arguments. Nothing is sent.
    static func accept(_ command: SlashCommand) -> String {
        "/\(command.name) "
    }

    /// composer.rs `slash_error_message`, for relay failures.
    static func errorMessage(_ error: Error) -> String {
        if case RelayError.rpc(let message) = error, message.lowercased().contains("unknown method") {
            return "The session's device runs an older Cypher — update it to list commands"
        }
        switch error as? RelayError {
        case .notConnected, .hostOffline, .timeout:
            return "The session's device is unreachable"
        default:
            return "Couldn't load this agent's commands"
        }
    }
}

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

/// The popup: matching commands over the composer, tap to fill the draft.
struct SlashMenuView: View {
    let catalog: RemoteCommandCatalog
    let query: String
    let onPick: (SlashCommand) -> Void
    let onRetry: () -> Void

    var body: some View {
        let matches = SlashMenu.filter(catalog.commands, query: query)
        Group {
            if catalog.loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    status("Loading commands…")
                }
                .padding(.vertical, 14)
            } else if let error = catalog.error {
                Button(action: onRetry) {
                    VStack(spacing: 4) {
                        status(error)
                        Text("Tap to retry")
                            .font(Theme.sans(12, weight: .medium))
                            .foregroundStyle(Theme.textMuted)
                    }
                    .padding(.vertical, 12)
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.plain)
            } else if matches.isEmpty {
                status(emptyMessage)
                    .padding(.vertical, 14)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(matches, id: \.name) { command in
                            row(command)
                        }
                    }
                    .padding(.vertical, 4)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(maxHeight: 232)
            }
        }
        .frame(maxWidth: .infinity)
        .glassEffect(.regular.tint(Theme.surface.opacity(0.72)),
                     in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityIdentifier("slash-menu")
    }

    private var emptyMessage: String {
        if catalog.commands.isEmpty { return "This agent has no slash commands" }
        if catalog.commands.allSatisfy({ SlashMenu.hiddenByDefault($0.name) }) {
            return "All slash commands are hidden"
        }
        return "No matching commands"
    }

    private func status(_ text: String) -> some View {
        Text(text)
            .font(Theme.sans(13))
            .foregroundStyle(Theme.textMuted)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 16)
    }

    private func row(_ command: SlashCommand) -> some View {
        Button {
            UISelectionFeedbackGenerator().selectionChanged()
            onPick(command)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text("/\(command.name)")
                    .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                if let detail = command.detail {
                    Text(detail)
                        .font(Theme.sans(12.5, relativeTo: .caption))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(2)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("slash-command-\(command.name)")
    }
}

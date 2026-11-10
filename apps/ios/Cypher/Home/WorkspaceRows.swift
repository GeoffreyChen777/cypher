// Home's Workspaces: a section over the sessions (All only) with a row per
// workspace — its avatar, name and one status line, the most pressing
// first: what needs you, working, paused, its device offline, else its
// latest message. A tap opens the workspace; a swipe or long press offers
// Settings, Pause / Resume and Delete.

import SwiftUI

/// What a workspace row's status line says.
enum WorkspaceRowStatus: Equatable {
    case needsYou(Int)
    case working
    case paused
    case offline(device: String)
    /// The latest message and when, or nothing yet.
    case latest(preview: String?, at: Int64?)
}

extension AppModel {
    /// A workspace row's status: what needs you, its chat or any of its
    /// threads running, a pause, its device offline, else its latest message.
    func workspaceRowStatus(_ bot: Bot) -> WorkspaceRowStatus {
        guard let data else { return .latest(preview: nil, at: nil) }
        let status = data.botStatus(bot)
        if case .needsYou(let n) = status { return .needsYou(n) }
        let threadWorking = allChats.contains { $0.bot?.botId == bot.id && indicator(for: $0) == .working }
        if status == .working || threadWorking { return .working }
        if bot.paused { return .paused }
        if !data.botHostOnline(bot) { return .offline(device: deviceName(bot.hostDeviceId)) }
        let chat = data.botChat(bot)
        return .latest(preview: chat?.lastMessagePreview, at: chat?.lastMessageAt)
    }

    /// The workspaces whose names match a search, oldest first.
    func listedBots(matching query: String) -> [Bot] {
        let query = query.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return bots }
        return bots.filter { $0.name.localizedCaseInsensitiveContains(query) }
    }
}

struct WorkspaceRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dynamicTypeSize) private var typeSize
    let bot: Bot

    var body: some View {
        HStack(spacing: 12) {
            BotAvatar(bot: bot, size: 34)
            VStack(alignment: .leading, spacing: 3) {
                Text(bot.name)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
                statusLine
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("workspace-row-\(bot.id)")
    }

    @ViewBuilder private var statusLine: some View {
        switch model.workspaceRowStatus(bot) {
        case .needsYou(let n):
            Text(WorkspaceChatText.needsYou(n))
                .fontWeight(.medium)
                .foregroundStyle(Theme.warning)
        case .working:
            HStack(spacing: 6) {
                MiniSpinner(cellSize: 2.2)
                    .foregroundStyle(Theme.statusWorking)
                Text("Working…").foregroundStyle(Theme.textMuted)
            }
        case .paused:
            Text("Paused").foregroundStyle(Theme.textMuted)
        case .offline(let device):
            Text("\(device) is offline").foregroundStyle(Theme.textMuted)
        case .latest(let preview, let at):
            HStack(spacing: 0) {
                Text(preview.map(Self.oneLine) ?? "No messages yet")
                    .truncationMode(.tail)
                if let at {
                    Text(" · \(relativeTime(at))").fixedSize()
                }
            }
            .foregroundStyle(Theme.textMuted)
        }
    }

    private static func oneLine(_ text: String) -> String {
        text.split(whereSeparator: \.isNewline).joined(separator: " ")
    }
}

/// The Workspaces section; nothing when there are none.
struct WorkspaceSection: View {
    @Environment(AppModel.self) private var model
    let bots: [Bot]
    let settings: (Bot) -> Void
    let delete: (Bot) -> Void

    var body: some View {
        if !bots.isEmpty {
            Section {
                ForEach(bots) { bot in
                    RouteRow(route: .bot(bot.id)) {
                        WorkspaceRow(bot: bot)
                    }
                    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                        // Not `.destructive`: that role animates the row
                        // away before the confirmation is answered.
                        Button {
                            delete(bot)
                        } label: {
                            Label("Delete", systemImage: "trash")
                        }
                        .tint(Theme.danger)
                        pauseButton(bot)
                            .tint(.gray)
                    }
                    .contextMenu {
                        Button("Settings…", systemImage: "slider.horizontal.3") { settings(bot) }
                        pauseButton(bot)
                        Divider()
                        Button("Delete…", systemImage: "trash", role: .destructive) { delete(bot) }
                    }
                }
            } header: {
                ListSectionHeader(title: "Workspaces")
            }
        }
    }

    private func pauseButton(_ bot: Bot) -> some View {
        Button {
            Task { try? await model.workspaceData().setBotPaused(botId: bot.id, paused: !bot.paused) }
        } label: {
            Label(bot.paused ? "Resume" : "Pause", systemImage: bot.paused ? "play" : "pause")
        }
    }
}

/// " · by Atlas" after a session row's time, when a workspace's work
/// started the session.
struct StartedByLabel: View {
    @Environment(AppModel.self) private var model
    let chatId: String

    var body: some View {
        if let started = model.data?.startedBy(chatId: chatId), let bot = model.bot(id: started.work.botId) {
            Text(" · by \(bot.name)")
                .foregroundStyle(Theme.textFaint)
                .lineLimit(1)
                .fixedSize()
                .accessibilityLabel("Started by \(bot.name)")
        }
    }
}

/// The settings sheet's subject: a workspace, or nil for a new one.
struct BotSettingsTarget: Identifiable {
    var botId: String?
    var id: String { botId ?? "new" }
}

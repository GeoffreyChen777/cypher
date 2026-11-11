// The projects a workspace sees (crates/ui/src/shell/bots/projects.rs): the
// list grouped by device, and one project — where it lives, its sessions in
// the workspace's scope, the workspace's work there and its routines there.

import SwiftUI

struct BotProjectListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        let bot = model.bot(id: botId)
        let groups = bot.flatMap { model.data?.projectGroups($0) } ?? []
        BotPageList(title: "Projects") {
            ForEach(groups) { group in
                Section {
                    ForEach(group.projects) { entry in
                        NavigationLink(value: BotPage.project(entry.id)) {
                            ProjectRow(
                                entry: entry, mode: bot.flatMap { model.data?.boardProjectMode($0, space: entry.space) }
                            )
                        }
                        .accessibilityIdentifier("project-row-\(entry.id)")
                    }
                } header: {
                    ListSectionHeader(title: model.deviceName(group.deviceId)) {
                        DeviceOnlineText(online: model.deviceOnline(group.deviceId))
                    }
                }
                .groupedRowStyle()
            }
        }
        .overlay {
            if groups.isEmpty {
                BotEmptyState(
                    symbol: "folder", title: "No projects yet",
                    hint: "Choose which projects \(bot?.name ?? "the workspace") may read in its Settings.")
            }
        }
    }
}

/// A project on the list: its name, its sessions with what runs or asks
/// there, how the workspace works in it, and when it was last active.
struct ProjectRow: View {
    let entry: ProjectEntry
    /// "works directly", "via sessions".
    var mode: String?

    var body: some View {
        let load = entry.load
        BoardRow(
            title: entry.space.displayName,
            sub: ([entry.sessionsLine] + [mode].compactMap { $0 }).joined(separator: " · "),
            trailing: entry.lastActivity.map { BotTime.ago($0) }
        ) {
            BoardGlyph(symbol: "folder")
        } extra: {
            if load.asking > 0 || load.running > 0 {
                HStack(spacing: 10) {
                    if load.asking > 0 {
                        Label("\(load.asking) asking", systemImage: "questionmark.circle")
                            .foregroundStyle(Theme.warning)
                    }
                    if load.running > 0 {
                        HStack(spacing: 5) {
                            MiniSpinner(cellSize: 1.8).foregroundStyle(Theme.statusWorking)
                            Text("\(load.running) running")
                        }
                        .foregroundStyle(Theme.text)
                    }
                }
                .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                .labelStyle(CompactLabelStyle())
                .padding(.top, 1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// An icon and its words, close together.
private struct CompactLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon.font(.system(size: 11, weight: .semibold))
            configuration.title
        }
    }
}

/// A device's state beside its name: "Online", or "Offline" faint.
struct DeviceOnlineText: View {
    let online: Bool

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(online ? Theme.success : Theme.textFaint)
                .frame(width: 6, height: 6)
            Text(online ? "Online" : "Offline")
                .font(Theme.sans(13, relativeTo: .footnote))
                .foregroundStyle(online ? Theme.textMuted : Theme.textFaint)
        }
        .accessibilityElement(children: .combine)
    }
}

struct BotProjectPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.botOpenChat) private var openChat
    @Environment(\.openBotRoute) private var openRoute
    let botId: String
    let spaceId: String

    var body: some View {
        if let bot = model.bot(id: botId), bot.scope.seesSpace(spaceId),
            let space = model.spaces.first(where: { $0.id == spaceId })
        {
            page(bot, space: space)
        } else {
            BotEmptyState(
                symbol: "folder", title: "Not in this workspace",
                hint: "The workspace no longer sees this project."
            )
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle("Project")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func page(_ bot: Bot, space: Space) -> some View {
        let sessions = ProjectLoads.sessions(bot.scope, chats: model.allChats, spaceId: space.id)
        let work = ProjectLoads.work(model.data?.work(of: bot.id) ?? [], spaceId: space.id)
        let routines = ProjectLoads.routines(model.data?.routinesInOrder(of: bot.id) ?? [], spaceId: space.id)
        return BotPageList(title: "Project") {
            Section {
                BotPageHeader(title: space.displayName) {
                    BotHeaderGlyph(symbol: "folder")
                }
                BotFact(label: "Device") {
                    HStack(spacing: 8) {
                        Text(model.deviceName(space.deviceId))
                        DeviceOnlineText(online: model.deviceOnline(space.deviceId))
                    }
                }
                CopyPathFact(label: "Folder", path: space.path)
                if let mode = model.data?.boardProjectMode(bot, space: space) {
                    BotFact(label: "\(bot.name)", value: mode.prefix(1).uppercased() + mode.dropFirst())
                }
            }
            .groupedRowStyle()

            if let openRoute {
                Section {
                    Button {
                        openRoute(.space(space.id))
                    } label: {
                        Label("Open project", systemImage: "arrow.up.forward.square")
                            .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    }
                    .accessibilityIdentifier("project-open")
                }
                .groupedRowStyle()
            }

            Section {
                if sessions.isEmpty {
                    Text("No sessions here yet.")
                        .font(Theme.sans(15, relativeTo: .body))
                        .foregroundStyle(Theme.textMuted)
                }
                let slot = ChatRow.needsStatusSlot(sessions, in: model)
                ForEach(sessions) { chat in
                    Button {
                        openChat(chat.id)
                    } label: {
                        HStack(spacing: 8) {
                            ChatRow(chat: chat, showLocation: false, statusSlot: slot)
                            Image(systemName: "chevron.right")
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundStyle(Theme.textFaint)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("project-session-\(chat.id)")
                }
            } header: {
                ListSectionHeader(title: "Sessions")
            }
            .groupedRowStyle()

            if !work.isEmpty {
                Section {
                    ForEach(work) { item in
                        NavigationLink(value: BotPage.workItem(item.id)) {
                            WorkRow(work: item)
                        }
                    }
                } header: {
                    ListSectionHeader(title: "Work")
                }
                .groupedRowStyle()
            }

            if !routines.isEmpty {
                Section {
                    ForEach(routines) { routine in
                        NavigationLink(value: BotPage.routine(routine.id)) {
                            RoutineRow(routine: routine)
                        }
                    }
                } header: {
                    ListSectionHeader(title: "Routines")
                }
                .groupedRowStyle()
            }
        }
    }
}

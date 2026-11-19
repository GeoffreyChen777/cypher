// The board's front page (board.rs, docs/design/agent-bots.md §13.1): the
// workspace's face, what it may do and, when something is off, one line
// saying so; then a section per card, top to bottom, each drawn only when it
// has something to show: Needs you (first, so attention is never below the
// fold), Work, Routines, Projects and Connections (Routines and Connections
// keep a quiet row when empty), then Activity and Memory. A row's state is
// its mark, at its left; its right end is one quiet note. Rows open their
// pages; approvals and plans also decide in place.

import SwiftUI

struct BotOverviewPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var actions = BotActionRunner()

    /// Finished work the overview keeps under the running ones.
    static let recentWork = 3
    /// Routines it lists before "All routines".
    static let routineRows = 5
    /// Connections it lists before "All connections".
    static let connectionRows = 5

    var body: some View {
        if let data = model.data, let bot = data.bot(id: botId) {
            content(data, bot)
        } else {
            ContentUnavailableView(
                "No Workspace", systemImage: "questionmark.square.dashed",
                description: Text("It may have been deleted on another device."))
        }
    }

    private func content(_ data: any WorkspaceData, _ bot: Bot) -> some View {
        BotPageList(title: bot.name) {
            header(data, bot)
            needsYou(data, bot)
            work(data)
            routines(data)
            projects(data, bot)
            connections(data)
            Section {
                NavigationLink(value: BotPage.activity) {
                    BoardRow(title: "Activity") { BoardGlyph(symbol: "waveform.path.ecg") }
                }
                .accessibilityIdentifier("board-activity")
                NavigationLink(value: BotPage.memory(.workspace(botId: bot.id))) {
                    BoardRow(title: "Memory") { BoardGlyph(symbol: "book") }
                }
                .accessibilityIdentifier("board-memory")
            }
            .groupedRowStyle()
        }
        // The header block names the workspace; the bar stays quiet.
        .toolbar {
            ToolbarItem(placement: .principal) { Text("") }
        }
        .botActionAlert(actions)
    }

    // MARK: Header

    private func header(_ data: any WorkspaceData, _ bot: Bot) -> some View {
        Section {
            HStack(spacing: 14) {
                BotAvatar(bot: bot, size: 52)
                VStack(alignment: .leading, spacing: 3) {
                    Text(bot.name)
                        .font(Theme.sans(22, weight: .semibold, relativeTo: .title2))
                        .foregroundStyle(Theme.text)
                        .lineLimit(2)
                        .accessibilityAddTraits(.isHeader)
                    Text(data.accessSummary(of: bot))
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(3)
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 4)
            if let notice = data.boardNotice(bot) {
                noticeRow(notice, bot: bot)
            }
        }
        .groupedRowStyle()
    }

    /// Something off, in one line; a pause offers Resume in place.
    private func noticeRow(_ notice: BoardNotice, bot: Bot) -> some View {
        let (symbol, text, tint): (String, String, Color) =
            switch notice {
            case .paused: ("pause.circle", "\(bot.name) is paused", Theme.warning)
            case .offline(let host): ("icloud.slash", "\(host) is offline", Theme.textMuted)
            case .failed: ("exclamationmark.triangle", "The last run failed", Theme.danger)
            }
        return HStack(spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 14, weight: .medium))
                .frame(width: 18)
                .accessibilityHidden(true)
            Text(text)
                .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                .frame(maxWidth: .infinity, alignment: .leading)
            if notice == .paused {
                Button("Resume") {
                    actions.run { try await model.workspaceData().setBotPaused(botId: bot.id, paused: false) }
                }
                .buttonStyle(.glass)
                .disabled(actions.busy)
                .accessibilityIdentifier("board-resume")
            }
        }
        .foregroundStyle(tint)
    }

    // MARK: Needs you

    @ViewBuilder
    private func needsYou(_ data: any WorkspaceData, _ bot: Bot) -> some View {
        let rows = data.boardNeedRows(bot)
        if !rows.isEmpty {
            Section {
                ForEach(rows) { row in
                    NeedsYouRowLink(row: row, actions: actions)
                }
            } header: {
                NavigationLink(value: BotPage.needsYou) {
                    ListSectionHeader(title: "Needs you") {
                        Text("\(rows.count)")
                            .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                            .foregroundStyle(Theme.warning)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("board-needs-header")
            }
            .groupedRowStyle()
        }
    }

    // MARK: Work

    @ViewBuilder
    private func work(_ data: any WorkspaceData) -> some View {
        let lists = BoardWorkLists(data.work(of: botId))
        if !lists.isEmpty {
            Section {
                ForEach(lists.running + lists.recent(Self.recentWork)) { work in
                    NavigationLink(value: BotPage.workItem(work.id)) { WorkRow(work: work) }
                        .accessibilityIdentifier("board-work-\(work.id)")
                }
                NavigationLink(value: BotPage.work) {
                    BoardRow(title: "All work", trailing: "\(data.work(of: botId).count)") {
                        BoardGlyph(symbol: "list.bullet.indent")
                    }
                }
                .accessibilityIdentifier("board-all-work")
            } header: {
                ListSectionHeader(title: "Work")
            }
            .groupedRowStyle()
        }
    }

    // MARK: Routines

    private func routines(_ data: any WorkspaceData) -> some View {
        let routines = BoardRoutineState.ordered(data.routines(of: botId))
        return Section {
            if routines.isEmpty {
                NavigationLink(value: BotPage.routines) {
                    BoardRow(title: "No routines yet", sub: "Jobs it runs on a schedule or when you ask.") {
                        BoardGlyph(symbol: "clock")
                    }
                    .opacity(0.7)
                }
            }
            ForEach(routines.prefix(Self.routineRows)) { routine in
                NavigationLink(value: BotPage.routine(routine.id)) {
                    RoutineRow(routine: routine)
                }
                .accessibilityIdentifier("board-routine-\(routine.id)")
            }
            if routines.count > Self.routineRows {
                NavigationLink(value: BotPage.routines) {
                    BoardRow(title: "All routines", trailing: "\(routines.count)") { BoardGlyph(symbol: "clock") }
                }
            }
        } header: {
            ListSectionHeader(title: "Routines") {
                NavigationLink(value: BotPage.routineForm(routineId: nil)) {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.textMuted)
                        .frame(minWidth: 32, minHeight: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New routine")
                .accessibilityIdentifier("board-new-routine")
            }
        }
        .groupedRowStyle()
    }

    // MARK: Projects

    @ViewBuilder
    private func projects(_ data: any WorkspaceData, _ bot: Bot) -> some View {
        let loads = data.boardProjects(bot)
        if !loads.isEmpty {
            Section {
                ForEach(loads) { load in
                    NavigationLink(value: BotPage.project(load.space.id)) {
                        BoardRow(
                            title: load.space.displayName,
                            sub: projectSub(data, bot, load.space),
                            trailing: load.note,
                            trailingColor: load.asking > 0 ? Theme.warning : Theme.textFaint
                        ) {
                            BoardGlyph(symbol: "folder")
                        }
                    }
                    .accessibilityIdentifier("board-project-\(load.space.id)")
                }
            } header: {
                NavigationLink(value: BotPage.projects) {
                    ListSectionHeader(title: "Projects")
                }
                .buttonStyle(.plain)
            }
            .groupedRowStyle()
        }
    }

    /// How the workspace works in a project, led by its device only when
    /// that is not the workspace's own.
    private func projectSub(_ data: any WorkspaceData, _ bot: Bot, _ space: Space) -> String {
        let device = space.deviceId == bot.hostDeviceId ? nil : data.botDeviceName(space.deviceId)
        return [device, data.boardProjectMode(bot, space: space)].compactMap { $0 }.joined(separator: " · ")
    }

    // MARK: Connections

    /// One row per connected workspace (network.rs `render_network_card`):
    /// its state as its mark, who it is, and when the last question was.
    /// An open invite keeps one quiet row; with nothing yet, one row says
    /// how to start.
    private func connections(_ data: any WorkspaceData) -> some View {
        let rows = data.boardConnectionRows(botId)
        let invites = data.openInvites(of: botId)
        return Section {
            ForEach(rows.prefix(Self.connectionRows)) { row in
                NavigationLink(value: BotPage.connection(row.connection.id)) {
                    ConnectionRow(row: row)
                }
                .accessibilityIdentifier("board-connection-\(row.connection.id)")
            }
            if rows.count > Self.connectionRows {
                NavigationLink(value: BotPage.connections) {
                    BoardRow(title: "All connections", trailing: "\(rows.count)") { BoardGlyph(symbol: "person.2") }
                }
            }
            if let soonest = invites.map(\.expiresAt).min() {
                NavigationLink(value: BotPage.connect) {
                    BoardRow(
                        title: invites.count == 1 ? "Invite open" : "\(invites.count) invites open",
                        trailing: inviteLeft(soonest)
                    ) {
                        BoardGlyph(symbol: "key")
                    }
                }
                .accessibilityIdentifier("board-invites")
            }
            if rows.isEmpty && invites.isEmpty {
                NavigationLink(value: BotPage.connect) {
                    BoardRow(title: "Connect a workspace") { BoardGlyph(symbol: "person.2") }
                        .opacity(0.7)
                }
                .accessibilityIdentifier("board-connections")
            }
        } header: {
            ListSectionHeader(title: "Connections") {
                NavigationLink(value: BotPage.connect) {
                    Image(systemName: "plus")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.textMuted)
                        .frame(minWidth: 32, minHeight: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Connect a workspace")
                .accessibilityIdentifier("board-connect")
            }
        }
        .groupedRowStyle()
    }
}

// A workspace's routines (crates/ui/src/shell/bots/schedule.rs): the list
// by next run, and one routine — where and when it runs, its prompt, what
// its last run did and the sessions its runs happen in; Run now, Edit,
// Enabled and Delete. The form is RoutineFormPage.swift.

import SwiftUI

// MARK: - List

struct RoutineListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var runner = BotActionRunner()
    @State private var adding = false
    @State private var deleting: BotRoutine?

    var body: some View {
        TimelineView(.everyMinute) { context in
            let now = Int64(context.date.timeIntervalSince1970 * 1000)
            let routines = model.data?.routinesInOrder(of: botId, now: now) ?? []
            BotPageList(title: "Routines") {
                if let bot = model.bot(id: botId), bot.paused, !routines.isEmpty {
                    Section {
                        RoutinePausedNote(bot: bot)
                    }
                    .groupedRowStyle()
                }
                Section {
                    ForEach(routines) { routine in
                        NavigationLink(value: BotPage.routine(routine.id)) {
                            RoutineRow(routine: routine, now: now)
                        }
                        .swipeActions(edge: .leading) {
                            Button("Run now", systemImage: "play.fill") { run(routine) }
                                .tint(Theme.accent)
                                .disabled(BoardRoutineState(routine, now: now).runBusy)
                        }
                        .swipeActions(edge: .trailing) {
                            Button("Delete", systemImage: "trash") { deleting = routine }
                                .tint(Theme.danger)
                        }
                        .contextMenu {
                            Button("Run now", systemImage: "play") { run(routine) }
                                .disabled(BoardRoutineState(routine, now: now).runBusy)
                            Button("Delete", systemImage: "trash", role: .destructive) { deleting = routine }
                        }
                        .accessibilityIdentifier("routine-row-\(routine.id)")
                    }
                }
                .groupedRowStyle()
            }
            .overlay {
                if routines.isEmpty {
                    BotEmptyState(
                        symbol: "clock.arrow.circlepath", title: "No routines yet",
                        hint: "Run a prompt on a schedule, like a morning brief or a nightly test run.",
                        action: ("Add routine", { adding = true }))
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New routine", systemImage: "plus") { adding = true }
                    .accessibilityIdentifier("routine-new")
            }
        }
        .navigationDestination(isPresented: $adding) {
            RoutineFormPage(botId: botId, routineId: nil)
        }
        .routineDeleteConfirmation($deleting, runner: runner)
        .botActionAlert(runner)
    }

    private func run(_ routine: BotRoutine) {
        runner.run { try await model.workspaceData().runRoutine(routineId: routine.id) }
    }
}

/// A routine on a list: its mark (the workspace's repeat or a project's
/// code), its name and the next run, where and when it runs, and how the
/// last run went. Switched off or done, it fades.
struct RoutineRow: View {
    @Environment(AppModel.self) private var model
    let routine: BotRoutine
    var now: Int64 = nowMs()

    var body: some View {
        let state = BoardRoutineState(routine, now: now)
        BoardRow(
            title: singleLine(routine.name), sub: "\(place) · \(routine.schedule.describe)",
            trailing: state == .running ? nil : state.note(now: now),
            trailingColor: noteColor(state), titleLines: 2
        ) {
            BoardGlyph(symbol: RoutineLook.symbol(routine), color: RoutineLook.tint(routine, bot: bot))
        } extra: {
            if state == .running {
                HStack(spacing: 6) {
                    MiniSpinner(cellSize: 2.0).foregroundStyle(Theme.statusWorking)
                    Text("Running")
                        .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                }
                .padding(.top, 1)
            } else if let run = routine.lastRun {
                HStack(spacing: 0) {
                    BotStatusText(run.outcome.status, size: 13)
                    Text(" · \(BotTime.past(run.at, now: now))")
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                .padding(.top, 1)
            }
        }
        .opacity(state.dim ? 0.55 : 1)
        .accessibilityElement(children: .combine)
    }

    private var bot: Bot? { model.bot(id: routine.botId) }

    /// "Workspace", or the project and its device.
    private var place: String {
        RoutineLook.place(routine.target, model: model)
    }

    private func noteColor(_ state: BoardRoutineState) -> Color {
        switch state {
        case .running: Theme.text
        case .starting, .next: Theme.textMuted
        default: Theme.textFaint
        }
    }
}

/// How a routine is marked and placed, the same on every page.
@MainActor
enum RoutineLook {
    /// The workspace's repeat, or a project's code work (schedule.rs
    /// `job_mark`).
    static func symbol(_ routine: BotRoutine) -> String {
        routine.target.spaceId == nil ? "arrow.clockwise" : "chevron.left.forwardslash.chevron.right"
    }

    /// The workspace's colour on its own routines.
    static func tint(_ routine: BotRoutine, bot: Bot?) -> Color {
        guard routine.target.spaceId == nil else { return Theme.textMuted }
        return BotStyle.color(bot?.color) ?? Theme.textMuted
    }

    /// "Workspace", or "cypher · MacBook Pro".
    static func place(_ target: RoutineTarget, model: AppModel) -> String {
        guard let spaceId = target.spaceId else { return "Workspace" }
        guard let space = model.spaces.first(where: { $0.id == spaceId }) else { return "A project it can't see" }
        return "\(space.displayName) · \(model.deviceName(space.deviceId))"
    }

    /// The device its runs use: its project's, else the workspace's host.
    static func deviceId(_ target: RoutineTarget, bot: Bot, model: AppModel) -> String {
        target.spaceId.flatMap { id in model.spaces.first { $0.id == id }?.deviceId } ?? bot.hostDeviceId
    }
}

/// A paused workspace runs no routine: said once, above them.
struct RoutinePausedNote: View {
    let bot: Bot

    var body: some View {
        Label("\(bot.name) is paused, so no routine runs.", systemImage: "pause.circle")
            .font(Theme.sans(14, relativeTo: .subheadline))
            .foregroundStyle(Theme.warning)
    }
}

extension View {
    /// Confirms deleting `routine` (with its sessions), then deletes it.
    func routineDeleteConfirmation(
        _ routine: Binding<BotRoutine?>, runner: BotActionRunner, deleted: @escaping () -> Void = {}
    ) -> some View {
        modifier(RoutineDeleteConfirmation(routine: routine, runner: runner, deleted: deleted))
    }
}

private struct RoutineDeleteConfirmation: ViewModifier {
    @Environment(AppModel.self) private var model
    @Binding var routine: BotRoutine?
    let runner: BotActionRunner
    let deleted: () -> Void

    func body(content: Content) -> some View {
        content.confirmationDialog(
            "Delete \u{201C}\(routine?.name ?? "")\u{201D}?",
            isPresented: Binding(get: { routine != nil }, set: { if !$0 { routine = nil } }),
            titleVisibility: .visible, presenting: routine
        ) { routine in
            Button("Delete Routine", role: .destructive) {
                runner.run {
                    try await model.workspaceData().deleteRoutine(routineId: routine.id)
                    deleted()
                }
            }
        } message: { _ in
            Text("Its sessions go with it. This can't be undone.")
        }
    }
}

// MARK: - One routine

struct RoutinePage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let botId: String
    let routineId: String
    @State private var runner = BotActionRunner()
    @State private var editing = false
    @State private var deleting: BotRoutine?
    /// Model labels by id, from the routine's device.
    @State private var modelLabels: [String: String] = [:]

    var body: some View {
        if let routine = model.data?.routine(id: routineId), let bot = model.bot(id: routine.botId) {
            TimelineView(.everyMinute) { context in
                page(routine, bot: bot, now: Int64(context.date.timeIntervalSince1970 * 1000))
            }
            .task(id: RoutineLook.deviceId(routine.target, bot: bot, model: model)) {
                await loadModelLabels(RoutineLook.deviceId(routine.target, bot: bot, model: model))
            }
        } else {
            BotEmptyState(
                symbol: "clock.arrow.circlepath", title: "Routine deleted",
                hint: "It may have been deleted on another device.")
                .background(Theme.surface.ignoresSafeArea())
                .navigationTitle("Routine")
                .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func page(_ routine: BotRoutine, bot: Bot, now: Int64) -> some View {
        let state = BoardRoutineState(routine, now: now)
        return BotPageList(title: "Routine") {
            Section {
                BotPageHeader(title: routine.name) {
                    BotHeaderGlyph(symbol: RoutineLook.symbol(routine), tint: RoutineLook.tint(routine, bot: bot))
                }
                if bot.paused {
                    RoutinePausedNote(bot: bot)
                }
            }
            .groupedRowStyle()

            Section {
                BotFact(label: "Status") { BotStatusText(status(routine, state: state)) }
                    .accessibilityIdentifier("routine-status")
                BotFact(label: "Where", value: RoutineLook.place(routine.target, model: model))
                if case .project(_, let worktree, _) = routine.target {
                    BotFact(label: "Checkout", value: worktree ? "Its own worktree" : "The project's checkout")
                    if worktree, let path = routine.worktreePath {
                        CopyPathFact(label: "Worktree", path: path)
                    }
                }
                BotFact(label: "Trigger", value: RoutineWords.trigger(routine.schedule))
                if routine.schedule != .manual {
                    BotFact(label: "Next run", value: RoutineWords.nextRun(state, now: now))
                }
                BotFact(label: "Model", value: modelLabel(routine))
                if routine.target.spaceId == nil {
                    BotFact(label: "May write memory", value: routine.writeNotes ? "Yes" : "No")
                    BotFact(label: "May run commands", value: routine.workDirectly ? "Yes" : "No")
                }
            }
            .groupedRowStyle()

            if state.switchable(routine) {
                Section {
                    Toggle(
                        "Enabled",
                        isOn: Binding(
                            get: { routine.enabled },
                            set: { on in
                                runner.run {
                                    try await model.workspaceData().setRoutineEnabled(
                                        routineId: routine.id, enabled: on)
                                }
                            })
                    )
                    .font(Theme.sans(16, relativeTo: .body))
                    .accessibilityIdentifier("routine-enabled")
                } footer: {
                    Text(routine.enabled ? "Runs on its schedule." : "Runs only when you press Run now.")
                }
                .groupedRowStyle()
            }

            Section {
                Text(routine.prompt)
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } header: {
                ListSectionHeader(title: "Prompt")
            }
            .groupedRowStyle()

            lastRunSection(routine, now: now)
            sessionsSection(routine)
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Delete", systemImage: "trash", role: .destructive) { deleting = routine }
                    .accessibilityIdentifier("routine-delete")
            }
        }
        .safeAreaInset(edge: .bottom) {
            BotDecisionBar(
                decisions: [
                    BotDecision(title: "Edit") { editing = true },
                    BotDecision(title: runTitle(state), role: .primary, enabled: !state.runBusy) {
                        runner.run { try await model.workspaceData().runRoutine(routineId: routine.id) }
                    },
                ])
        }
        .navigationDestination(isPresented: $editing) {
            RoutineFormPage(botId: botId, routineId: routine.id)
        }
        .routineDeleteConfirmation($deleting, runner: runner) { dismiss() }
        .botActionAlert(runner)
    }

    @ViewBuilder
    private func lastRunSection(_ routine: BotRoutine, now: Int64) -> some View {
        Section {
            if let run = routine.lastRun {
                BotFact(label: "Outcome") {
                    if run.outcome == .running {
                        HStack(spacing: 6) {
                            MiniSpinner(cellSize: 2.0).foregroundStyle(Theme.statusWorking)
                            Text("Running").font(Theme.sans(15, weight: .medium, relativeTo: .body))
                        }
                    } else {
                        BotStatusText(run.outcome.status)
                    }
                }
                BotFact(label: run.outcome == .running ? "Started" : "Ran") {
                    Text(BotTime.past(run.at, now: now) + (run.manual ? " · by hand" : ""))
                }
                if let ended = run.endedAt {
                    BotFact(label: "Took", value: BotTime.span(ended - run.at))
                } else if run.outcome == .running {
                    BotFact(label: "Running for", value: BotTime.span(now - run.at))
                }
                if let summary = run.summary?.trimmingCharacters(in: .whitespacesAndNewlines), !summary.isEmpty {
                    BotMarkdown(text: summary, cacheKey: "routine-\(routine.id)-summary")
                        .padding(.vertical, 2)
                }
            } else {
                Text("Not run yet")
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
            }
        } header: {
            ListSectionHeader(title: "Last run")
        }
        .groupedRowStyle()
    }

    /// The sessions its runs happen in: the standing one scheduled runs
    /// continue, and the latest Run now's (both hidden; read-only here).
    @ViewBuilder
    private func sessionsSection(_ routine: BotRoutine) -> some View {
        let sessions = [
            routine.threadChatId.map { (id: $0, label: "Scheduled runs") },
            routine.manualChatId.map { (id: $0, label: "Latest Run now") },
        ].compactMap { $0 }
        if !sessions.isEmpty {
            Section {
                ForEach(sessions, id: \.id) { session in
                    NavigationLink(value: BotThreadLink(chatId: session.id)) {
                        let chat = model.chat(id: session.id)
                        BoardRow(
                            title: session.label,
                            sub: chat?.lastMessagePreview.map(singleLine) ?? "Not on this device yet",
                            trailing: chat.map { BotTime.ago($0.lastMessageAt ?? $0.createdAt) }
                        ) {
                            if let chat, model.indicator(for: chat) != .idle {
                                SessionStatusMark(indicator: model.indicator(for: chat), cellSize: 2.0)
                            } else {
                                BoardGlyph(symbol: "bubble.left")
                            }
                        }
                    }
                    .accessibilityIdentifier("routine-session-\(session.id)")
                }
            } header: {
                ListSectionHeader(title: "Sessions")
            }
            .groupedRowStyle()
        }
    }

    /// The Status fact: what it is doing, else how it stands.
    private func status(_ routine: BotRoutine, state: BoardRoutineState) -> BotStatusLabel {
        switch state {
        case .running: return BotStatusLabel("Running", .active)
        case .starting: return BotStatusLabel("Run requested", .active, symbol: "hourglass")
        case .done: return BotStatusLabel("Done", .done)
        case .off: return BotStatusLabel("Disabled", .muted, symbol: "pause.circle")
        default:
            if routine.lastRunFailed { return BotStatusLabel("Last run failed", .failed) }
            if routine.lastRun?.outcome == .skipped { return BotStatusLabel("Last run skipped", .muted) }
            return state == .manual
                ? BotStatusLabel("Manual", .muted)
                : BotStatusLabel("Enabled", .muted, symbol: "checkmark.circle")
        }
    }

    private func runTitle(_ state: BoardRoutineState) -> String {
        switch state {
        case .running: "Running…"
        case .starting: "Run requested"
        default: "Run now"
        }
    }

    private func modelLabel(_ routine: BotRoutine) -> String {
        guard let id = routine.chosenModel else { return "Default" }
        return modelLabels[id] ?? id
    }

    private func loadModelLabels(_ deviceId: String) async {
        guard model.deviceOnline(deviceId), let models = try? await model.listPiModels(deviceId: deviceId) else {
            return
        }
        modelLabels = Dictionary(models.map { ($0.id, $0.label) }, uniquingKeysWith: { a, _ in a })
    }
}

/// A path fact whose tap copies the path.
struct CopyPathFact: View {
    let label: String
    let path: String
    @State private var copied = false

    var body: some View {
        Button {
            UIPasteboard.general.string = path
            withAnimation(Motion.fadeQuick) { copied = true }
            Task {
                try? await Task.sleep(for: .seconds(1.5))
                withAnimation(Motion.fadeQuick) { copied = false }
            }
        } label: {
            BotFact(label: label) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(path)
                        .font(.system(size: 13, design: .monospaced))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(2)
                        .truncationMode(.middle)
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(copied ? Theme.success : Theme.textFaint)
                }
            }
        }
        .buttonStyle(.plain)
        .accessibilityHint(copied ? "Copied" : "Copies the path")
    }
}

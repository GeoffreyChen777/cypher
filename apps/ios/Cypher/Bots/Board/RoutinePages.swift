// A workspace's routines (crates/ui/src/shell/bots/schedule.rs): the list
// by next run, and one routine: how it stands, how it is set up where that
// is not the default, its prompt, what its last run said and the sessions
// its runs happen in; Run now, Edit, Enabled and Delete. The form is
// RoutineFormPage.swift.

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

/// A routine on a list (schedule.rs `job_item`): its state as its mark,
/// its name, how it is set up where that is not the default, and at the
/// right how long its run has gone or when the next one comes. Switched
/// off or done, it fades.
struct RoutineRow: View {
    @Environment(AppModel.self) private var model
    let routine: BotRoutine
    var now: Int64 = nowMs()

    var body: some View {
        let state = BoardRoutineState(routine, now: now)
        let note =
            if state == .running, let run = routine.lastRun { BotTime.span(now - run.at) } else { state.note(now: now) }
        BoardRow(
            title: singleLine(routine.name),
            sub: routine.setupLine(place: RoutineLook.place(routine.target, bot: bot, model: model)),
            trailing: note, titleLines: 2
        ) {
            RoutineMark(routine: routine, state: state)
        }
        .opacity(state.dim ? 0.55 : 1)
        .accessibilityElement(children: .combine)
    }

    private var bot: Bot? { model.bot(id: routine.botId) }
}

/// A routine's mark, on its row and its page (schedule.rs
/// `job_status_mark`): the ring while a run is under way (amber and still
/// when it waits on the owner), a red mark when its last run failed and
/// `flagFailed` (its row, which has no other place to say so), else the
/// glyph of its kind.
struct RoutineMark: View {
    @Environment(AppModel.self) private var model
    let routine: BotRoutine
    let state: BoardRoutineState
    var flagFailed = true

    var body: some View {
        switch state {
        case .running: ProgressRing(state: model.routineAsking(routine) ? .asking : .running)
        case .starting: ProgressRing(state: .waiting)
        default:
            if flagFailed && routine.lastRunFailed {
                BoardGlyph(symbol: "xmark.circle", color: Theme.danger)
            } else {
                BoardGlyph(
                    symbol: RoutineLook.symbol(routine),
                    color: RoutineLook.tint(routine, bot: model.bot(id: routine.botId)))
            }
        }
    }
}

extension AppModel {
    /// Whether the session a routine's run is in waits on the owner.
    func routineAsking(_ routine: BotRoutine) -> Bool {
        guard let run = routine.lastRun, run.outcome == .running, let chat = run.chatId.flatMap(chat(id:)) else {
            return false
        }
        return indicator(for: chat) == .awaitingInput
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

    /// The project it runs in ("cypher", with its device when that is not
    /// the workspace's own); nil for one that runs in the workspace, which
    /// is the default and goes unsaid.
    static func place(_ target: RoutineTarget, bot: Bot?, model: AppModel) -> String? {
        guard let spaceId = target.spaceId else { return nil }
        guard let space = model.spaces.first(where: { $0.id == spaceId }) else { return "a project it can't see" }
        return model.placeName(space, for: bot)
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
    @Environment(\.botOpenChat) private var openChat
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
                hint: "It may have been deleted on another device."
            )
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle("Routine")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    /// One routine's page, laid out like a work item's: how it stands, its
    /// title and one line of how it is set up; then what it is told to do
    /// and what its last run said. Each thing is said once, and what is
    /// the default (it runs in the workspace, on the default model, in the
    /// project's checkout) is not said at all.
    private func page(_ routine: BotRoutine, bot: Bot, now: Int64) -> some View {
        let state = BoardRoutineState(routine, now: now)
        let asking = model.routineAsking(routine)
        let line = routine.setupLine(
            place: RoutineLook.place(routine.target, bot: bot, model: model),
            model: routine.chosenModel.map { modelLabels[$0] ?? $0 })
        return BotPageList(title: "Routine") {
            Section {
                BotItemHeader(
                    status: RoutineWords.status(
                        state, schedule: routine.schedule, asking: asking, paused: bot.paused, now: now),
                    title: routine.name, details: RoutineWords.statusDetails(routine, state: state, now: now),
                    statusIdentifier: "routine-status"
                ) {
                    // No failure mark here: Last run says it just below.
                    RoutineMark(routine: routine, state: state, flagFailed: false)
                } line: {
                    if !line.isEmpty { Text(line) }
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
                }
                .groupedRowStyle()
            }

            // What it is told to do: a quote, as a plan's request is.
            Section {
                BotQuote(text: routine.prompt, cacheKey: "routine-prompt-\(routine.id)")
                    .textSelection(.enabled)
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

    /// What its last run said, once one has ended: when (with Failed or
    /// Skipped in words; a check for one that went well) and its summary.
    /// A run under way is the status's to say, so it has no section.
    @ViewBuilder
    private func lastRunSection(_ routine: BotRoutine, now: Int64) -> some View {
        if let run = routine.lastRun, run.outcome != .running {
            let when = BotTime.past(run.at, now: now) + (run.manual ? " · by hand" : "")
            Section {
                switch run.outcome {
                case .failed: BotStatusText("Failed · \(when)", tone: .failed)
                case .skipped: BotStatusText("Skipped · \(when)", tone: .muted)
                default: BotStatusText(BotStatusLabel(when, .muted, symbol: "checkmark"))
                }
                if let summary = run.summary?.trimmingCharacters(in: .whitespacesAndNewlines), !summary.isEmpty {
                    BotMarkdown(text: summary, cacheKey: "routine-\(routine.id)-summary")
                        .padding(.vertical, 2)
                }
            } header: {
                ListSectionHeader(title: "Last run")
            }
            .groupedRowStyle()
        }
    }

    /// The sessions its runs happen in: the standing one scheduled runs
    /// continue, and the latest Run now's.
    @ViewBuilder
    private func sessionsSection(_ routine: BotRoutine) -> some View {
        let sessions = [
            routine.threadChatId.map { (id: $0, label: "Scheduled runs") },
            routine.manualChatId.map { (id: $0, label: "Latest Run now") },
        ].compactMap { $0 }
        if !sessions.isEmpty {
            Section {
                ForEach(sessions, id: \.id) { session in
                    let chat = model.chat(id: session.id)
                    // No preview of its last message: Last run says it.
                    let row = BoardRow(
                        title: session.label, sub: chat == nil ? "Not on this device yet" : nil,
                        trailing: chat.map { BotTime.ago($0.lastMessageAt ?? $0.createdAt) }
                    ) {
                        if let chat, model.indicator(for: chat) != .idle {
                            WorkspaceSessionMark(indicator: model.indicator(for: chat))
                        } else {
                            BoardGlyph(symbol: "bubble.left")
                        }
                    }
                    Group {
                        // A project routine's session is an ordinary session:
                        // it opens as one. The workspace's own thread opens
                        // read-only on the board.
                        if let chat, !chat.isReadOnlyThread {
                            Button {
                                openChat(session.id)
                            } label: {
                                row
                            }
                            .tint(Theme.text)
                        } else {
                            NavigationLink(value: BotThreadLink(chatId: session.id)) { row }
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

    private func runTitle(_ state: BoardRoutineState) -> String {
        switch state {
        case .running: "Running…"
        case .starting: "Run requested"
        default: "Run now"
        }
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

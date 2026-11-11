// The routine form, new or edit (crates/ui/src/shell/bots/schedule.rs
// `render_job_form`): its name and prompt, when it runs, where (the
// workspace, or one of its projects with Coordinate), what it may do, its
// model and whether it is on. Saves through the engine, which checks it
// again; its error shows as is.

import SwiftUI

struct RoutineFormPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let botId: String
    /// nil: a new routine.
    let routineId: String?

    @State private var draft = RoutineDraft()
    @State private var loaded = false
    @State private var saving = false
    @State private var error: String?
    @State private var models: ModelChoices = .loading
    @FocusState private var focus: Field?

    private enum Field { case name, prompt }

    /// The device's models, as the form loads them.
    enum ModelChoices: Equatable {
        case loading
        case ready([ModelInfo])
        case unavailable(String)
    }

    var body: some View {
        if let bot = model.bot(id: botId) {
            form(bot)
                .task {
                    guard !loaded else { return }
                    loaded = true
                    if let routineId, let routine = model.data?.routine(id: routineId) {
                        draft = RoutineDraft(routine: routine)
                    } else {
                        focus = .name
                    }
                }
                .task(id: deviceId(bot)) { await loadModels(deviceId(bot)) }
        } else {
            BotEmptyState(symbol: "questionmark.square.dashed", title: "Workspace deleted")
        }
    }

    private var isNew: Bool { routineId == nil }

    private func form(_ bot: Bot) -> some View {
        Form {
            whatSection
            whenSection
            placeSection(bot)
            if draft.spaceId == nil {
                permissionsSection(bot)
            } else {
                checkoutSection
            }
            modelSection(bot)
            Section {
                Toggle("Enabled", isOn: $draft.enabled)
                    .accessibilityIdentifier("routine-form-enabled")
            } footer: {
                Text(draft.enabled ? "Runs on its schedule." : "Runs only when you press Run now.")
            }
            .groupedRowStyle()
        }
        .font(Theme.sans(16, relativeTo: .body))
        .scrollContentBackground(.hidden)
        .background(Theme.surface.ignoresSafeArea())
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle(isNew ? "New routine" : "Edit routine")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            BotDecisionBar(
                decisions: [
                    BotDecision(title: isNew ? "Create routine" : "Save", role: .primary) { save(bot) }
                ],
                busy: saving)
        }
        .alert(
            "Couldn't save the routine",
            isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(error ?? "")
        }
        .disabled(saving)
    }

    // MARK: Sections

    private var whatSection: some View {
        Group {
            Section {
                TextField("Morning summary", text: $draft.name)
                    .focused($focus, equals: .name)
                    .submitLabel(.next)
                    .onSubmit { focus = .prompt }
                    .accessibilityIdentifier("routine-form-name")
            } header: {
                ListSectionHeader(title: "Name")
            }
            .groupedRowStyle()
            Section {
                TextField("What moved, what's stuck, and what needs me?", text: $draft.prompt, axis: .vertical)
                    .lineLimit(4...12)
                    .focused($focus, equals: .prompt)
                    .accessibilityIdentifier("routine-form-prompt")
            } header: {
                ListSectionHeader(title: "What it does each run")
            }
            .groupedRowStyle()
        }
    }

    private var whenSection: some View {
        Section {
            Picker("Runs", selection: $draft.schedule.kind) {
                ForEach(ScheduleDraft.Kind.allCases) { kind in
                    Text(kind.label).tag(kind)
                }
            }
            .pickerStyle(.segmented)
            .listRowSeparator(.hidden)
            .accessibilityIdentifier("routine-form-kind")
            switch draft.schedule.kind {
            case .daily:
                DatePicker("Time", selection: dailyTime, displayedComponents: .hourAndMinute)
                    .accessibilityIdentifier("routine-form-time")
                DayChips(schedule: $draft.schedule)
            case .interval:
                Picker("Every", selection: $draft.schedule.intervalMinutes) {
                    ForEach(draft.schedule.intervalOptions, id: \.self) { minutes in
                        Text(ScheduleDraft.intervalLabel(minutes)).tag(minutes)
                    }
                }
                .accessibilityIdentifier("routine-form-interval")
            case .once:
                DatePicker(
                    "When", selection: $draft.schedule.onceAt, in: Date()...,
                    displayedComponents: [.date, .hourAndMinute]
                )
                .accessibilityIdentifier("routine-form-once")
            case .manual:
                EmptyView()
            }
        } header: {
            ListSectionHeader(title: "When")
        } footer: {
            let hint = draft.scheduleHint()
            Label(hint.text, systemImage: hint.problem ? "exclamationmark.triangle" : "clock")
                .foregroundStyle(hint.problem ? Theme.danger : Theme.textMuted)
                .accessibilityIdentifier("routine-form-hint")
        }
        .groupedRowStyle()
    }

    /// The workspace, or one of its projects: those need Coordinate.
    private func placeSection(_ bot: Bot) -> some View {
        let projects = model.data?.grantedSpaces(of: bot) ?? []
        let coordinate = bot.capabilities.coordinate
        return Section {
            placeRow(
                title: "This workspace", detail: "as its agent", selected: draft.spaceId == nil, enabled: true
            ) { draft.spaceId = nil }
            ForEach(projects) { space in
                let selected = draft.spaceId == space.id
                placeRow(
                    title: space.displayName, detail: model.deviceName(space.deviceId), selected: selected,
                    enabled: coordinate || selected
                ) { draft.spaceId = space.id }
            }
        } header: {
            ListSectionHeader(title: "Runs in")
        } footer: {
            if !coordinate && !projects.isEmpty {
                Text(RoutineDraft.coordinateNote)
            } else if draft.spaceId == nil {
                Text("Runs as the workspace's agent and posts what it found in the workspace chat.")
            } else {
                Text("Runs as an ordinary session in the project.")
            }
        }
        .groupedRowStyle()
    }

    private func placeRow(
        title: String, detail: String, selected: Bool, enabled: Bool, pick: @escaping () -> Void
    ) -> some View {
        Button(action: pick) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(Theme.sans(16, relativeTo: .body))
                        .foregroundStyle(Theme.text)
                    Text(detail)
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                }
                Spacer(minLength: 8)
                if selected {
                    Image(systemName: "checkmark")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.accent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.45)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("routine-form-place-\(title)")
    }

    private func permissionsSection(_ bot: Bot) -> some View {
        Section {
            Toggle(isOn: $draft.writeNotes) {
                toggleLabel("May write memory", "Lets runs add to what the workspace remembers.")
            }
            .accessibilityIdentifier("routine-form-memory")
            if bot.capabilities.workDirectly {
                Toggle(isOn: $draft.workDirectly) {
                    toggleLabel(
                        "May run commands",
                        "In the workspace's projects on its device, e.g. to run tests. Anything that needs approval fails the run."
                    )
                }
                .accessibilityIdentifier("routine-form-commands")
            }
        } header: {
            ListSectionHeader(title: "Permissions")
        } footer: {
            Text("Runs read what the workspace reads: its projects, and its memory.")
        }
        .groupedRowStyle()
    }

    private var checkoutSection: some View {
        Section {
            Toggle("Its own worktree", isOn: $draft.worktree)
                .accessibilityIdentifier("routine-form-worktree")
        } header: {
            ListSectionHeader(title: "Checkout")
        } footer: {
            Text(
                draft.worktree
                    ? "A worktree made on the first run and kept, so runs never touch your checkout. Scheduled runs continue one session; Run now starts a fresh one."
                    : "Runs work in the project's own folder. Scheduled runs continue one session; Run now starts a fresh one."
            )
        }
        .groupedRowStyle()
    }

    private func modelSection(_ bot: Bot) -> some View {
        Section {
            Picker(selection: $draft.model) {
                Text("Default").tag(String?.none)
                if let chosen = draft.model, !modelList.contains(where: { $0.id == chosen }) {
                    Text(chosen).tag(String?.some(chosen))
                }
                ForEach(modelList) { info in
                    Text(info.label).tag(String?.some(info.id))
                }
            } label: {
                HStack(spacing: 8) {
                    Text("Model")
                    if models == .loading {
                        ProgressView().controlSize(.small)
                    }
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("routine-form-model")
        } footer: {
            if case .unavailable(let why) = models {
                Text("Default: the workspace chat's. \(why)")
            } else {
                Text("Default: the workspace chat's.")
            }
        }
        .groupedRowStyle()
    }

    private func toggleLabel(_ title: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            Text(detail)
                .font(Theme.sans(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
        }
    }

    // MARK: Values

    /// The daily time as a date today, for the time picker.
    private var dailyTime: Binding<Date> {
        Binding(
            get: {
                Calendar.current.date(
                    bySettingHour: draft.schedule.hour, minute: draft.schedule.minute, second: 0, of: Date())
                    ?? Date()
            },
            set: { date in
                let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
                draft.schedule.hour = parts.hour ?? draft.schedule.hour
                draft.schedule.minute = parts.minute ?? draft.schedule.minute
            })
    }

    private var modelList: [ModelInfo] {
        if case .ready(let list) = models { return list }
        return []
    }

    /// The device its runs use: its project's, else the workspace's host.
    private func deviceId(_ bot: Bot) -> String {
        RoutineLook.deviceId(draft.target, bot: bot, model: model)
    }

    private func loadModels(_ deviceId: String) async {
        guard model.deviceOnline(deviceId) else {
            models = .unavailable("\(model.deviceName(deviceId)) is offline, so its models can't be listed.")
            return
        }
        models = .loading
        do {
            models = .ready(try await model.listPiModels(deviceId: deviceId))
        } catch {
            models = .unavailable("\(model.deviceName(deviceId)) can't list its models.")
        }
    }

    private func save(_ bot: Bot) {
        let existing = routineId.flatMap { model.data?.routine(id: $0) }
        if routineId != nil && existing == nil {
            error = BotActionError.missing("routine").message
            return
        }
        switch draft.routine(for: bot, existing: existing) {
        case .failure(let failure):
            error = failure.message
        case .success(let routine):
            saving = true
            Task {
                defer { saving = false }
                do {
                    try await model.workspaceData().saveRoutine(routine)
                    dismiss()
                } catch {
                    let device = model.deviceName(bot.hostDeviceId)
                    self.error = BotActionError.from(error, device: device).message
                }
            }
        }
    }
}

/// The Daily editor's days: a chip per weekday, Monday first, and the
/// three common sets in one tap.
private struct DayChips: View {
    @Binding var schedule: ScheduleDraft

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 0) {
                ForEach(1...7, id: \.self) { day in
                    let on = schedule.hasDay(day)
                    Button {
                        UISelectionFeedbackGenerator().selectionChanged()
                        schedule.toggleDay(day)
                    } label: {
                        Text(ScheduleDraft.dayLetters[day - 1])
                            .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                            .foregroundStyle(on ? Theme.bg : Theme.text)
                            .frame(width: 36, height: 36)
                            .background(on ? Theme.text : Theme.text.opacity(0.08), in: Circle())
                            .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .frame(maxWidth: .infinity)
                    .accessibilityLabel(ScheduleDraft.dayNames[day - 1])
                    .accessibilityAddTraits(on ? .isSelected : [])
                    .accessibilityIdentifier("routine-form-day-\(day)")
                }
            }
            HStack(spacing: 8) {
                ForEach(ScheduleDraft.DayPreset.allCases) { preset in
                    let selected = schedule.preset == preset
                    Button {
                        UISelectionFeedbackGenerator().selectionChanged()
                        schedule.days = preset.days
                    } label: {
                        Text(preset.label)
                            .font(Theme.sans(13, weight: .medium, relativeTo: .footnote))
                            .foregroundStyle(selected ? Theme.text : Theme.textMuted)
                            .padding(.horizontal, 12)
                            .frame(minHeight: 30)
                            .background(
                                selected ? Theme.text.opacity(0.14) : Theme.text.opacity(0.05), in: Capsule()
                            )
                            .contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(selected ? .isSelected : [])
                    .accessibilityIdentifier("routine-form-preset-\(preset.label)")
                }
                Spacer(minLength: 0)
            }
        }
        .padding(.vertical, 4)
    }
}

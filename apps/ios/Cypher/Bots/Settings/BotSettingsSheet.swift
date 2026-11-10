// A workspace's settings, or a new workspace (the desktop's bot palette,
// crates/ui/src/shell/bots/dialog.rs): name and avatar, the device it runs
// on (picked once, at creation), permissions (projects, conversations,
// archived sessions; work directly, coordinate, steer) with the access they
// add up to, instructions and, for a new one, its model. Saves through
// `createBot` / `updateBot` in one go; Delete when editing.

import SwiftUI

struct BotSettingsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// nil: create a new workspace.
    let botId: String?
    /// A new workspace was created (its id): open it.
    var onCreated: ((String) -> Void)?
    /// The workspace was deleted.
    var onDeleted: (() -> Void)?

    @State private var draft: BotSettingsDraft?
    @State private var busy = false
    @State private var error: String?
    @State private var confirmDelete = false
    @State private var models: [ModelInfo] = []
    @State private var loadingModels = false

    private var editing: Bool { botId != nil }

    var body: some View {
        NavigationStack {
            Group {
                if let draft = Binding($draft) {
                    form(draft)
                } else {
                    ContentUnavailableView(
                        "No Workspace", systemImage: "questionmark.square.dashed",
                        description: Text("It may have been deleted on another device."))
                }
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle(editing ? "Workspace Settings" : "New Workspace")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .accessibilityIdentifier("bot-settings-cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    if busy {
                        ProgressView()
                    } else {
                        Button(editing ? "Save" : "Create") { save() }
                            .disabled(!canSave)
                            .accessibilityIdentifier("bot-settings-save")
                    }
                }
            }
            .interactiveDismissDisabled(busy)
        }
        .onAppear(perform: seed)
        .task(id: draft?.hostDeviceId) { await loadModels() }
        .confirmationDialog("Delete workspace?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) { delete() }
        } message: {
            Text(
                "“\(model.bot(id: botId ?? "")?.name ?? "This workspace")” and its conversation will be permanently deleted. Your sessions are not affected."
            )
        }
    }

    private var canSave: Bool {
        guard let draft else { return false }
        if Bot.nameError(draft.name) != nil { return false }
        return editing || draft.hostDeviceId.map(model.deviceOnline) == true
    }

    private var hosts: [BotHostOption] {
        BotSettingsDraft.hosts(model.data?.devices ?? [], online: model.deviceOnline)
    }

    private func seed() {
        guard draft == nil else { return }
        if let botId {
            draft = model.bot(id: botId).map(BotSettingsDraft.init(bot:))
        } else {
            draft = BotSettingsDraft(existingCount: model.bots.count, host: hosts.first { $0.online }?.id)
        }
    }

    // MARK: Form

    private func form(_ draft: Binding<BotSettingsDraft>) -> some View {
        List {
            nameSection(draft)
            appearanceSection(draft)
            hostSection(draft)
            permissionsSection(draft)
            instructionsSection(draft)
            if !editing { modelSection(draft) }
            if let error {
                Section {
                    Text(error).font(Theme.sans(13, relativeTo: .footnote)).foregroundStyle(Theme.danger)
                }
                .listRowBackground(Color.clear)
            }
            if editing {
                Section {
                    Button("Delete Workspace…", role: .destructive) { confirmDelete = true }
                        .foregroundStyle(Theme.danger)
                        .disabled(busy)
                        .groupedRowStyle()
                        .accessibilityIdentifier("bot-settings-delete")
                }
            }
        }
    }

    private func nameSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        let value = draft.wrappedValue
        let shown = value.trimmedName.isEmpty ? "Workspace" : value.trimmedName
        return Section {
            HStack(spacing: 14) {
                BotAvatarTile(name: shown, icon: value.icon, color: value.color, size: 48)
                TextField("Workspace name", text: draft.name)
                    .font(Theme.sans(18, weight: .medium, relativeTo: .title3))
                    .submitLabel(.done)
                    .accessibilityIdentifier("bot-settings-name")
            }
            .padding(.vertical, 4)
            .groupedRowStyle()
        } footer: {
            if value.name.count > BotLimits.nameMaxChars {
                Text("Name must be \(BotLimits.nameMaxChars) characters or fewer.").foregroundStyle(Theme.danger)
            }
        }
    }

    // MARK: Appearance

    private func appearanceSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        let value = draft.wrappedValue
        let tint = BotStyle.color(value.color) ?? Theme.text
        return Section("Appearance") {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 40, maximum: 52), spacing: 6)], spacing: 6) {
                ForEach(BotFace.allCases, id: \.self) { face in
                    let on = value.icon == face.rawValue
                    Button {
                        draft.wrappedValue.icon = face.rawValue
                    } label: {
                        BotFaceView(face: face, size: 24, color: on ? tint : Theme.textMuted)
                            .frame(maxWidth: .infinity, minHeight: 40)
                            .background(
                                on ? tint.opacity(0.16) : Color.clear,
                                in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(face.rawValue.replacingOccurrences(of: "-", with: " "))
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
            .padding(.vertical, 4)
            .groupedRowStyle()
            NavigationLink {
                BotSymbolPicker(draft: draft)
            } label: {
                HStack {
                    Text("Symbol or letter")
                    Spacer()
                    if BotFace(rawValue: value.icon ?? "") == nil {
                        BotGlyph(
                            name: value.trimmedName.isEmpty ? "Workspace" : value.trimmedName, icon: value.icon,
                            color: value.color, size: 20)
                    }
                }
            }
            .groupedRowStyle()
            .accessibilityIdentifier("bot-settings-symbols")
            BotColorRow(color: draft.color)
                .padding(.vertical, 6)
                .groupedRowStyle()
        }
    }

    // MARK: Host

    @ViewBuilder
    private func hostSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        if editing {
            Section {
                BotFact(label: "Runs on", value: model.deviceName(draft.wrappedValue.hostDeviceId ?? ""))
                    .groupedRowStyle()
            } footer: {
                Text("A workspace stays on the device it was created on.")
            }
        } else {
            Section {
                if hosts.isEmpty {
                    Text("No device can host a workspace.")
                        .foregroundStyle(Theme.textMuted)
                        .groupedRowStyle()
                }
                ForEach(hosts) { host in
                    let on = draft.wrappedValue.hostDeviceId == host.id
                    Button {
                        draft.wrappedValue.hostDeviceId = host.id
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: host.laptop ? "laptopcomputer" : "desktopcomputer")
                                .foregroundStyle(Theme.textMuted)
                                .frame(width: 24)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(host.name).foregroundStyle(Theme.text)
                                Text(host.online ? "Online" : "Offline")
                                    .font(Theme.sans(13, relativeTo: .footnote))
                                    .foregroundStyle(host.online ? Theme.success : Theme.textMuted)
                            }
                            Spacer()
                            if on {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 15, weight: .semibold))
                                    .foregroundStyle(Theme.text)
                            }
                        }
                    }
                    .disabled(!host.online)
                    .groupedRowStyle()
                    .accessibilityAddTraits(on ? .isSelected : [])
                    .accessibilityIdentifier("bot-settings-host-\(host.id)")
                }
            } header: {
                Text("Run on device")
            } footer: {
                Text("Mac and Linux devices that are online. Fixed once created.")
            }
        }
    }

    // MARK: Permissions

    private func permissionsSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        let value = draft.wrappedValue
        let spaces = model.spaces
        return Section {
            Picker("Projects", selection: draft.access) {
                ForEach(ProjectAccess.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.menu)
            .groupedRowStyle()
            .accessibilityIdentifier("bot-settings-projects")
            if value.access == .selected {
                if spaces.isEmpty {
                    Text("No projects yet. Add one from Home.")
                        .font(Theme.sans(14, relativeTo: .body))
                        .foregroundStyle(Theme.textMuted)
                        .groupedRowStyle()
                }
                ForEach(spaces) { space in
                    let on = value.selected.contains(space.id)
                    Button {
                        draft.wrappedValue.toggle(space.id)
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: on ? "checkmark.circle.fill" : "circle")
                                .font(.system(size: 19))
                                .foregroundStyle(on ? Theme.text : Theme.textFaint)
                            Text(space.displayName).foregroundStyle(Theme.text).lineLimit(1)
                            Spacer()
                            Text(model.deviceName(space.deviceId))
                                .font(Theme.sans(13, relativeTo: .footnote))
                                .foregroundStyle(Theme.textFaint)
                                .lineLimit(1)
                        }
                    }
                    .groupedRowStyle()
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
            PermissionToggle(
                title: "Read conversations", detail: "Message text, not only titles and status.",
                isOn: draft.transcripts, id: "bot-settings-transcripts")
            PermissionToggle(
                title: "Include archived sessions", detail: "Sessions you have archived.",
                isOn: draft.includeArchived, id: "bot-settings-archived")
            PermissionToggle(
                title: "Work directly",
                detail: "Run commands and edit files in its projects on its device, under your permission rules.",
                isOn: draft.capabilities.workDirectly, id: "bot-settings-work-directly")
            PermissionToggle(
                title: "Coordinate", detail: "Plan work and start sessions in its projects, after you approve.",
                isOn: draft.capabilities.coordinate, id: "bot-settings-coordinate")
            PermissionToggle(
                title: "Steer others' sessions",
                detail: "Steer, answer or stop sessions it didn't start, each after you confirm.",
                isOn: draft.capabilities.steer, id: "bot-settings-steer")
        } header: {
            Text("Permissions")
        } footer: {
            Label(value.accessSummary(chats: model.allChats, spaces: spaces), systemImage: "checkmark.shield")
        }
    }

    // MARK: Instructions and model

    private func instructionsSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        let value = draft.wrappedValue
        return Section {
            TextField(
                "Anything it should always keep in mind (optional)", text: draft.instructions, axis: .vertical
            )
            .lineLimit(4...12)
            .font(Theme.sans(15, relativeTo: .body))
            .groupedRowStyle()
            .accessibilityIdentifier("bot-settings-instructions")
        } header: {
            Text("Instructions")
        } footer: {
            if let counter = value.instructionsCounter {
                Text(counter)
                    .foregroundStyle(
                        value.instructions.count > BotLimits.instructionsMaxChars ? Theme.danger : Theme.textMuted)
            }
        }
    }

    private func modelSection(_ draft: Binding<BotSettingsDraft>) -> some View {
        Section {
            Picker("Model", selection: draft.model) {
                Text("Device default").tag(String?.none)
                ForEach(models) { Text($0.label).tag(Optional($0.id)) }
            }
            .pickerStyle(.menu)
            .disabled(models.isEmpty)
            .groupedRowStyle()
            .accessibilityIdentifier("bot-settings-model")
        } footer: {
            if loadingModels {
                Text("Loading the device's models…")
            } else if models.isEmpty {
                Text("The device's default model, until its models load.")
            } else {
                Text("The model the workspace's chat starts with.")
            }
        }
    }

    private func loadModels() async {
        guard !editing, let host = draft?.hostDeviceId, model.deviceOnline(host), let data = model.data else {
            models = []
            return
        }
        loadingModels = true
        defer { loadingModels = false }
        let loaded = (try? await data.piModels(deviceId: host)) ?? []
        guard !Task.isCancelled else { return }
        models = loaded
        if let chosen = draft?.model, !loaded.contains(where: { $0.id == chosen }) { draft?.model = nil }
    }

    // MARK: Actions

    private func save() {
        guard let draft, !busy else { return }
        if let problem = draft.problem {
            error = problem
            return
        }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                let data = try model.workspaceData()
                if let botId {
                    try await data.updateBot(
                        botId: botId, name: draft.trimmedName, icon: draft.icon, color: draft.color,
                        instructions: draft.savedInstructions, scope: draft.scope, capabilities: draft.capabilities)
                    dismiss()
                } else {
                    guard let host = draft.hostDeviceId else {
                        error = "Pick the device the workspace runs on."
                        return
                    }
                    let bot = try await data.createBot(draft.createRequest, on: host)
                    dismiss()
                    onCreated?(bot.id)
                }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func delete() {
        guard let botId else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                try await model.workspaceData().deleteBot(botId: botId)
                dismiss()
                onDeleted?()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// A permission's switch with its one-line explanation.
private struct PermissionToggle: View {
    let title: String
    let detail: String
    @Binding var isOn: Bool
    let id: String

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).foregroundStyle(Theme.text)
                Text(detail)
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .groupedRowStyle()
        .accessibilityIdentifier(id)
    }
}

/// The avatar colours: none, then the project colours.
private struct BotColorRow: View {
    @Binding var color: String?

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 30, maximum: 40), spacing: 8)], spacing: 8) {
            swatch(nil, fill: Theme.text.opacity(0.12), label: "No colour")
            ForEach(BotStyle.colors, id: \.key) { entry in
                swatch(entry.key, fill: BotStyle.color(entry.key) ?? Theme.text, label: entry.key.capitalized)
            }
        }
    }

    private func swatch(_ key: String?, fill: Color, label: String) -> some View {
        let on = color == key
        return Button {
            color = key
        } label: {
            Circle()
                .fill(fill)
                .frame(width: 26, height: 26)
                .padding(3)
                .overlay(Circle().strokeBorder(on ? Theme.text : Color.clear, lineWidth: 2))
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// The symbols, and the name's initial, as a workspace's icon.
private struct BotSymbolPicker: View {
    @Binding var draft: BotSettingsDraft

    var body: some View {
        let tint = BotStyle.color(draft.color) ?? Theme.text
        let name = draft.trimmedName.isEmpty ? "Workspace" : draft.trimmedName
        List {
            Section {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 44, maximum: 56), spacing: 6)], spacing: 6) {
                    cell(nil, on: draft.icon == nil, tint: tint, label: "Initial") {
                        Text(BotStyle.initial(name)).font(Theme.sans(18, weight: .semibold))
                    }
                    ForEach(BotStyle.symbols, id: \.key) { entry in
                        cell(entry.key, on: draft.icon == entry.key, tint: tint, label: entry.key) {
                            Image(systemName: entry.symbol).font(.system(size: 18))
                        }
                    }
                }
                .padding(.vertical, 6)
                .groupedRowStyle()
            } footer: {
                Text("The letter is the name's first.")
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(Theme.surface.ignoresSafeArea())
        .navigationTitle("Symbol")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func cell<Glyph: View>(
        _ key: String?, on: Bool, tint: Color, label: String, @ViewBuilder glyph: () -> Glyph
    ) -> some View {
        Button {
            draft.icon = key
        } label: {
            glyph()
                .foregroundStyle(on ? tint : Theme.textMuted)
                .frame(maxWidth: .infinity, minHeight: 44)
                .background(
                    on ? tint.opacity(0.16) : Color.clear, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

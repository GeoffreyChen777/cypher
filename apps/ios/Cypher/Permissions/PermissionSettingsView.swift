// Settings → Permissions on the phone (crates/ui/src/settings/permissions.rs):
// what agents on a device may touch, read and written on that device through
// the relay. Every change saves at once, like the desktop page.

import SwiftUI

struct PermissionSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var deviceId: String?
    @State private var snapshot: PermissionSnapshot?
    @State private var loading = false
    @State private var saving = false
    @State private var error: String?
    @State private var models: [ModelInfo] = []

    var body: some View {
        NavigationStack {
            Form {
                deviceSection
                if let snapshot {
                    statusSection(snapshot)
                    if snapshot.settings.enabled {
                        approvalsSection(snapshot.settings)
                        toolsSection(snapshot.settings)
                        networkSection(snapshot.settings)
                        commandsSection(snapshot.settings)
                        alwaysSection(snapshot.settings)
                        listsSection(snapshot.settings)
                    }
                } else if loading {
                    Section {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Loading…").foregroundStyle(Theme.textMuted)
                        }
                    }
                }
                if let error {
                    Section {
                        Text(error).font(Theme.sans(13)).foregroundStyle(Theme.danger)
                        Button("Retry") { Task { await load() } }
                    }
                }
            }
            .navigationTitle("Permissions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
                if saving {
                    ToolbarItem(placement: .topBarLeading) { ProgressView().controlSize(.small) }
                }
            }
            .disabled(saving)
        }
        .task(id: deviceId) { await load() }
        .onAppear {
            if deviceId == nil {
                deviceId = model.devices.first { model.deviceOnline($0.id) }?.id ?? model.devices.first?.id
            }
        }
    }

    // MARK: Sections

    private var deviceSection: some View {
        Section {
            Picker("Device", selection: $deviceId) {
                ForEach(model.devices) { device in
                    Text(model.deviceOnline(device.id) ? device.name : "\(device.name) (offline)")
                        .tag(Optional(device.id))
                }
            }
            .accessibilityIdentifier("permissions-device")
        } footer: {
            Text("What agents on this device may touch. Changes apply from the next turn.")
        }
    }

    private func statusSection(_ snapshot: PermissionSnapshot) -> some View {
        Section {
            Toggle(
                isOn: Binding(
                    get: { snapshot.settings.enabled },
                    set: { on in save { $0.enabled = on } })
            ) {
                Label {
                    Text("Permission control")
                } icon: {
                    LineIconView(
                        .shieldCheck, size: 18,
                        color: snapshot.settings.enabled ? Theme.text : Theme.textMuted)
                }
            }
            .disabled(!snapshot.status.installed)
            .accessibilityIdentifier("permissions-enabled")
            if snapshot.unreadable {
                Text(
                    "permissions.json on this device is not valid JSON, so the defaults apply. Saving here replaces it."
                )
                .font(Theme.sans(12))
                .foregroundStyle(Theme.warning)
            }
        } footer: {
            Text(statusText(snapshot))
        }
    }

    /// settings/permissions.rs `status_card`.
    private func statusText(_ snapshot: PermissionSnapshot) -> String {
        let status = snapshot.status
        if !status.installed {
            return status.error ?? "This device's Pi Runtime has no permission control; update it."
        }
        if !snapshot.settings.enabled {
            return
                "Agents read, write and run commands without checks or approval requests. Turn it on to sandbox shell commands and check every file access."
        }
        if !status.sandboxAvailable {
            return
                "\(status.error ?? "The OS sandbox does not work here.") Commands and file tools run without restrictions."
        }
        let mechanism: String
        switch status.implementation {
        case "seatbelt": mechanism = "macOS Seatbelt"
        case let other? where other.hasPrefix("landlock"): mechanism = "Linux Landlock"
        case let other?: mechanism = other
        case nil: mechanism = "the OS sandbox"
        }
        return "Shell commands run in \(mechanism). Each chat's level is set with /permissions in the composer."
    }

    private static let approvalRows: [(key: String, title: String, detail: String)] = [
        (
            "escalate", "Running commands outside the sandbox",
            "A command that needs more than the sandbox (ssh, git push, gh, other processes) runs as you once you allow it."
        ),
        (
            "externalWrite", "Writing outside the workspace",
            "Other projects and folders. Reading them is always allowed."
        ),
        (
            "protectedWrite", "Writing protected files",
            "Git config and hooks, shell startup files, launch agents: a write there runs code later."
        ),
        (
            "sensitiveRead", "Reading sensitive files",
            "Keys, .env files and credentials, for the file tools. Sandboxed commands never read them."
        ),
        ("sensitiveWrite", "Writing sensitive files", "Keys, .env files and credentials, for the file tools."),
        ("network", "Network beyond the filter", "With the filtered network: a domain not on the list."),
        ("tools", "Tools set to Ask", "MCP and extension tools whose setting below is Ask."),
    ]

    /// Sensitive requests and commands leaving the sandbox never go to the
    /// reviewing model.
    private static func allowsModel(_ key: String) -> Bool {
        !["sensitiveRead", "sensitiveWrite", "escalate"].contains(key)
    }

    private func approvalsSection(_ settings: PermissionSettings) -> some View {
        Section {
            ForEach(Self.approvalRows, id: \.key) { row in
                Picker(
                    selection: Binding(
                        get: { settings.approval(row.key) },
                        set: { mode in save { $0.setApproval(row.key, mode) } })
                ) {
                    Text("Ask me").tag("ask")
                    if Self.allowsModel(row.key) { Text("Model first").tag("assisted") }
                    Text("Deny").tag("deny")
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.title)
                        Text(row.detail).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                    }
                }
                .accessibilityIdentifier("permissions-approval-\(row.key)")
            }
            Picker(
                "Reviewing model",
                selection: Binding(
                    get: { settings.judgeModel },
                    set: { id in save { $0.judgeModel = id } })
            ) {
                Text("The chat's model").tag("")
                if !settings.judgeModel.isEmpty, !models.contains(where: { $0.id == settings.judgeModel }) {
                    Text(settings.judgeModel).tag(settings.judgeModel)
                }
                ForEach(models, id: \.id) { model in
                    Text(model.label).tag(model.id)
                }
            }
        } header: {
            Text("Approvals")
        } footer: {
            Text(
                "Inside the workspace, agents read and write freely; they read everything else except sensitive files. Anything more is a request, decided here."
            )
        }
    }

    private func toolsSection(_ settings: PermissionSettings) -> some View {
        Section("Tools") {
            Picker(
                "MCP and extension tools",
                selection: Binding(
                    get: { settings.toolsDefault },
                    set: { decision in save { $0.toolsDefault = decision } })
            ) {
                Text("Allow").tag("allow")
                Text("Ask").tag("elevate")
                Text("Deny").tag("deny")
            }
        }
    }

    private func networkSection(_ settings: PermissionSettings) -> some View {
        Section {
            Picker(
                "Network",
                selection: Binding(
                    get: { settings.networkMode },
                    set: { mode in save { $0.networkMode = mode } })
            ) {
                Text("Open").tag("open")
                Text("Filtered").tag("filtered")
            }
            Toggle(
                "Local servers",
                isOn: Binding(
                    get: { settings.localBinding },
                    set: { on in save { $0.localBinding = on } }))
        } header: {
            Text("Files and network")
        } footer: {
            Text(
                "Filtered: commands reach the network only through a proxy that lets allowed domains through and asks about the rest."
            )
        }
    }

    private func commandsSection(_ settings: PermissionSettings) -> some View {
        Section {
            let commands = settings.commands
            if commands.isEmpty {
                Text(
                    "Nothing yet. Choosing Always allow when the agent asks to run a command outside the sandbox adds it here."
                )
                .font(Theme.sans(13))
                .foregroundStyle(Theme.textMuted)
            }
            ForEach(Array(commands.enumerated()), id: \.offset) { index, rule in
                VStack(alignment: .leading, spacing: 3) {
                    Text(rule.label ?? rule.text)
                        .lineLimit(1)
                    Text(rule.text)
                        .font(Theme.mono(12))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(Self.decisionText(rule))
                        .font(Theme.sans(12))
                        .foregroundStyle(rule.decision == "allow" ? Theme.warning : Theme.textMuted)
                }
                .swipeActions {
                    Button("Remove", role: .destructive) { save { $0.removeCommand(at: index) } }
                }
                .accessibilityIdentifier("permissions-command-\(index)")
            }
        } header: {
            Text("Commands outside the sandbox")
        } footer: {
            Text("Commands you allowed to run as you, with your files, keys and network. Swipe to remove.")
        }
    }

    private static func decisionText(_ rule: PermissionSettings.CommandRule) -> String {
        let decision =
            switch rule.decision {
            case "allow": "Runs outside the sandbox"
            case "deny": "Never runs"
            default: "Always asks"
            }
        return rule.pinned ? "\(decision) · pinned to its current contents" : decision
    }

    private func alwaysSection(_ settings: PermissionSettings) -> some View {
        Section {
            let rules = settings.rules
            if rules.isEmpty {
                Text("Nothing yet. Choosing Always allow in an approval adds the path here.")
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.textMuted)
            }
            ForEach(Array(rules.enumerated()), id: \.offset) { index, rule in
                let (title, detail) = Self.ruleText(rule)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).lineLimit(1).truncationMode(.middle)
                    Text(detail).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                }
                .swipeActions {
                    Button("Remove", role: .destructive) { save { $0.removeRule(at: index) } }
                }
            }
        } header: {
            Text("Always allowed")
        } footer: {
            Text("Paths and tools you approved with Always allow.")
        }
    }

    /// settings/permissions.rs `always_card`.
    private static func ruleText(_ rule: PermissionSettings.Rule) -> (String, String) {
        if rule.path.hasPrefix("tool:") { return ("Use \(rule.path.dropFirst(5))", "Tool") }
        if rule.path.hasPrefix("net:") { return (String(rule.path.dropFirst(4)), "Network") }
        return (rule.path, rule.capability == "write" ? "Read and write" : "Read")
    }

    private func listsSection(_ settings: PermissionSettings) -> some View {
        Section {
            NavigationLink("Sensitive, protected and writable paths") {
                PermissionListsView(settings: settings) { edited in save { $0 = edited } }
            }
        } footer: {
            Text("Patterns added to the built-in lists, and the domains and sockets commands may use.")
        }
    }

    // MARK: Loading and saving

    private func load() async {
        guard let deviceId else { return }
        loading = true
        error = nil
        defer { loading = false }
        do {
            snapshot = try await model.permissionSettings(deviceId: deviceId)
        } catch {
            snapshot = nil
            self.error = "Couldn't load this device's permissions: \(error.localizedDescription)"
        }
        models = (try? await model.listPiModels(deviceId: deviceId)) ?? []
    }

    /// Apply `change` to the settings and save them on the device; the page
    /// shows what the device saved.
    private func save(_ change: @escaping (inout PermissionSettings) -> Void) {
        guard let deviceId, var settings = snapshot?.settings else { return }
        change(&settings)
        saving = true
        error = nil
        Task { @MainActor in
            defer { saving = false }
            do {
                snapshot = try await model.savePermissionSettings(deviceId: deviceId, settings: settings)
            } catch {
                self.error = "Couldn't save: \(error.localizedDescription)"
            }
        }
    }
}

/// The extra pattern, domain and socket lists, one entry per line; saved
/// together.
struct PermissionListsView: View {
    let settings: PermissionSettings
    let save: (PermissionSettings) -> Void

    @State private var texts: [String: String] = [:]
    @Environment(\.dismiss) private var dismiss

    private static let lists: [(key: String, title: String, detail: String)] = [
        ("sensitive", "Sensitive", "Never read or written without asking (~/x, <root>/x, **/x)."),
        ("protected", "Protected", "Readable; writes need approval."),
        ("writableSupport", "Writable", "Commands may write here beside the workspace."),
        ("allowDomains", "Allowed domains", "The filtered network lets these through (*.x covers subdomains)."),
        ("unixSockets", "Unix sockets", "Sockets commands may use (~/.docker/run/docker.sock)."),
    ]

    var body: some View {
        Form {
            ForEach(Self.lists, id: \.key) { list in
                Section {
                    TextEditor(
                        text: Binding(
                            get: { texts[list.key] ?? settings.list(list.key).joined(separator: "\n") },
                            set: { texts[list.key] = $0 })
                    )
                    .font(Theme.mono(12))
                    .frame(minHeight: 72)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                } header: {
                    Text(list.title)
                } footer: {
                    Text(list.detail)
                }
            }
        }
        .navigationTitle("Paths and domains")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Save") {
                    var edited = settings
                    for (key, text) in texts {
                        edited.setList(
                            key,
                            text.split(whereSeparator: \.isNewline)
                                .map { $0.trimmingCharacters(in: .whitespaces) }
                                .filter { !$0.isEmpty })
                    }
                    save(edited)
                    dismiss()
                }
                .disabled(texts.isEmpty)
            }
        }
    }
}

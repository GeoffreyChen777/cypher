import SwiftUI

struct NotificationSettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    private var controller: NotificationController { model.notifications }
    private func setting<T>(_ keyPath: WritableKeyPath<NotificationPreferences, T>) -> Binding<T> {
        Binding(get: { controller.settings[keyPath: keyPath] }, set: { value in
            var next = controller.settings
            next[keyPath: keyPath] = value
            Task { await controller.updateSettings(next) }
        })
    }
    var body: some View {
        NavigationStack {
            Form {
                Section {
                    LabeledContent("iOS permission", value: controller.permission)
                    if controller.permission == "Disabled in iOS Settings" {
                        Button("Open iOS Settings") {
                            if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                        }
                    } else {
                        Button(controller.registered ? "Update notification permissions" : "Enable notifications on this iPhone") {
                            Task { await controller.enable() }
                        }
                        .disabled(!controller.available || controller.busy)
                        Button("Open iOS Settings") {
                            if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                        }
                    }
                    if let error = controller.error {
                        Text(error).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                        Button("Retry") { Task { await controller.refresh() } }
                    }
                    if model.demo != nil { Text("Notifications are not sent in Demo mode.") }
                }
                Section {
                    Toggle("Waiting for input", isOn: setting(\.input))
                    Toggle("Task failed", isOn: setting(\.failed))
                    Toggle("Task completed", isOn: setting(\.completed))
                    Toggle("Subagent results", isOn: setting(\.subagents))
                } footer: {
                    Text("Only the last device used for this session receives its notification. Alerts wait 10 seconds so a new session action can supersede them. Subagent results remain off by default.")
                }
                .disabled(!controller.available || controller.busy)
                Section("Muted projects") {
                    ForEach(model.spaces) { project in
                        Toggle(isOn: Binding(get: {
                            controller.settings.mutedProjects.contains(project.id)
                        }, set: { muted in
                            var next = controller.settings
                            next.mutedProjects.removeAll { $0 == project.id }
                            if muted { next.mutedProjects.append(project.id) }
                            Task { await controller.updateSettings(next) }
                        })) {
                            VStack(alignment: .leading) {
                                Text(project.displayName)
                                Text(model.deviceName(project.deviceId)).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                .disabled(!controller.available || controller.busy)
                Section {
                    Text("The app icon badge counts sessions with unread important events in this account. Opening a session clears its count; opening Home does not clear everything. Enable Badges in iOS Settings if the number is hidden.")
                        .font(.footnote).foregroundStyle(.secondary)
                    Text("Notification preferences apply to this account's workspace. Lock-screen alerts do not include chat text, task details, or project names.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Notifications")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .task { await controller.refresh() }
        }
    }
}

struct InAppNotificationBanner: View {
    let controller: NotificationController
    var body: some View {
        if let banner = controller.banner {
            Button {
                controller.pendingNavigation = banner
                controller.banner = nil
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "bell").font(.system(size: 13))
                    Text(banner.title).font(Theme.sans(13, weight: .medium))
                    Spacer()
                    Image(systemName: "chevron.right").font(.system(size: 10))
                }
                .padding(14)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 16)
            .padding(.top, 52)
        }
    }
}

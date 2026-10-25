// The session list's view menu — the desktop sidebar header's view menu
// (shell/sidebar.rs `render_sidebar_view_menu`) as a toolbar menu: Device
// (on Home), Last active, Sort by and Order. Saved on this phone only, like
// the desktop's ui-settings.

import SwiftUI

/// The view options in AppStorage, shared by every list that reads them.
@propertyWrapper
struct StoredSessionListOptions: DynamicProperty {
    @AppStorage("sessionListSort") private var sort = SessionSort.activity.rawValue
    @AppStorage("sessionListReversed") private var reversed = false
    // 0 = All.
    @AppStorage("sessionListActiveWithinMins") private var activeWithin = 0
    // "" = every device. The key Home's device tabs used.
    @AppStorage("homeDeviceFilter") private var device = ""

    var wrappedValue: SessionListOptions {
        get {
            SessionListOptions(
                sort: SessionSort(rawValue: sort) ?? .activity, reversed: reversed,
                activeWithinMins: activeWithin > 0 ? activeWithin : nil,
                device: device.isEmpty ? nil : device)
        }
        nonmutating set {
            sort = newValue.sort.rawValue
            reversed = newValue.reversed
            activeWithin = newValue.activeWithinMins ?? 0
            device = newValue.device ?? ""
        }
    }
}

struct SessionListMenu: View {
    @Environment(AppModel.self) private var model
    @StoredSessionListOptions private var options
    /// Home filters by device; a project already lives on one.
    var showsDevice = false

    var body: some View {
        Menu {
            if showsDevice && model.devices.count > 1 {
                Picker("Device", selection: binding(\.device)) {
                    Text("All Devices").tag(String?.none)
                    ForEach(model.devices) { device in
                        Text(device.name).tag(String?.some(device.id))
                    }
                }
                .pickerStyle(.menu)
            }
            Picker("Last active", selection: binding(\.activeWithinMins)) {
                Text("All").tag(Int?.none)
                ForEach(SessionListOptions.stops, id: \.self) { mins in
                    Text(SessionListOptions.format(mins)).tag(Int?.some(mins))
                }
            }
            .pickerStyle(.menu)
            Section("Sort by") {
                Picker("Sort by", selection: sortBinding) {
                    ForEach(SessionSort.allCases) { sort in
                        Text(sort.label).tag(sort)
                    }
                }
                .pickerStyle(.inline)
            }
            Section("Order") {
                Picker("Order", selection: descendingBinding) {
                    Label("Ascending", systemImage: "arrow.up").tag(false)
                    Label("Descending", systemImage: "arrow.down").tag(true)
                }
                .pickerStyle(.inline)
            }
        } label: {
            Image(systemName: "slider.horizontal.3")
        }
        // Tinted while a non-default view is active, so a narrowed or
        // reordered list is obvious.
        .tint(narrowed ? Theme.accent : nil)
        .accessibilityLabel("View options")
        .accessibilityIdentifier("session-list-menu")
    }

    private var narrowed: Bool {
        var shown = options
        if !showsDevice || !model.devices.contains(where: { $0.id == shown.device }) { shown.device = nil }
        return !shown.isDefault
    }

    private func binding<T>(_ key: WritableKeyPath<SessionListOptions, T>) -> Binding<T> {
        Binding(
            get: { options[keyPath: key] },
            set: { value in
                var next = options
                next[keyPath: key] = value
                withAnimation(Motion.fadeQuick) { options = next }
            }
        )
    }

    /// A new sort starts in its natural direction.
    private var sortBinding: Binding<SessionSort> {
        Binding(
            get: { options.sort },
            set: { sort in
                var next = options
                if next.sort != sort { next.reversed = false }
                next.sort = sort
                withAnimation(Motion.fadeQuick) { options = next }
            }
        )
    }

    private var descendingBinding: Binding<Bool> {
        Binding(
            get: { options.descending },
            set: { descending in
                var next = options
                next.reversed = next.sort.naturalDescending != descending
                withAnimation(Motion.fadeQuick) { options = next }
            }
        )
    }
}

/// A list the time filter emptied: says so, and offers every session back.
struct FilteredEmptyState: View {
    @StoredSessionListOptions private var options

    var body: some View {
        ContentUnavailableView {
            Label("No Recent Sessions", systemImage: "clock")
        } description: {
            Text("No sessions active in the last \(SessionListOptions.format(options.activeWithinMins)).")
        } actions: {
            Button("Show All Sessions") {
                withAnimation(Motion.fadeQuick) { options.activeWithinMins = nil }
            }
            .buttonStyle(.glass)
        }
        .listRowBackground(Color.clear)
    }
}

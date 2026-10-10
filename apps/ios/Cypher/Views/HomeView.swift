// Home — the mobile shell: the desktop sidebar's project list as a native
// inset-grouped list, filtered by owning-device tabs across the top. A
// project opens into its sessions (SpaceView); close=archive becomes
// swipe-to-archive.

import SwiftUI

struct HomeView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [Route] = []
    @State private var showNewSpace = false
    @State private var showNotifications = false
    @State private var actions = SessionActions()
    /// The in-flight notification navigation (see `scheduleNotificationNavigation`).
    @State private var notificationTask: Task<Void, Never>?
    /// When `path` last changed — a push/pop is likely still animating for a
    /// moment afterwards, and a notification route must not land on top of it.
    @State private var lastPathChangeAt: TimeInterval = 0
    /// The stack's UINavigationController, for `transitionCoordinator`: the
    /// only honest signal that a push/pop (incl. an interactive back swipe)
    /// is still in flight — SwiftUI reports a UIKit-driven pop only once it
    /// has finished.
    @State private var navigation = NavigationProbe()
    // "" = All. Sticky across launches; falls back to All if the device's
    // projects are gone.
    @AppStorage("homeDeviceFilter") private var deviceFilter: String = ""

    /// Registry rows a pending notification may be waiting on: the chat and
    /// its project id, so a late-hydrating row re-triggers the attempt.
    private var chatsKey: String {
        model.allChats.map { "\($0.id):\($0.spaceId ?? "")" }.joined()
    }

    // `body` is split into layers (list → routing → lifecycle) so each
    // type-checks on its own: as one expression it timed out on CI's older
    // Xcode ("unable to type-check this expression in reasonable time").
    var body: some View {
        NavigationStack(path: $path) {
            routedList
                .onChange(of: path) { _, route in
                    lastPathChangeAt = Date().timeIntervalSinceReferenceDate
                    if case .chat(let id) = route.last { model.notifications.viewing(id) }
                    else { model.notifications.viewing(nil) }
                }
                // Notification taps navigate from `onChange`, NOT a `.task` on
                // this root: `.task` is cancelled while a pushed session covers
                // Home, so a tap taken inside a session was swallowed — and the
                // stale request then fired on the way back, replacing the path
                // in the middle of the pop transition (the reported crash).
                // `onChange` stays live while covered, so the tap opens the
                // session immediately, whichever screen the app was on.
                .onChange(of: model.notifications.pendingNavigation?.id, initial: true) { _, _ in
                    scheduleNotificationNavigation()
                }
                .onChange(of: chatsKey) { _, _ in
                    scheduleNotificationNavigation()
                }
                .alert("Notification", isPresented: showsNavigationError) {
                    Button("OK") { model.notifications.navigationError = nil }
                } message: { Text(model.notifications.navigationError ?? "") }
                .task(id: preloadKey) {
                    model.preloadSessions()
                }
                .onAppear {
                    if case .chat(let id) = path.last { model.notifications.viewing(id) }
                    else { model.notifications.viewing(nil) }
                    if let route = model.launchRoute {
                        model.launchRoute = nil
                        // Push the whole stack atomically — appending from a child's
                        // onAppear mid-transition gets dropped by NavigationStack.
                        if case .space(let id) = route, model.launchSheet == "newsession" {
                            model.launchSheet = nil
                            path = [route, .newSession(spaceId: id)]
                        } else {
                            path = [route]
                        }
                    }
                    if model.launchSheet == "newspace" {
                        model.launchSheet = nil
                        showNewSpace = true
                    }
                }
        }
    }

    private var projectList: some View {
        List {
            let groups = deviceGroups
            let selected = groups.first { $0.id == deviceFilter }
            // In the list, under the large title: a top safeAreaBar
            // shifted the scroll inset while the title collapsed (the
            // title flickered and slid under the tabs) and ran the scroll
            // indicator across the tabs. A header rather than a row: a
            // cell masks its content to the section's corner radius,
            // which clipped the first tab.
            if !groups.isEmpty {
                Section {} header: {
                    DeviceTabs(deviceIds: groups.map(\.id), selection: $deviceFilter)
                        .listRowInsets(EdgeInsets())
                        // Out past the section margin to the screen edge.
                        .padding(.horizontal, -DeviceTabs.margin)
                        .textCase(nil)
                }
            }
            // One tab: one card. All: a card per device, named by a plain
            // header (the tabs already carry presence), so rows never
            // repeat their device.
            ForEach(selected.map { [$0] } ?? groups) { group in
                Section {
                    ForEach(group.spaces) { space in
                        // A button rather than a NavigationLink: the card
                        // opens the project without a disclosure chevron.
                        Button {
                            path.append(.space(space.id))
                        } label: {
                            ProjectRow(space: space)
                        }
                        .groupedRowStyle()
                    }
                } header: {
                    if selected == nil && groups.count > 1 {
                        ListSectionHeader(title: model.deviceName(group.id))
                    }
                }
            }
            quickChatsSection(deviceId: selected?.id)
            if selected == nil {
                otherSessionsSection
                ArchivedSection(spaceId: nil, orphanedOnly: true)
            }
        }
        .sessionActionPrompts(actions)
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(Theme.surface.ignoresSafeArea())
        .overlay {
            if model.spaces.isEmpty && orphanedChats.isEmpty && model.quickChats.isEmpty {
                emptyState
            }
        }
        .background(NavigationProbeView(probe: navigation))
        .navigationTitle("Projects")
        .navigationSubtitle(subtitle)
        .navigationBarTitleDisplayMode(.large)
    }

    private var routedList: some View {
        projectList
            .navigationDestination(for: Route.self, destination: destination)
            .toolbar { homeToolbar }
            .sheet(isPresented: $showNewSpace) {
                NewSpaceSheet { spaceId in
                    path.append(.space(spaceId))
                }
            }
            .sheet(isPresented: $showNotifications) { NotificationSettingsView() }
    }

    @ViewBuilder
    private func destination(_ route: Route) -> some View {
        switch route {
        case .space(let id): SpaceView(spaceId: id, path: $path)
        case .chat(let id): SessionView(chatId: id, path: $path).id(id)
        case .newSession(let spaceId): NewSessionView(spaceId: spaceId, path: $path)
        case .quickChat(let deviceId):
            NewSessionView(spaceId: "", path: $path, quickDeviceId: deviceId)
        }
    }

    @ToolbarContentBuilder
    private var homeToolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            accountMenu
        }
        // Account and Add are unrelated: separate glass groups.
        ToolbarSpacer(.fixed, placement: .topBarTrailing)
        ToolbarItem(placement: .topBarTrailing) {
            quickChatMenu
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button {
                showNewSpace = true
            } label: {
                Image(systemName: "plus")
            }
            .accessibilityLabel("New project")
        }
    }

    private var showsNavigationError: Binding<Bool> {
        Binding(
            get: { model.notifications.navigationError != nil },
            set: { if !$0 { model.notifications.navigationError = nil } }
        )
    }

    /// Sessions to preload: re-run when the visible set changes.
    private var preloadKey: String {
        (model.overviewChats + model.projectlessChats + model.quickChats).map(\.id).joined()
    }

    // MARK: Notification navigation

    /// Resolve the pending notification into a route, once the chat row is
    /// known (`chatsKey` re-triggers on late hydration; 12s later it gives
    /// up with a notice). The request is consumed before `path` changes so
    /// nothing can replay it, and the assignment waits out a push/pop that
    /// is still animating — a path replaced mid-transition is undefined.
    private func scheduleNotificationNavigation() {
        guard let pending = model.notifications.pendingNavigation else { return }
        notificationTask?.cancel()
        notificationTask = Task { @MainActor in
            let since = Date().timeIntervalSinceReferenceDate - lastPathChangeAt
            if since < Self.transitionGrace {
                try? await Task.sleep(for: .seconds(Self.transitionGrace - since))
            } else {
                await Task.yield()
            }
            // A user-driven pop (Back, or the edge swipe) is invisible to
            // SwiftUI until it ends; never replace the path underneath it.
            var waited: TimeInterval = 0
            while navigation.transitioning, waited < 3 {
                try? await Task.sleep(for: .milliseconds(50))
                waited += 0.05
            }
            if waited > 0 {
                // Let SwiftUI fold the finished UIKit transition into `path`
                // before the route is computed from it.
                try? await Task.sleep(for: .milliseconds(80))
            }
            guard !Task.isCancelled, model.notifications.pendingNavigation?.id == pending.id else { return }
            guard pending.scope == model.notifications.scope else {
                model.notifications.pendingNavigation = nil
                return
            }
            if let chat = model.chat(id: pending.chatId), chat.spaceId == pending.projectId {
                let route = SessionNavigation.openingNotification(chat.id, in: path)
                model.notifications.pendingNavigation = nil
                if route != path { path = route }
                model.notifications.viewing(chat.id)
            } else {
                try? await Task.sleep(for: .seconds(12))
                guard !Task.isCancelled, model.notifications.pendingNavigation?.id == pending.id else { return }
                model.notifications.pendingNavigation = nil
                model.notifications.navigationError = "This session isn't available in the current workspace."
            }
        }
    }

    /// Longer than a NavigationStack push/pop animation (~0.35s).
    private static let transitionGrace: TimeInterval = 0.6

    // MARK: Chrome

    private var accountMenu: some View {
        Menu {
            if model.demo != nil {
                Text("Demo mode")
            }
            AppearancePicker()
            Button("Notifications") { showNotifications = true }
            Button("Sign out", role: .destructive) { model.signOut() }
        } label: {
            Image(systemName: "person.crop.circle")
        }
        .accessibilityLabel("Account")
    }

    /// Quick chat: pick the device (desktop's palette — offline devices are
    /// listed but can't be picked; the host has to make the folder).
    private var quickChatMenu: some View {
        Menu {
            Section("Quick chat on") {
                ForEach(model.devices) { device in
                    Button {
                        path.append(.quickChat(deviceId: device.id))
                    } label: {
                        Text(device.name)
                        if !model.deviceOnline(device.id) { Text("Offline") }
                    }
                    .disabled(!model.deviceOnline(device.id))
                }
            }
        } label: {
            Image(systemName: "bubble.left")
        }
        .disabled(model.devices.isEmpty)
        .accessibilityLabel("Quick chat")
        .accessibilityIdentifier("quick-chat")
    }

    /// Always one line, so the large title never jumps: connection state
    /// first, then live activity, then the plain project count.
    private var subtitle: String {
        guard model.connected else { return "Connecting…" }
        let indicators = model.overviewChats.map { model.indicator(for: $0) }
        let activity = ChatIndicator.activitySummary(indicators)
        if !activity.isEmpty { return activity.joined(separator: " · ") }
        let count = model.spaces.count
        return count == 1 ? "1 project" : "\(count) projects"
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No Projects", systemImage: "folder.badge.plus")
        } description: {
            Text("Connect Cypher on a Mac or Linux device, then add a project folder. This phone is a remote control — no Runtime is needed here.")
        } actions: {
            Button("Add Project") { showNewSpace = true }
                .buttonStyle(.glass)
        }
    }

    // MARK: Sections

    private struct DeviceGroup: Identifiable {
        let id: String
        var spaces: [Space]
    }

    /// Projects under their owning device, in registry order.
    private var deviceGroups: [DeviceGroup] {
        var groups: [DeviceGroup] = []
        for space in model.spaces {
            if let ix = groups.firstIndex(where: { $0.id == space.deviceId }) {
                groups[ix].spaces.append(space)
            } else {
                groups.append(DeviceGroup(id: space.deviceId, spaces: [space]))
            }
        }
        return groups
    }

    /// Sessions with no live project — desktop "No project" chats, and
    /// history whose project row is gone; never invent a project/device
    /// association for them. (This filtered `overviewChats`, which only
    /// holds live-project chats, so the section never showed anything.)
    private var orphanedChats: [Chat] {
        model.projectlessChats
    }

    /// Quick chats, every device in one card (state.rs merge_scratch_groups);
    /// a device tab narrows it to that device.
    @ViewBuilder private func quickChatsSection(deviceId: String?) -> some View {
        let chats = model.quickChats.filter { deviceId == nil || $0.deviceId == deviceId }
        if !chats.isEmpty {
            let statusSlot = ChatRow.needsStatusSlot(chats, in: model)
            Section {
                ForEach(chats) { chat in
                    NavigationLink(value: Route.chat(chat.id)) {
                        ChatRow(chat: chat, showLocation: true, statusSlot: statusSlot)
                    }
                    .groupedRowStyle()
                    .sessionRowActions(chat)
                }
            } header: {
                ListSectionHeader(title: "Quick chats")
            }
        }
    }

    @ViewBuilder private var otherSessionsSection: some View {
        let orphaned = orphanedChats
        if !orphaned.isEmpty {
            let statusSlot = ChatRow.needsStatusSlot(orphaned, in: model)
            Section {
                ForEach(orphaned) { chat in
                    NavigationLink(value: Route.chat(chat.id)) {
                        ChatRow(chat: chat, showLocation: true, statusSlot: statusSlot)
                    }
                    .groupedRowStyle()
                    .sessionRowActions(chat)
                }
            } header: {
                ListSectionHeader(title: "Other Sessions")
            }
        }
    }
}

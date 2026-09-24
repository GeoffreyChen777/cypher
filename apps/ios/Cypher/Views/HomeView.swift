// Home — the mobile shell: the desktop sidebar's project list as a native
// inset-grouped list, filtered by owning-device tabs across the top. A
// project opens into its sessions (SpaceView); close=archive becomes
// swipe-to-archive.

import SwiftUI

enum Route: Hashable {
    case space(String)
    case chat(String)
    case newSession(spaceId: String)
    /// A project-less session on a device (its folder is made on send).
    case quickChat(deviceId: String)
}

enum SessionNavigation {
    /// Reuse an existing ancestor instead of building duplicate/cyclic stacks.
    static func opening(_ chatId: String, in path: [Route]) -> [Route] {
        if let index = path.lastIndex(of: .chat(chatId)) {
            return Array(path.prefix(index + 1))
        }
        return path + [.chat(chatId)]
    }

    /// Notification taps may arrive while that chat is already presented, or
    /// while the stack is empty. Rebuilding `[space, parent, chat]` from
    /// scratch re-inserts an on-screen destination at a new index and
    /// NavigationStack crashes. Only pop-to-existing or append the chat.
    static func openingNotification(_ chatId: String, in path: [Route]) -> [Route] {
        opening(chatId, in: path)
    }
}


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
                        NavigationLink(value: Route.space(space.id)) {
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

// MARK: - Rows

extension View {
    /// Cell paint shared by every inset-grouped list in the app.
    func groupedRowStyle() -> some View {
        listRowBackground(Theme.groupedRow)
            .listRowSeparatorTint(Theme.border)
    }
}

/// Sentence-case section header in the app's type, not the system's caps.
/// Keeps the system position (aligned with row text), per the list style.
struct ListSectionHeader<Accessory: View>: View {
    let title: String
    @ViewBuilder var accessory: Accessory

    var body: some View {
        HStack(spacing: 8) {
            Text(title)
                .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
                .lineLimit(1)
            Spacer(minLength: 8)
            accessory
        }
        .textCase(nil)
    }
}

extension ListSectionHeader where Accessory == EmptyView {
    init(title: String) {
        self.init(title: title) { EmptyView() }
    }
}

/// "All" plus one rounded-rectangle tab per project-owning device, with its
/// presence dot. Content-layer filters, so solid fills rather than glass
/// (glass belongs to the navigation layer). Scrolls sideways past ~4 devices.
private struct DeviceTabs: View {
    @Environment(AppModel.self) private var model
    let deviceIds: [String]
    @Binding var selection: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                tab(id: "", title: "All", online: nil)
                ForEach(deviceIds, id: \.self) { id in
                    tab(id: id, title: model.deviceName(id), online: model.deviceOnline(id))
                }
            }
        }
        // Tabs start on the cards' edge (the inset-grouped section margin)
        // but scroll out to the screen edge.
        .contentMargins(.horizontal, Self.margin, for: .scrollContent)
    }

    /// The inset-grouped section margin.
    static let margin: CGFloat = 16

    private func tab(id: String, title: String, online: Bool?) -> some View {
        // A filter naming a vanished device reads as All.
        let current = deviceIds.contains(selection) ? selection : ""
        let selected = id == current
        return Button {
            guard !selected else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            withAnimation(Motion.fadeQuick) { selection = id }
        } label: {
            HStack(spacing: 6) {
                if let online {
                    Circle()
                        .fill(online ? Theme.statusCompleted : Theme.textFaint.opacity(0.5))
                        .frame(width: 6, height: 6)
                }
                Text(title)
                    .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                    .foregroundStyle(selected ? Theme.bg : Theme.text)
                    .lineLimit(1)
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .background(selected ? Theme.text : Theme.groupedRow,
                        in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(online.map { "\(title), \($0 ? "online" : "offline")" } ?? title)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// Folder glyph, name, and one summary line carrying the project's activity.
private struct ProjectRow: View {
    @Environment(AppModel.self) private var model
    let space: Space

    var body: some View {
        let chats = model.chats(in: space.id)
        let indicators = chats.map { model.indicator(for: $0) }
        HStack(spacing: 12) {
            LineIconView(space.gitDetected ? .folderWithFiles : .folder, size: 18,
                         color: Theme.textMuted)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 3) {
                Text(space.displayName)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text(summary(count: chats.count, indicators: indicators))
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func summary(count: Int, indicators: [ChatIndicator]) -> String {
        let total = count == 0 ? "No sessions" : count == 1 ? "1 session" : "\(count) sessions"
        return (ChatIndicator.activitySummary(indicators) + [total]).joined(separator: " · ")
    }
}

extension ChatIndicator {
    /// "1 running · 2 need input · 1 failed · 1 done" parts, attention
    /// first. Done/failed count unread runs only (the indicator's meaning).
    static func activitySummary(_ indicators: [ChatIndicator]) -> [String] {
        let running = indicators.filter { $0 == .working }.count
        let input = indicators.filter { $0 == .awaitingInput }.count
        let failed = indicators.filter { $0 == .errored }.count
        let done = indicators.filter { $0 == .completed }.count
        var parts: [String] = []
        if running > 0 { parts.append("\(running) running") }
        if input > 0 { parts.append(input == 1 ? "1 needs input" : "\(input) need input") }
        if failed > 0 { parts.append("\(failed) failed") }
        if done > 0 { parts.append("\(done) done") }
        return parts
    }
}

/// Session row with one reading edge: live status in a leading gutter
/// (Mail's unread-dot slot), the title, then one muted line — checkout (or,
/// outside a project, "project @ device") and time. The trailing edge is
/// left to the disclosure chevron.
struct ChatRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dynamicTypeSize) private var typeSize
    /// Lifts the mark's center from the title baseline to mid x-height.
    @ScaledMetric(relativeTo: .body) private var markLift: CGFloat = 5.5
    let chat: Chat
    var showLocation: Bool
    /// Reserve the status slot. The list decides, so its titles share one
    /// edge: set when any of its rows has a status (see `needsStatusSlot`).
    var statusSlot: Bool

    /// A list of all-idle sessions drops the slot rather than indent every
    /// title past an empty gutter.
    static func needsStatusSlot(_ chats: [Chat], in model: AppModel) -> Bool {
        chats.contains { model.indicator(for: $0) != .idle }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            if statusSlot {
                // Idle keeps the slot, so every title starts on the same edge.
                SessionStatusMark(indicator: model.indicator(for: chat))
                    .frame(width: 10)
                    .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + markLift }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(chat.displayTitle)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    // Accessibility sizes leave room for ~2 words per line.
                    .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
                HStack(spacing: 6) {
                    // Pi is the only new-session harness; mark the exceptions.
                    if let harness = chat.config?.harness, harness != "pi" {
                        HarnessBadge(harness: harness, size: 12, neutral: Theme.textMuted)
                    }
                    // The time never truncates; the checkout gives way first.
                    HStack(spacing: 0) {
                        Text(showLocation ? location : checkoutLabel)
                            .lineLimit(1)
                            .truncationMode(showLocation ? .tail : .middle)
                        Text(" · \(relativeTime(chat.lastMessageAt ?? chat.createdAt))")
                            .fixedSize()
                    }
                }
                .font(Theme.sans(13, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var isWorktree: Bool {
        guard let cwd = chat.cwd, !cwd.isEmpty, let space = model.space(for: chat) else { return false }
        return (cwd as NSString).standardizingPath != (space.path as NSString).standardizingPath
    }

    private var checkoutLabel: String {
        let branch = chat.branch?.trimmingCharacters(in: .whitespacesAndNewlines)
        let branchLabel = branch.flatMap { $0.isEmpty ? nil : $0 }
        if isWorktree, let cwd = chat.cwd {
            if let branchLabel { return "\(branchLabel) (worktree)" }
            let name = ((cwd as NSString).standardizingPath as NSString).lastPathComponent
            return "Worktree \(name)"
        }
        return branchLabel ?? "Current checkout"
    }

    /// "space @ device" (the session header's format). The space name (not
    /// the cwd basename) is what the desktop row shows — they differ once a
    /// space has been renamed, or when the session runs in a worktree off to
    /// the side.
    private var location: String {
        // The folder is named after the chat id — the device says it all.
        if chat.isScratch { return model.deviceName(chat.deviceId) }
        let space = model.space(for: chat)?.displayName
            ?? chat.cwd.map { ($0 as NSString).lastPathComponent }
            ?? "?"
        return "\(space) @ \(model.deviceName(chat.deviceId))"
    }
}

/// Live status as a bare glyph for the row's leading slot (the label is
/// spoken, not shown); blank when idle.
struct SessionStatusMark: View {
    let indicator: ChatIndicator

    var body: some View {
        Group {
            switch indicator {
            case .working:
                MiniSpinner(cellSize: 2.6)
            case .completed:
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .bold))
            case .awaitingInput, .errored:
                Circle().frame(width: 8, height: 8)
            case .idle:
                Color.clear.frame(width: 8, height: 8)
            }
        }
        .foregroundStyle(color)
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label ?? "")
        .accessibilityHidden(label == nil)
    }

    private var label: String? {
        switch indicator {
        case .working: return "Working"
        case .awaitingInput: return "Needs input"
        case .errored: return "Failed"
        case .completed: return "Done"
        case .idle: return nil
        }
    }

    private var color: Color {
        switch indicator {
        case .working: return Theme.statusWorking
        case .awaitingInput: return Theme.accent
        case .errored: return Theme.danger
        case .completed, .idle: return Theme.statusCompleted
        }
    }
}

func relativeTime(_ ms: Int64) -> String {
    let delta = max(0, nowMs() - ms) / 1000
    if delta < 60 { return "now" }
    if delta < 3600 { return "\(delta / 60)m" }
    if delta < 86_400 { return "\(delta / 3600)h" }
    return "\(delta / 86_400)d"
}

/// Weak handle onto the NavigationStack's UIKit controller (see
/// `HomeView.navigation`).
@MainActor
final class NavigationProbe {
    weak var controller: UINavigationController?
    var transitioning: Bool { controller?.transitionCoordinator != nil }
}

/// Zero-size view inside the stack's root whose responder chain climbs
/// through the hosting controller to the stack's UINavigationController.
struct NavigationProbeView: UIViewRepresentable {
    let probe: NavigationProbe

    func makeUIView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.probe = probe
        view.isUserInteractionEnabled = false
        view.backgroundColor = .clear
        return view
    }

    func updateUIView(_ view: ProbeView, context: Context) {
        view.probe = probe
        view.capture()
    }

    final class ProbeView: UIView {
        var probe: NavigationProbe?
        override func didMoveToWindow() {
            super.didMoveToWindow()
            capture()
        }
        override func layoutSubviews() {
            super.layoutSubviews()
            capture()
        }
        func capture() {
            guard let probe, probe.controller == nil else { return }
            var responder: UIResponder? = next
            while let current = responder {
                if let nav = current as? UINavigationController { probe.controller = nav; return }
                if let controller = current as? UIViewController, let nav = controller.navigationController {
                    probe.controller = nav
                    return
                }
                responder = current.next
            }
        }
    }
}

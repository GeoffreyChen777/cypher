// Home — the mobile shell, session-first: every session in one inset-grouped
// list under time headings (Now, Today, Yesterday, …), one tap from launch.
// Project chips narrow it (a long press opens the project's own page,
// SpaceView), search finds any session, archived ones included, and the view
// menu (SessionListMenu) filters by device and time and picks the sort.
// close=archive becomes swipe-to-archive.

import SwiftUI

struct HomeView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [Route] = []
    @State private var showNewSpace = false
    @State private var showNotifications = false
    @State private var showPermissions = false
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
    @StoredSessionListOptions private var listOptions
    /// The project chips' choice. Session-transient: a narrowed list left
    /// over from last time is how sessions go missing.
    @State private var scope: SessionScope = .all
    @State private var query = ""
    @FocusState private var searchFocused: Bool
    /// The search field has scrolled away: the toolbar offers search instead.
    @State private var searchTucked = false
    /// Bumped by the toolbar's search button: scroll back to the field.
    @State private var searchRequests = 0

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
                    if case .chat(let id) = route.last {
                        model.notifications.viewing(id)
                    } else {
                        model.notifications.viewing(nil)
                    }
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
                } message: {
                    Text(model.notifications.navigationError ?? "")
                }
                .task(id: preloadKey) {
                    model.preloadSessions()
                }
                .onAppear {
                    if case .chat(let id) = path.last {
                        model.notifications.viewing(id)
                    } else {
                        model.notifications.viewing(nil)
                    }
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

    private var sessionList: some View {
        let options = effectiveOptions
        let chats = listedChats(options)
        let sections = options.sections(
            chats, now: Date(), indicator: model.indicator(for:), deviceName: model.deviceName)
        let statusSlot = ChatRow.needsStatusSlot(chats, in: model)
        // Inside one project the rows name their checkout, not the project.
        let showLocation: Bool = {
            if case .project = currentScope { return false }
            return true
        }()
        return ScrollViewReader { proxy in
            List {
                if !isEmpty {
                    // A row in the list, not `.searchable`: the system
                    // drawer on iOS 26 starts hidden (pull down to show), and
                    // this one should sit under the title until a swipe up
                    // scrolls it away.
                    Section {
                        searchField
                            .id(Self.searchRowId)
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                    }
                    .listSectionSpacing(.compact)
                }
                sessionRows(chats: chats, sections: sections, statusSlot: statusSlot, showLocation: showLocation)
            }
            .onScrollGeometryChange(for: Bool.self) { geometry in
                geometry.contentOffset.y + geometry.contentInsets.top > Self.searchTuckOffset
            } action: { _, tucked in
                withAnimation(Motion.fadeQuick) { searchTucked = tucked }
            }
            .onChange(of: searchRequests) { _, _ in
                withAnimation { proxy.scrollTo(Self.searchRowId, anchor: .top) }
                Task { @MainActor in
                    try? await Task.sleep(for: .milliseconds(350))
                    searchFocused = true
                }
            }
        }
        .scrollDismissesKeyboard(.immediately)
        .sessionActionPrompts(actions)
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(Theme.surface.ignoresSafeArea())
        .overlay {
            if isEmpty { emptyState }
        }
        .background(NavigationProbeView(probe: navigation))
        .navigationTitle("Sessions")
        .navigationSubtitle(subtitle)
        .navigationBarTitleDisplayMode(.large)
    }

    private static let searchRowId = "home-search-row"
    /// About the field's height: past it, the field is under the bar.
    private static let searchTuckOffset: CGFloat = 40

    @ViewBuilder
    private func sessionRows(
        chats: [Chat], sections: [SessionSection], statusSlot: Bool, showLocation: Bool
    ) -> some View {
        // In the list, under the large title: a top safeAreaBar
        // shifted the scroll inset while the title collapsed (the
        // title flickered and slid under the chips) and ran the scroll
        // indicator across them. A header rather than a row: a cell
        // masks its content to the section's corner radius, which
        // clipped the first chip.
        if !isEmpty {
            Section {
            } header: {
                scopeHeader
                    .listRowInsets(EdgeInsets())
                    // Out past the section margin to the screen edge.
                    .padding(.horizontal, -ScopeChips.margin)
                    .textCase(nil)
            }
        }
        if chats.isEmpty && !isEmpty {
            emptyRows
        }
        ForEach(sections) { section in
            Section {
                ForEach(section.chats) { chat in
                    NavigationLink(value: Route.chat(chat.id)) {
                        ChatRow(chat: chat, showLocation: showLocation, statusSlot: statusSlot)
                    }
                    .groupedRowStyle()
                    .sessionRowActions(chat)
                }
            } header: {
                if !section.title.isEmpty {
                    ListSectionHeader(title: section.title)
                }
            }
        }
        ArchivedSection(scope: currentScope, query: query, startsOpen: false)
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Theme.textMuted)
            TextField("Search sessions", text: $query)
                .font(Theme.sans(16, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .focused($searchFocused)
                .submitLabel(.search)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .accessibilityIdentifier("home-search")
            if !query.isEmpty {
                Button {
                    query = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 15))
                        .foregroundStyle(Theme.textFaint)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear search")
            }
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 40)
        .background(Theme.groupedRow, in: Capsule())
        .contentShape(Capsule())
        .onTapGesture { searchFocused = true }
    }

    private var routedList: some View {
        sessionList
            .navigationDestination(for: Route.self, destination: destination)
            .toolbar { homeToolbar }
            .sheet(isPresented: $showNewSpace) {
                // A new project is for a new session: start one there, and
                // narrow the list to it for the way back.
                NewSpaceSheet { spaceId in
                    scope = .project(spaceId)
                    path.append(.newSession(spaceId: spaceId))
                }
            }
            .sheet(isPresented: $showNotifications) { NotificationSettingsView() }
            .sheet(isPresented: $showPermissions) { PermissionSettingsView() }
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
        ToolbarItem(placement: .topBarLeading) {
            SessionListMenu(showsDevice: true)
        }
        // The search field scrolled away: search from here instead. Tinted
        // while a search is narrowing the list.
        if searchTucked {
            ToolbarItem(placement: .topBarLeading) {
                Button {
                    searchRequests += 1
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .tint(searching ? Theme.accent : nil)
                .accessibilityLabel("Search")
                .accessibilityIdentifier("search-button")
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            accountMenu
        }
        // Account and New are unrelated: separate glass groups.
        ToolbarSpacer(.fixed, placement: .topBarTrailing)
        ToolbarItem(placement: .topBarTrailing) {
            composeMenu
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
        model.activeChats.map(\.id).joined()
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
            Button("Permissions") { showPermissions = true }
                .disabled(model.devices.isEmpty)
            Button("Sign out", role: .destructive) { model.signOut() }
        } label: {
            Image(systemName: "person.crop.circle")
        }
        .accessibilityLabel("Account")
    }

    /// New: a session in the chosen project (else the latest session's),
    /// another project, a quick chat on a device (offline devices are listed
    /// but can't be picked; the host has to make the folder), or a project.
    private var composeMenu: some View {
        let target = composeTarget
        return Menu {
            if let target {
                Button {
                    path.append(.newSession(spaceId: target.id))
                } label: {
                    Label("New Session", systemImage: "square.and.pencil")
                    Text("\(target.displayName) @ \(model.deviceName(target.deviceId))")
                }
            }
            let others = model.listedSpaces(options: SessionListOptions()).filter { $0.id != target?.id }
            if !others.isEmpty {
                Menu("In Another Project", systemImage: "folder") {
                    ForEach(others) { space in
                        Button {
                            path.append(.newSession(spaceId: space.id))
                        } label: {
                            Text(space.displayName)
                            Text(model.deviceName(space.deviceId))
                        }
                    }
                }
            }
            Menu("Quick Chat", systemImage: "bubble.left") {
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
            .disabled(model.devices.isEmpty)
            Divider()
            Button("New Project…", systemImage: "folder.badge.plus") { showNewSpace = true }
        } label: {
            Image(systemName: "square.and.pencil")
        }
        .accessibilityLabel("New")
        .accessibilityIdentifier("compose")
    }

    /// Where New Session goes: the chosen project, else the latest session's.
    private var composeTarget: Space? {
        if case .project(let id) = currentScope { return model.spaces.first { $0.id == id } }
        return model.activeChats.lazy.compactMap { model.space(for: $0) }.first
            ?? model.listedSpaces(options: SessionListOptions()).first
    }

    /// Always one line, so the large title never jumps: connection state
    /// first, then live activity, then the plain session count.
    private var subtitle: String {
        guard model.connected else { return "Connecting…" }
        let activity = ChatIndicator.activitySummary(model.activeChats.map { model.indicator(for: $0) })
        if !activity.isEmpty { return activity.joined(separator: " · ") }
        let count = model.activeChats.count
        return count == 1 ? "1 session" : "\(count) sessions"
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No Projects", systemImage: "folder.badge.plus")
        } description: {
            Text(
                "Connect Cypher on a Mac or Linux device, then add a project folder. This phone is a remote control — no Runtime is needed here."
            )
        } actions: {
            Button("Add Project") { showNewSpace = true }
                .buttonStyle(.glass)
        }
    }

    // MARK: List

    /// Nothing anywhere yet: no project and no session.
    private var isEmpty: Bool {
        model.spaces.isEmpty && model.activeChats.isEmpty
    }

    private var searching: Bool {
        !query.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// The view menu's options as the list applies them: a vanished
    /// device's filter reads as every device, and a search looks past the
    /// time filter (finding an old session is its point).
    private var effectiveOptions: SessionListOptions {
        var options = listOptions
        if let device = options.device, !model.devices.contains(where: { $0.id == device }) {
            options.device = nil
        }
        if searching { options.activeWithinMins = nil }
        return options
    }

    /// The chips' choice; a vanished project's reads as All.
    private var currentScope: SessionScope {
        if case .project(let id) = scope, !model.spaces.contains(where: { $0.id == id }) { return .all }
        return scope
    }

    /// The sessions in scope that match the search, under the view menu.
    private func listedChats(_ options: SessionListOptions) -> [Chat] {
        let liveSpaceIds = Set(model.spaces.map(\.id))
        let scoped = model.activeChats.filter {
            currentScope.contains($0, liveSpaceIds: liveSpaceIds) && model.matches($0, query: query)
        }
        return model.listed(scoped, options: options)
    }

    /// "All", the projects the view menu lists (in its order; the chosen
    /// one stays even when filtered out), then Quick chats and No project
    /// when they hold listed sessions. Each carries its sessions' status.
    private var chips: [ScopeChip] {
        let options = effectiveOptions
        var spaces = model.listedSpaces(options: options)
        if case .project(let id) = currentScope, !spaces.contains(where: { $0.id == id }),
            let space = model.spaces.first(where: { $0.id == id })
        {
            spaces.append(space)
        }
        // Same-named projects on different devices say which is which.
        let names = Dictionary(grouping: spaces, by: \.displayName)
        var chips = [ScopeChip(scope: .all, title: "All")]
        chips += spaces.map { space in
            let title =
                names[space.displayName, default: []].count > 1
                ? "\(space.displayName) · \(model.deviceName(space.deviceId))" : space.displayName
            return ScopeChip(scope: .project(space.id), title: title, indicator: model.spaceIndicator(space.id))
        }
        for (scope, title, chats) in [
            (SessionScope.quickChats, "Quick chats", model.quickChats),
            (SessionScope.noProject, "No project", model.projectlessChats),
        ] where currentScope == scope || !model.listed(chats, options: options).isEmpty {
            let indicator = ChatIndicator.projectSummary(chats.map { model.indicator(for: $0) })
            chips.append(ScopeChip(scope: scope, title: title, indicator: indicator))
        }
        return chips
    }

    /// The chips, and under them the chosen project's place and the way to
    /// its own page.
    private var scopeHeader: some View {
        VStack(alignment: .leading, spacing: 10) {
            ScopeChips(
                chips: chips, selection: $scope,
                openProject: { path.append(.space($0)) },
                newSession: { path.append(.newSession(spaceId: $0)) })
            if case .project(let id) = currentScope, let space = model.spaces.first(where: { $0.id == id }) {
                HStack(spacing: 8) {
                    Text("\(model.deviceName(space.deviceId)) · \(SpaceView.abbreviatingHome(space.path))")
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 8)
                    Button {
                        path.append(.space(id))
                    } label: {
                        HStack(spacing: 3) {
                            Text("Project")
                            Image(systemName: "chevron.right")
                                .font(.system(size: 11, weight: .semibold))
                        }
                        .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                        .foregroundStyle(Theme.accent)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("open-project")
                }
                .padding(.horizontal, ScopeChips.margin)
            }
        }
    }

    /// Why the list is empty: no search match, the time filter, or a scope
    /// without sessions.
    @ViewBuilder private var emptyRows: some View {
        let liveSpaceIds = Set(model.spaces.map(\.id))
        let inScope = model.activeChats.filter { currentScope.contains($0, liveSpaceIds: liveSpaceIds) }
        if searching {
            // Archived matches still list below; say nothing over them.
            let archivedMatch = model.archivedChats().contains {
                currentScope.contains($0, liveSpaceIds: liveSpaceIds) && model.matches($0, query: query)
            }
            if !archivedMatch {
                Section {
                    ContentUnavailableView.search(text: query)
                        .listRowBackground(Color.clear)
                }
            }
        } else if !inScope.isEmpty && effectiveOptions.activeWithinMins != nil {
            Section { FilteredEmptyState() }
        } else {
            Section {
                ContentUnavailableView {
                    Label("No Sessions", systemImage: "bubble.left.and.bubble.right")
                } description: {
                    Text(inScope.isEmpty ? "Nothing here yet." : "No sessions on this device.")
                } actions: {
                    if case .project(let id) = currentScope {
                        Button("New Session") { path.append(.newSession(spaceId: id)) }
                            .buttonStyle(.glass)
                    }
                }
                .listRowBackground(Color.clear)
            }
        }
    }
}

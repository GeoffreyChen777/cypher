// Home — the mobile shell. The desktop sidebar collapses into one screen: a
// space dropdown in the nav bar (default "All") scopes the attention-sorted
// session list below it. Tabs-as-sessions don't fit a phone; close=archive
// becomes swipe-to-archive.

import SwiftUI

enum Route: Hashable {
    case space(String)
    case chat(String)
    case newSession(spaceId: String)
}

enum SessionNavigation {
    /// Reuse an existing ancestor instead of building duplicate/cyclic stacks.
    static func opening(_ chatId: String, in path: [Route]) -> [Route] {
        if let index = path.lastIndex(of: .chat(chatId)) {
            return Array(path.prefix(index + 1))
        }
        return path + [.chat(chatId)]
    }
}

struct HomeView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [Route] = []
    @State private var showNewSpace = false
    @State private var showNotifications = false
    // "" = All. Sticky across launches; falls back to All if the space is gone.
    @AppStorage("homeProjectFilter") private var spaceFilter: String = ""

    private var selectedSpace: Space? {
        model.spaces.first { $0.id == spaceFilter }
    }

    var body: some View {
        NavigationStack(path: $path) {
            List {
                if let selectedSpace {
                    sessionsSection
                    ArchivedSection(spaceId: selectedSpace.id, path: $path)
                } else {
                    projectsSection
                    ArchivedSection(spaceId: nil, path: $path, orphanedOnly: true)
                }
            }
            .listStyle(.plain)
            .environment(\.defaultMinListRowHeight, 10)
            .contentMargins(.top, 2, for: .scrollContent)
            .scrollContentBackground(.hidden)
            .scrollEdgeEffectStyle(.soft, for: .top)
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle("Cypher")  // feeds the back menu; not displayed
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(.hidden, for: .navigationBar)
            .toolbar(removing: .title)
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .space(let id): SpaceView(spaceId: id, path: $path)
                case .chat(let id): SessionView(chatId: id, path: $path).id(id)
                case .newSession(let spaceId): NewSessionView(spaceId: spaceId, path: $path)
                }
            }
            .toolbar {
                // One leading item: a second topBarLeading entry gets folded
                // into a "…" overflow next to the dropdown. The item's SHARED
                // glass is hidden and the selector wears its own capsule, so
                // the connect spinner sits bare on the bar beside it instead
                // of inside the button's glass.
                ToolbarItem(placement: .topBarLeading) {
                    HStack(spacing: 10) {
                        spaceDropdown
                            // The hidden shared glass still reserves its
                            // content inset, landing the capsule's edge at
                            // ~30pt while the list rows' rail starts at 20 —
                            // pull it back onto the content's left line.
                            .padding(.leading, -10)
                        // In the bar, not the list: as a list row it appeared
                        // and vanished with the connection and shoved the
                        // content down.
                        if !model.connected {
                            ProgressView()
                                .controlSize(.mini)
                                .tint(Theme.textMuted)
                                .accessibilityLabel("Connecting")
                        }
                    }
                }
                .sharedBackgroundVisibility(.hidden)
                ToolbarItem(placement: .topBarTrailing) {
                    newButton
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        if model.demo != nil {
                            Text("Demo mode")
                        }
                        AppearancePicker()
                        Button("Notifications") { showNotifications = true }
                        Button("Sign out", role: .destructive) { model.signOut() }
                    } label: {
                        Image(systemName: "person.circle")
                    }
                }
            }
            .sheet(isPresented: $showNewSpace) {
                NewSpaceSheet { spaceId in
                    path.append(.space(spaceId))
                }
            }
            .sheet(isPresented: $showNotifications) { NotificationSettingsView() }
            .onChange(of: path) { _, route in
                if case .chat(let id) = route.last { model.notifications.viewing(id) }
                else { model.notifications.viewing(nil) }
            }
            .task(id: "\(model.notifications.pendingNavigation?.id ?? "")/\(model.allChats.map { "\($0.id):\($0.spaceId ?? "")" }.joined())") {
                guard let pending = model.notifications.pendingNavigation else { return }
                guard pending.scope == model.notifications.scope else {
                    model.notifications.pendingNavigation = nil
                    return
                }
                if let chat = model.chat(id: pending.chatId), chat.spaceId == pending.projectId {
                    var route: [Route] = [.space(pending.projectId)]
                    if let relation = chat.child, let parent = model.chat(id: relation.parentChatId),
                       parent.deviceId == chat.deviceId { route.append(.chat(parent.id)) }
                    route.append(.chat(chat.id))
                    path = route
                    model.notifications.viewing(chat.id)
                    model.notifications.pendingNavigation = nil
                } else {
                    try? await Task.sleep(for: .seconds(12))
                    guard !Task.isCancelled, model.notifications.pendingNavigation?.id == pending.id else { return }
                    model.notifications.pendingNavigation = nil
                    model.notifications.navigationError = "This session isn't available in the current workspace."
                }
            }
            .alert("Notification", isPresented: Binding(
                get: { model.notifications.navigationError != nil },
                set: { if !$0 { model.notifications.navigationError = nil } }
            )) {
                Button("OK") { model.notifications.navigationError = nil }
            } message: { Text(model.notifications.navigationError ?? "") }
            .task(id: model.overviewChats.map(\.id).joined()) {
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

    // MARK: Space dropdown

    /// The nav-bar dropdown that scopes the session list — a NATIVE glass
    /// menu. Rows are Buttons, not a Picker: Picker menu rows drop two-Text
    /// subtitles, while Button rows map to UIAction subtitles, so each space
    /// shows its owning device ("@ mac") on the small second line without the
    /// three-line title wraps. Selection carries a checkmark in the icon slot.
    private var spaceDropdown: some View {
        Menu {
            spaceMenuButton(id: "", title: "Projects", subtitle: nil)
            ForEach(model.spaces) { space in
                spaceMenuButton(id: space.id, title: space.displayName,
                                subtitle: deviceTag(space))
            }
            Divider()
            Button {
                showNewSpace = true
            } label: {
                Label("New project…", systemImage: "folder.badge.plus")
            }
        } label: {
            HStack(spacing: 5) {
                Text(selectedSpace?.displayName ?? "Projects")
                    .font(Theme.sans(14, weight: .semibold))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Theme.textFaint)
            }
            // Keep long space names from swallowing the whole bar; the owning
            // device lives on the menu rows ("@ mac"), not up here.
            .frame(maxWidth: 220, alignment: .leading)
            // Its own glass capsule (the item's shared glass is hidden so the
            // connect spinner doesn't ride inside the button).
            .padding(.horizontal, 16)
            .frame(height: 44)
            .glassEffect(.regular.interactive(), in: Capsule())
        }
        .accessibilityLabel("Select project")
    }

    private func deviceTag(_ space: Space) -> String {
        let name = model.deviceName(space.deviceId)
        return model.deviceOnline(space.deviceId) ? "@ \(name)" : "@ \(name) · offline"
    }

    private func spaceMenuButton(id: String, title: String, subtitle: String?) -> some View {
        let selected = id.isEmpty ? selectedSpace == nil : spaceFilter == id
        return Button {
            spaceFilter = id
        } label: {
            if selected {
                Label {
                    Text(title)
                    if let subtitle { Text(subtitle) }
                } icon: {
                    Image(systemName: "checkmark")
                }
            } else {
                Text(title)
                if let subtitle { Text(subtitle) }
            }
        }
    }

    /// "+" starts a session in the scoped space; under All it asks which
    /// space first. With no spaces yet it falls through to space creation.
    @ViewBuilder private var newButton: some View {
        if let space = selectedSpace {
            Button {
                path.append(.newSession(spaceId: space.id))
            } label: {
                Image(systemName: "plus")
            }
            .accessibilityLabel("New session")
        } else {
            Button {
                showNewSpace = true
            } label: {
                Image(systemName: "plus")
            }
            .accessibilityLabel("New project")
        }
    }

    private var projectsSection: some View {
        Section {
            if model.spaces.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Your projects, across devices")
                        .font(Theme.sans(16, weight: .medium))
                    Text("Connect Cypher on a Mac or Linux device, then add a project folder with +. This phone is a remote control — no Runtime is needed here.")
                        .font(Theme.sans(13))
                        .foregroundStyle(Theme.textMuted)
                }
                .padding(.vertical, 20)
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
            }
            ForEach(model.spaces) { space in
                Button { path.append(.space(space.id)) } label: {
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            Text(model.deviceName(space.deviceId))
                                .font(Theme.sans(12))
                                .foregroundStyle(Theme.textMuted)
                                .lineLimit(1)
                            Circle()
                                .fill(model.deviceOnline(space.deviceId)
                                      ? Theme.statusCompleted.opacity(0.9)
                                      : Theme.textFaint.opacity(0.4))
                                .frame(width: 6, height: 6)
                                .accessibilityLabel(model.deviceOnline(space.deviceId)
                                                    ? "Device online" : "Device offline")
                        }
                        HStack {
                            Text(space.displayName)
                                .font(Theme.sans(15, weight: .medium))
                                .foregroundStyle(Theme.text)
                                .lineLimit(1)
                            Spacer()
                            Text("\(model.chats(in: space.id).count)")
                                .font(Theme.mono(12)).foregroundStyle(Theme.textFaint)
                            Image(systemName: "chevron.right")
                                .font(.system(size: 11)).foregroundStyle(Theme.textFaint)
                        }
                    }
                    .padding(.vertical, 12)
                    .padding(.horizontal, 12)
                    .frame(minHeight: 64)
                    .contentShape(Rectangle())
                }
                .buttonStyle(PressWashButtonStyle())
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 2, leading: 8, bottom: 2, trailing: 8))
            }
            // Preserve access to history whose project row is gone; never
            // invent a project/device association for an orphaned chat.
            let orphaned = model.overviewChats.filter { chat in
                !model.spaces.contains(where: { $0.id == chat.spaceId })
            }
            if !orphaned.isEmpty {
                Section("Other sessions") {
                    ForEach(orphaned) { chat in
                        Button { path.append(.chat(chat.id)) } label: {
                            ChatRow(chat: chat, showLocation: true)
                        }
                        .listRowBackground(Color.clear)
                    }
                }
            }
        }
    }

    // MARK: Sessions

    private var sessionsSection: some View {
        Section {
            let chats = selectedSpace.map { model.chats(in: $0.id) } ?? model.overviewChats
            if chats.isEmpty {
                Text(model.spaces.isEmpty
                    ? "No projects yet — add a folder from a connected device"
                    : "No sessions yet")
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textFaint)
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
            }
            ForEach(chats) { chat in
                Button {
                    path.append(.chat(chat.id))
                } label: {
                    ChatRow(chat: chat, showLocation: selectedSpace == nil)
                }
                .buttonStyle(PressWashButtonStyle())
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 1, leading: 12, bottom: 1, trailing: 12))
                .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                    Button {
                        // withAnimation, not a value-keyed .animation: the row
                        // leaves THIS section and lands in the archived shelf
                        // — one coordinated List diff, or the hand-off jumps.
                        withAnimation(Motion.resort) {
                            model.archive(chatId: chat.id)
                        }
                    } label: {
                        Label("Archive", systemImage: "archivebox")
                    }
                    .tint(Theme.surfaceRaised)
                }
            }
            .motionAnimation(Motion.resort, value: chats.map(\.id))
        }
    }
}

// MARK: - Rows

/// Two-line session row: project-scoped rows show checkout context above
/// the title. Cross-project history retains its project/device context.
struct ChatRow: View {
    @Environment(AppModel.self) private var model
    let chat: Chat
    var showLocation: Bool

    private var subline: Color { Theme.textMuted.opacity(0.5) }

    var body: some View {
        let indicator = model.indicator(for: chat)
        VStack(alignment: .leading, spacing: 5) {
            // Line 1: context and status (time-ago when idle).
            HStack(spacing: 8) {
                if showLocation {
                    Text(location)
                        .font(Theme.sans(11))
                        .foregroundStyle(subline)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    HStack(spacing: 4) {
                        LineIconView(isWorktree ? .folderWithFiles : .gitBranch,
                                     size: 11, color: subline)
                        Text(checkoutLabel)
                            .font(Theme.sans(11))
                            .foregroundStyle(subline)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                if indicator == .idle {
                    Text(relativeTime(chat.lastMessageAt ?? chat.createdAt))
                        .font(Theme.sans(10, weight: .medium))
                        .foregroundStyle(subline)
                        .fixedSize()
                } else {
                    StatusCorner(indicator: indicator)
                }
            }

            // Line 2: title with its live-run spinner.
            HStack(spacing: 6) {
                if let harness = chat.config?.harness {
                    HarnessBadge(harness: harness, size: 11, neutral: subline)
                }
                Text(chat.displayTitle)
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if indicator == .working {
                    MiniSpinner()
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 10)
        .frame(minHeight: 56)
        .contentShape(RoundedRectangle(cornerRadius: 8))
    }

    private var isWorktree: Bool {
        guard let cwd = chat.cwd, !cwd.isEmpty, let space = model.space(for: chat) else { return false }
        return (cwd as NSString).standardizingPath != (space.path as NSString).standardizingPath
    }

    private var checkoutLabel: String {
        let branch = chat.branch?.trimmingCharacters(in: .whitespacesAndNewlines)
        let branchLabel = branch.flatMap { $0.isEmpty ? nil : $0 }
        if isWorktree, let cwd = chat.cwd {
            let name = ((cwd as NSString).standardizingPath as NSString).lastPathComponent
            return [branchLabel, "Worktree · \(name)"].compactMap { $0 }.joined(separator: " / ")
        }
        return branchLabel ?? "Current checkout"
    }

    /// "space @ device" (the session header's format). The space name (not
    /// the cwd basename) is what the desktop row shows — they differ once a
    /// space has been renamed, or when the session runs in a worktree off to
    /// the side. No offline marker: the dropdown carries device liveness.
    private var location: String {
        let space = model.space(for: chat)?.displayName
            ?? chat.cwd.map { ($0 as NSString).lastPathComponent }
            ?? "?"
        return "\(space) @ \(model.deviceName(chat.deviceId))"
    }
}

func relativeTime(_ ms: Int64) -> String {
    let delta = max(0, nowMs() - ms) / 1000
    if delta < 60 { return "now" }
    if delta < 3600 { return "\(delta / 60)m" }
    if delta < 86_400 { return "\(delta / 3600)h" }
    return "\(delta / 86_400)d"
}

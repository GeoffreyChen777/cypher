// Session screen — transcript + status strip + composer (or question panel
// while input is requested, replacing the composer like the desktop). Reading
// marks the chat seen (the synced LWW marker behind the green dot everywhere).

import SwiftUI

struct SessionView: View {
    @Environment(AppModel.self) private var model
    let chatId: String
    @Binding var path: [Route]

    /// Reserve room around the native title slot for Back and child-session
    /// actions. A bounded title avoids an unbounded custom title-view proposal.
    private static let headerChromeInset: CGFloat = 170

    /// The view's own width, the only reliable basis for capping the principal
    /// toolbar item (its container proposes an unbounded width).
    @State private var viewWidth: CGFloat = 0
    @State private var viewHeight: CGFloat = 0

    /// Shared with TranscriptView; owned here so the composer inset (which
    /// this view composes) can report its global top edge — the measured
    /// bottom boundary TranscriptView's correctPin re-pins against.
    @State private var scroll = ScrollState()
    @State private var controlError: String?
    @State private var commentDrafts = CommentDrafts()
    @State private var catalog = RemotePiCatalog()
    @State private var connectionRetry = 0
    /// The progress card, opened by tapping the working strip's bar.
    @State private var showingProgress = false
    @State private var workspaceDestination: WorkspaceDestination?
    @State private var actions = SessionActions()
    /// The entry a fork / restart is in flight for (its strip button spins).
    @State private var forkingEntry: String?
    @State private var rewindingEntry: String?
    /// The message whose restart waits for the user's confirmation.
    @State private var rewindCandidate: TranscriptEntryContext?
    /// A passing outcome under the status strip ("Conversation restarted…").
    @State private var notice: String?
    @State private var noticeClear: Task<Void, Never>?
    @State private var sideChat: SideChatStore?
    @State private var promotedSideChat: String?
    /// Regular width (iPad) docks Files, Changes and the side chat in an
    /// inspector beside the transcript; compact keeps them in sheets.
    @Environment(\.horizontalSizeClass) private var sizeClass
    /// The iPad split view's columns, for making the inspector room.
    @Environment(\.splitColumns) private var splitColumns
    /// Where the split layout shows this session's inspector.
    @Environment(\.detailInspector) private var detailInspector
    @State private var inspectorShown = false
    @State private var inspectorPane: SessionInspectorPane = .workspace
    /// The inspector hid the sidebar to make room; closing it brings it back.
    @State private var inspectorHidSidebar = false

    /// Narrower than this beside the inspector, the chat takes the
    /// sidebar's room instead.
    static let minimumChatWidth: CGFloat = 480

    private var chat: Chat? { model.chat(id: chatId) }
    @Environment(\.scenePhase) private var scenePhase
    /// On screen, frontmost and holding activity newer than the synced seen
    /// marker — a run finishing (or asking) while you watch it.
    private var watchingUnseen: Bool {
        chat?.unseen == true && scenePhase == .active && path.last == .chat(chatId)
    }

    // `body` is split into layers (chrome → sheets → lifecycle) so each
    // type-checks on its own: as one expression it took ~0.9s here, close to
    // what timed out HomeView on CI's older Xcode.
    var body: some View {
        withSheets
            .onChange(of: chatId) { _, _ in workspaceDestination = nil }
            // Reading it here reads it everywhere: the marker syncs, the desktop
            // Dock drops it and the Worker clears the other phones' badges. The
            // desktop marks its focused session live the same way.
            .onChange(of: watchingUnseen) { _, watching in
                if watching { model.markSeen(chatId: chatId) }
            }
            .onChange(of: chat?.cwd) { _, _ in workspaceDestination = nil }
            .onChange(of: chat?.deviceId) { _, _ in workspaceDestination = nil }
            .onChange(of: path) { _, routes in
                if routes.last != .chat(chatId) {
                    commentDrafts.reset()
                }
            }
            .onChange(of: model.workspace.map { ObjectIdentifier($0) }) { _, _ in
                workspaceDestination = nil
                // Account/workspace replacement invalidates an in-flight send's
                // annotation snapshot even if the navigation path hasn't changed.
                commentDrafts.reset()
                commentDrafts.bind(to: chatId)
            }
            .onChange(of: workspaceDestination) { _, destination in
                if docked, destination != nil { showInspector(.workspace) }
            }
            .onChange(of: inspectorKey, initial: true) { _, _ in publishInspector() }
            .onAppear {
                model.sessionOnScreen(chatId)
                publishInspector()
            }
            .onDisappear {
                if path.last != .chat(chatId) { commentDrafts.reset() }
                // The side chat belongs to this session's screen: it ends
                // when the session leaves the path, not under a cover.
                if !path.contains(.chat(chatId)) { endSideChat() }
                detailInspector?.withdraw(owner: chatId)
                model.sessionOffScreen(chatId)
                model.markSeen(chatId: chatId)
                model.releaseSessionStore(chatId: chatId)
            }
    }

    private var chrome: some View {
        Group {
            if let chat, let store = model.sessionStore(for: chat) {
                content(chat: chat, store: store)
                    .onGeometryChange(for: CGSize.self) {
                        $0.size
                    } action: {
                        viewWidth = $0.width
                        viewHeight = $0.height
                    }
            } else {
                VStack(spacing: 12) {
                    CypherPulse()
                    Text("Opening session…")
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textFaint)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Theme.bg)
            }
        }
        .navigationTitle(chat?.displayTitle ?? "Session")  // feeds the back menu
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(.hidden, for: .navigationBar)
        .toolbar {
            if let chat { sessionToolbar(chat) }
        }
    }

    @ToolbarContentBuilder
    private func sessionToolbar(_ chat: Chat) -> some ToolbarContent {
        // Static, left-aligned session header — model/effort changes
        // moved into the composer's picker chips.
        // Static text belongs in the native title slot. Putting a
        // wide title in topBarLeading makes it a bar-button item that
        // can morph with Back's Liquid Glass background during a pop.
        ToolbarItem(placement: .principal) {
            VStack(alignment: .leading, spacing: 1) {
                Text(chat.displayTitle)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if let subtitle {
                    Text(subtitle)
                        .font(Theme.sans(10.5))
                        .foregroundStyle(Theme.textMuted.opacity(0.6))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .clearsHiddenSidebarButton(splitColumns)
            // Keep the title bounded without creating a leading
            // bar-button container or changing native Back behavior.
            .frame(
                width: max(140, viewWidth - Self.headerChromeInset),
                alignment: .leading)
        }
        // Bare text on the bar, not a glass capsule.
        .sharedBackgroundVisibility(.hidden)
        if docked {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    toggleInspector()
                } label: {
                    Image(systemName: "sidebar.trailing")
                }
                .keyboardShortcut("i", modifiers: [.command, .option])
                .accessibilityLabel(inspectorShown ? "Hide Inspector" : "Show Inspector")
                .accessibilityIdentifier("inspector-toggle")
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button("Files", systemImage: "folder") { workspaceDestination = .files }
                Button("Changes", systemImage: "plus.forwardslash.minus") { workspaceDestination = .changes }
                OpenInNewWindowItem(chatId: chat.id)
                if !chat.isChild {
                    Divider()
                    Button("Rename…", systemImage: "pencil") { actions.rename(chat) }
                    if chat.archived {
                        Button("Unarchive", systemImage: "tray.and.arrow.up") {
                            model.unarchive(chatId: chat.id)
                        }
                    } else {
                        Button("Archive", systemImage: "archivebox") {
                            model.archive(chatId: chat.id)
                            leave()
                        }
                    }
                    Button("Delete…", systemImage: "trash", role: .destructive) {
                        actions.delete(chat)
                    }
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .accessibilityLabel("Session menu")
            .accessibilityIdentifier("workspace-browser")
        }
        if let relation = chat.child,
            let parent = model.chat(id: relation.parentChatId),
            parent.id != chat.id, parent.deviceId == chat.deviceId
        {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    path = SessionNavigation.opening(parent.id, in: path)
                } label: {
                    Image(systemName: "arrow.turn.up.left")
                }
                .accessibilityLabel("Return to parent session")
            }
        }
    }

    private var withSheets: some View {
        chrome
            .onAppear {
                commentDrafts.bind(to: chatId)
                model.markSeen(chatId: chatId)
                if model.demo != nil, let destination = WorkspaceDestination(rawValue: model.launchSheet ?? "") {
                    model.launchSheet = nil
                    workspaceDestination = destination
                }
                // `-sheet round[:path]` opens the session's last recorded
                // round, `-sheet reference:path:12` a file as a chip would.
                if model.demo != nil, let sheet = model.launchSheet, let chat {
                    if sheet == "round" || sheet.hasPrefix("round:"),
                        let changes = model.sessionStore(for: chat)?.entries.last(where: { $0.turnChanges != nil })?
                            .turnChanges
                    {
                        model.launchSheet = nil
                        workspaceDestination = .round(
                            changes, focus: sheet.hasPrefix("round:") ? String(sheet.dropFirst(6)) : nil)
                    } else if sheet.hasPrefix("reference:"), let ref = FileReference.detect(String(sheet.dropFirst(10)))
                    {
                        model.launchSheet = nil
                        workspaceDestination = .reference(WorkspaceReference(path: ref.path, lines: ref.lines))
                    }
                }
                if model.demo != nil, model.launchSheet == "comment" || model.launchSheet == "comments" {
                    let showList = model.launchSheet == "comments"
                    model.launchSheet = nil
                    commentDrafts.begin(quote: "The transcript stays glued to the bottom until you scroll up.")
                    if showList, let source = commentDrafts.editor {
                        _ = commentDrafts.save(
                            source: source, quote: source.text,
                            comment: "Explain what happens when the keyboard opens.")
                        commentDrafts.showList()
                    }
                }
            }
            .sessionActionPrompts(actions) { _ in leave() }
            .environment(\.transcriptSelectionActions, chat.map(selectionActions))
            .environment(\.transcriptMessageActions, chat.map(messageActions))
            // File chips, file links and a round's changed files open here.
            .environment(\.openWorkspace, chat.map { _ in { workspaceDestination = $0 } })
            .environment(\.fileReferenceContext, chat.map(referenceContext))
            .environment(\.fileReferenceSnippet, chat.map(referenceSnippet))
            .confirmationDialog(
                "Restart from here?",
                isPresented: Binding(
                    get: { rewindCandidate != nil },
                    set: { if !$0 { rewindCandidate = nil } }),
                titleVisibility: .visible,
                presenting: rewindCandidate
            ) { entry in
                Button("Restart from Here", role: .destructive) {
                    if let chat { rewind(chat, at: entry) }
                }
            } message: { entry in
                Text(rewindWarning(for: entry))
            }
            .sheet(
                item: Binding(
                    get: { docked ? nil : sideChat },
                    set: { if !docked, $0 == nil { endSideChat() } }),
                onDismiss: {
                    // Navigate once the sheet is gone: a path change during its
                    // dismissal can be dropped.
                    if let chatId = promotedSideChat {
                        promotedSideChat = nil
                        path = SessionNavigation.opening(chatId, in: path)
                    }
                }
            ) { store in
                SideChatSheet(store: store) { chatId in
                    promotedSideChat = chatId
                }
            }
            .environment(\.commentDrafts, chat?.config?.harness == "pi" ? commentDrafts : nil)
            .sheet(
                isPresented: Binding(
                    get: { commentDrafts.presented && commentDrafts.presenter == .session },
                    set: { commentDrafts.presented = $0 })
            ) {
                CommentsPanel(drafts: commentDrafts)
            }
            .sheet(
                item: Binding(
                    get: { docked ? nil : workspaceDestination },
                    set: { if !docked { workspaceDestination = $0 } })
            ) { destination in
                if let chat {
                    WorkspaceBrowserView(
                        model: model, chat: chat, destination: destination,
                        // Selections in a diff or a file comment like the
                        // transcript's, on a Pi session.
                        drafts: chat.config?.harness == "pi" ? commentDrafts : nil)
                }
            }
    }

    // MARK: Inspector (iPad)

    private var docked: Bool { sizeClass == .regular }

    /// What the published panel depends on: republished when it changes.
    private var inspectorKey: String {
        [
            "\(docked)", "\(inspectorShown)", "\(inspectorPane)",
            sideChat.map { "\(ObjectIdentifier($0))" } ?? "", workspaceDestination?.id ?? "",
            chat?.deviceId ?? "", chat?.cwd ?? "", chat?.config?.harness ?? "",
        ].joined(separator: "|")
    }

    /// Hand the panel to the split layout's inspector, or take it down.
    private func publishInspector() {
        guard let detailInspector else { return }
        guard docked, inspectorShown, let chat else {
            detailInspector.withdraw(owner: chatId)
            return
        }
        let panel = SessionInspector(
            chat: chat, destination: $workspaceDestination, pane: $inspectorPane,
            sideChat: sideChat,
            drafts: chat.config?.harness == "pi" ? commentDrafts : nil,
            close: hideInspector, closeSideChat: endSideChat
        ) { chatId in
            endSideChat()
            path = SessionNavigation.opening(chatId, in: path)
        }
        detailInspector.present(owner: chatId, content: AnyView(panel), onHide: hideInspector)
    }

    private func showInspector(_ pane: SessionInspectorPane) {
        inspectorPane = pane
        guard !inspectorShown else { return }
        // Measured before it opens: the chat's whole width (unknown, 0,
        // before the first layout — leave the sidebar then).
        if let splitColumns, splitColumns.wrappedValue != .detailOnly, viewWidth > 0,
            viewWidth - SessionInspector.idealWidth < Self.minimumChatWidth
        {
            splitColumns.wrappedValue = .detailOnly
            inspectorHidSidebar = true
        }
        inspectorShown = true
    }

    private func hideInspector() {
        inspectorShown = false
        workspaceDestination = nil
        if inspectorHidSidebar {
            inspectorHidSidebar = false
            splitColumns?.wrappedValue = .all
        }
    }

    private func toggleInspector() {
        if inspectorShown {
            hideInspector()
        } else if sideChat != nil, inspectorPane == .sideChat {
            showInspector(.sideChat)
        } else {
            if workspaceDestination == nil { workspaceDestination = .changes }
            showInspector(.workspace)
        }
    }

    /// Start a side chat on a selection, replacing (and ending) any other.
    private func startSideChat(_ store: SideChatStore?) {
        endSideChat()
        sideChat = store
        if docked, store != nil { showInspector(.sideChat) }
    }

    /// End the side chat: closing discards it on the host (a no-op once
    /// it's a chat). The inspector falls back to Files / Changes, or hides
    /// when the side chat was all it showed.
    private func endSideChat() {
        guard let store = sideChat else { return }
        sideChat = nil
        store.close()
        if inspectorPane == .sideChat {
            inspectorPane = .workspace
            if inspectorShown, workspaceDestination == nil { hideInspector() }
        }
    }

    /// The paths a reply cites, for reading a bare name in it.
    private func referenceContext(_ chat: Chat) -> (String) -> [String] {
        { [model] entryId in
            TranscriptFileLinks.citedPaths(in: model.sessionStore(for: chat)?.entries.first { $0.id == entryId })
        }
    }

    /// A reference's cited lines, read from this chat's checkout.
    private func referenceSnippet(_ chat: Chat) -> (WorkspaceReference) async throws -> FileReferenceSnippet {
        { [model] reference in
            try await WorkspaceBrowserSession(model: model, chat: chat).snippet(reference)
        }
    }

    /// Fork and Side Chat on a selection — only what this session can do
    /// right now (transcript.rs fork_gate: Pi, a root chat, not mid-run,
    /// host reachable; side chats need a reachable Pi host too).
    private func selectionActions(for chat: Chat) -> TranscriptSelectionActions {
        let reachable = model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId))
        guard chat.config?.harness == "pi", reachable else { return TranscriptSelectionActions() }
        let live = liveStatus(chat: chat)
        let canFork = !chat.isChild && live != .working && live != .awaitingInput && forkingEntry == nil
        return TranscriptSelectionActions(
            fork: canFork ? { entry in fork(chat, at: entry) } : nil,
            sideChat: { entry, quote in
                startSideChat(model.sideChat(parent: chat, quote: quote, anchorEntryId: entry.entryId))
            })
    }

    /// The message strips' fork and restart, gated like the desktop's
    /// (transcript.rs `fork_gate` / `rewind_gate`, same order and wording).
    private func messageActions(for chat: Chat) -> TranscriptMessageActions {
        let live = liveStatus(chat: chat)
        func blocked(_ forked: String, _ finish: String) -> String? {
            if model.demo == nil, !model.connected { return "Not connected." }
            if chat.isChild { return "Subagent chats can't be \(forked)." }
            if chat.config?.harness != "pi" { return "Only Pi chats can be \(forked)." }
            if live == .working || live == .awaitingInput {
                return "Wait for the chat to finish before \(finish)."
            }
            if model.demo == nil, !model.deviceOnline(chat.deviceId) {
                return "The device hosting this chat is offline."
            }
            if forkingEntry != nil || rewindingEntry != nil {
                return "Wait for the current fork or restart to finish."
            }
            return nil
        }
        return TranscriptMessageActions(
            forkBlocked: blocked("forked", "forking"),
            rewindBlocked: blocked("restarted from a message", "restarting it"),
            forking: forkingEntry,
            rewinding: rewindingEntry,
            fork: { entry in fork(chat, at: entry) },
            rewind: { entry in rewindCandidate = entry })
    }

    /// What the confirming tap deletes — named, since it's permanent
    /// (gates.rs `rewind_tooltip`, armed).
    private func rewindWarning(for entry: TranscriptEntryContext) -> String {
        let entries = chat.flatMap { model.sessionStore(for: $0)?.entries } ?? []
        let later = entries.firstIndex { $0.id == entry.entryId }.map { entries.count - $0 - 1 } ?? 0
        let count = later == 1 ? "1 message" : "\(later) messages"
        return entry.role == .user
            ? "Deletes this message and \(count) after it. Its text comes back to the composer."
            : "Deletes \(count) after this response."
    }

    private func rewind(_ chat: Chat, at context: TranscriptEntryContext) {
        guard let anchor = model.sessionStore(for: chat)?.entries.first(where: { $0.id == context.entryId })
        else { return }
        rewindingEntry = anchor.id
        controlError = nil
        Task { @MainActor in
            let outcome = await model.rewindSession(chat, anchor: anchor)
            rewindingEntry = nil
            switch outcome {
            case .rewound(let message):
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                show(notice: message)
            case .failed(let message):
                controlError = message
            }
        }
    }

    private func show(notice message: String) {
        notice = message
        noticeClear?.cancel()
        noticeClear = Task { @MainActor in
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            notice = nil
        }
    }

    private func fork(_ chat: Chat, at context: TranscriptEntryContext) {
        guard let anchor = model.sessionStore(for: chat)?.entries.first(where: { $0.id == context.entryId })
        else { return }
        forkingEntry = anchor.id
        controlError = nil
        Task { @MainActor in
            let outcome = await model.forkSession(chat, anchor: anchor)
            forkingEntry = nil
            switch outcome {
            case .created(let forkId):
                UINotificationFeedbackGenerator().notificationOccurred(.success)
                // Only follow it if the reader is still here (shell.rs).
                if path.last == .chat(chat.id) {
                    path = SessionNavigation.opening(forkId, in: path)
                }
            case .failed(let message):
                controlError = message
            }
        }
    }

    /// Back out of this session (it was archived or deleted from its menu).
    private func leave() {
        if let ix = path.lastIndex(of: .chat(chatId)) {
            path = Array(path.prefix(ix))
        }
    }

    /// "space @ device" — short, like the home dropdown's rows. The space
    /// NAME (not the cwd basename: they differ for renamed spaces and
    /// worktree sessions), falling back to the cwd when the space row is gone.
    private var subtitle: String? {
        guard let chat else { return nil }
        if chat.isScratch { return "Quick chat @ \(model.deviceName(chat.deviceId))" }
        let space =
            model.space(for: chat)?.displayName
            ?? chat.cwd.map { ($0 as NSString).lastPathComponent }
            ?? "?"
        return "\(space) @ \(model.deviceName(chat.deviceId))"
    }

    private func content(chat: Chat, store: SessionStore) -> some View {
        let status = liveStatus(chat: chat)
        // The composer is a bottom SAFE-AREA BAR on the transcript, not a
        // VStack sibling: the scroll view then spans the full height down to
        // the keyboard, which is what lets UIKit's interactive
        // keyboard-dismiss (scrollDismissesKeyboard(.interactively) in
        // TranscriptView) track a downward drag — with a sibling composer the
        // scroll view ends above the keyboard and the pan never engages it.
        return TranscriptView(store: store, chatId: chat.id, scroll: scroll)
            // The registry row says this session HAS messages; an empty
            // transcript is therefore still hydrating (no disk snapshot yet,
            // checkpoint in flight) — show the pulse, not a black void. This
            // was "the session is blank when I open it" on a phone that had
            // never cached the doc.
            .overlay {
                // The fade is scoped to the overlay: on the transcript it also
                // animated the scroll view's jump to the arriving rows (see
                // TranscriptView's reveal).
                ZStack {
                    if store.entries.isEmpty, store.pendingSends.isEmpty,
                        chat.lastMessageAt != nil
                    {
                        TranscriptSkeleton()
                            .background(Theme.bg)
                    }
                }
                .motionAnimation(Motion.fadeQuick, value: store.entries.isEmpty)
            }
            // The keyboard's own transition bounds the transcript's no-correct
            // window; didShow/didHide land the single measured glide.
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { _ in
                // Re-arm the pin while the numbers are still honest (the
                // keyboard inset hasn't applied yet): a few points of
                // scroll-up breaks the pin though the feed still reads as
                // at-bottom, and the didShow correction only lifts a PINNED
                // feed — that near-bottom feed was left parked behind the
                // keyboard. Same 70pt band as the re-engage rule.
                if !scroll.userScrolling,
                    scroll.distanceFromBottom <= TranscriptView.stickThreshold
                {
                    scroll.pinned = true
                }
                scroll.keyboardTransitioning = true
            }
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardDidShowNotification)) { _ in
                scroll.keyboardTransitioning = false
                scroll.requestCorrection()
            }
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { _ in
                scroll.keyboardTransitioning = true
            }
            .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardDidHideNotification)) { _ in
                scroll.keyboardTransitioning = false
                scroll.requestCorrection()
            }
            .safeAreaBar(edge: .bottom, spacing: 0) {
                VStack(spacing: 0) {
                    HStack(spacing: 8) {
                        statusStrip(chat: chat, store: store, status: status)
                            // Only the progress bar's strip takes a tap (its card).
                            .allowsHitTesting(status == .working && sessionRow(chat: chat)?.progress != nil)
                            .lineLimit(1)
                        SubagentsAccessory(
                            parent: chat, store: store,
                            maxWidth: max(130, viewWidth * 0.5)
                        ) { childId in
                            path = SessionNavigation.opening(childId, in: path)
                        }
                        .padding(.trailing, 16)
                    }
                    .frame(minHeight: 44)
                    if forkingEntry != nil || rewindingEntry != nil {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.mini)
                            Text(forkingEntry != nil ? "Forking…" : "Restarting…")
                                .font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                        }
                    } else if let controlError {
                        Text(controlError).font(Theme.sans(12)).foregroundStyle(Theme.danger)
                            .padding(.horizontal, 20)
                    } else if let notice {
                        Text(notice).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                            .padding(.horizontal, 20)
                    }
                    Group {
                        if let request = store.openInputRequest, chat.config?.harness == "pi" {
                            QuestionPanel(
                                requestId: request.requestId, questions: request.questions,
                                maximumHeight: min(560, max(180, viewHeight * 0.72)),
                                canRespond: canControl(chat),
                                notice: store.inputAnswerFailure,
                                stop: {
                                    guard canControl(chat) else { return }
                                    controlError = store.sendInterrupt() ? nil : "Couldn't queue Stop. Please retry."
                                }
                            ) { requestId, answers in
                                guard canControl(chat) else { return }
                                controlError =
                                    store.respondInput(requestId: requestId, answers: answers)
                                    ? nil : "Couldn't queue your answer. Please retry."
                            }
                            .id(request.requestId)
                        } else {
                            // The `/` menu gets what's left above the composer
                            // and its status row (~175pt), so it never runs
                            // under the navigation bar with the keyboard up.
                            ComposerView(
                                store: store, chat: chat, runLive: status == .working,
                                catalog: catalog, connectionRetry: connectionRetry,
                                slashMenuMaxHeight: min(
                                    SlashMenuView.defaultMaxHeight,
                                    max(160, viewHeight - 175)))
                        }
                    }
                    .padding(.bottom, 8)
                }
                // The transcript's reading column, so a wide window (iPad)
                // keeps the composer under the messages.
                .frame(maxWidth: TranscriptView.maxContentWidth)
                .frame(maxWidth: .infinity)
                // Report the inset's global top edge — the visual line the
                // transcript's bottom pad should meet while pinned. Global
                // frames stay honest when the keyboard's inset math doesn't
                // (see TranscriptView.correctPin).
                .onGeometryChange(for: CGFloat.self) {
                    $0.frame(in: .global).minY
                } action: { [scroll] new in
                    scroll.insetTopGlobalY = new
                    scroll.insetTopChangedAt = Date().timeIntervalSinceReferenceDate
                }
                // safeAreaBar gives this custom control area the system's
                // scroll-edge treatment, like the navigation title above.
                // No additional material slab or tinted overlay.
            }
            .background(Theme.bg.ignoresSafeArea())
            .motionAnimation(Motion.fadeQuick, value: store.openInputRequest?.requestId)
    }

    private func liveStatus(chat: Chat) -> SessionStatus? {
        if let demo = model.demo {
            return effectiveStatus(demo.sessions[chat.id], now: nowMs())
        }
        return effectiveStatus(model.workspace?.sessions[chat.id], now: nowMs())
    }

    private func canControl(_ chat: Chat) -> Bool {
        chat.config?.harness == "pi"
            && (model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId)))
    }

    /// Reserved 24pt status strip (shell.rs render_status_strip) — Working
    /// shows the sunrise spinner + rotating flavour word + elapsed; a context
    /// recovery under way (render.rs `render_recovery_trailer`) shows the
    /// spinner + what it is doing; Errored shows "Run failed"; the strip
    /// always reserves its height so the composer never shifts. A long turn's
    /// narrated progress (render.rs's trailer bar) fills the rest of the row
    /// with its bar and percentage; a tap opens its stages.
    private func statusStrip(chat: Chat, store: SessionStore, status: SessionStatus?) -> some View {
        let transportReady =
            model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId) && store.connected)
        let connection = SessionConnectionPhase.resolve(
            transportReady: transportReady,
            needsCatalog: chat.config?.harness == "pi" && store.openInputRequest == nil,
            catalogMatches: catalog.deviceId == chat.deviceId,
            catalogLoading: catalog.loading,
            catalogError: catalog.error,
            modelAvailable: catalog.models(for: chat.deviceId).contains { $0.id == chat.config?.model })
        return TimelineView(.periodic(from: .now, by: 1)) { _ in
            let now = nowMs()
            // The working strip wins, as the desktop's working trailer does;
            // a recovery an engine left behind stops showing once stale.
            let recovery =
                status == .working
                ? nil : sessionRow(chat: chat)?.recovery.flatMap { $0.isLive(now: now) ? $0 : nil }
            // Every state but a quiet, connected session draws something here.
            let showing = connection != .ready || status == .working || status == .errored || recovery != nil
            let progress =
                connection == .ready && recovery == nil && status == .working ? sessionRow(chat: chat)?.progress : nil
            HStack(spacing: 6) {
                if connection != .ready {
                    SessionConnectionIndicator(phase: connection, retryRevision: connectionRetry) {
                        connectionRetry += 1
                        model.foregrounded()
                    }
                } else if let recovery {
                    let text = recovery.trailerText(now: now)
                    WorkingSpinner()
                    Text("\(text.word)…")
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .layoutPriority(1)
                    Text(text.tail)
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.textFaint)
                        .monospacedDigit()
                        .truncationMode(.tail)
                } else {
                    switch status {
                    case .working:
                        WorkingSpinner()
                        let startedAt = sessionStartedAt(chat: chat)
                        let elapsed = (now - startedAt) / 1000
                        Text("\(Motion.flavourWord(seed: Motion.flavourSeed(chat.id), elapsedSecs: elapsed))…")
                            .font(Theme.sans(12))
                            .foregroundStyle(Theme.textMuted)
                            .layoutPriority(1)
                        Text(Motion.formatElapsed(elapsed))
                            .font(Theme.sans(11))
                            .foregroundStyle(Theme.textFaint)
                            .monospacedDigit()
                            .fixedSize()
                        if let progress {
                            RunProgressBar(progress: progress)
                                .frame(minWidth: 40, maxWidth: .infinity)
                                .padding(.leading, 2)
                            Text("\(progress.shownPercent)%")
                                .font(Theme.sans(11))
                                .foregroundStyle(Theme.textFaint)
                                .monospacedDigit()
                                .fixedSize()
                                .accessibilityHidden(true)  // the bar's value reads it
                        }
                    case .errored:
                        Text("Run failed")
                            .font(Theme.sans(11))
                            .foregroundStyle(Theme.danger)
                    default:
                        EmptyView()
                    }
                }
            }
            .modifier(StatusCapsule(active: showing))
            .contentShape(Capsule())
            .onTapGesture { if progress != nil { showingProgress = true } }
            .accessibilityElement(children: progress != nil ? .combine : .contain)
            .accessibilityAddTraits(progress != nil ? .isButton : [])
            .accessibilityHint(progress != nil ? "Shows the turn's steps" : "")
            .accessibilityIdentifier(progress != nil ? "run-progress" : "")
            .popover(
                isPresented: Binding(
                    get: { showingProgress && progress != nil }, set: { showingProgress = $0 })
            ) {
                if let progress { RunProgressCard(progress: progress) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.leading, 16)  // the capsule on the composer's edge, its text on the composer's
        }
    }

    private func sessionRow(chat: Chat) -> SessionRow? {
        model.demo?.sessions[chat.id] ?? model.workspace?.sessions[chat.id]
    }

    private func sessionStartedAt(chat: Chat) -> Int64 {
        let row = sessionRow(chat: chat)
        return row?.startedAt ?? row?.updatedAt ?? nowMs()
    }
}

/// A status item over the transcript (the run status, the subagents count):
/// its own glass capsule, the collapsed composer pill's material — plain
/// glass, a faint wash and hairline — so the row reads as one family with
/// the composer under it. Inactive draws nothing.
struct StatusCapsule: ViewModifier {
    var active = true
    /// A button's capsule: the glass itself answers the press.
    var interactive = false

    func body(content: Content) -> some View {
        if active {
            content
                .padding(.horizontal, 10)
                .frame(height: 28)
                .background(whiteAlpha(0.04), in: Capsule())
                .glassEffect(interactive ? .regular.interactive() : .regular, in: Capsule())
                .overlay(Capsule().strokeBorder(whiteAlpha(0.05), lineWidth: 1).allowsHitTesting(false))
        } else {
            content
                .frame(height: 28)
        }
    }
}

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
    @State private var workspaceDestination: WorkspaceDestination?
    @State private var actions = SessionActions()
    @State private var forking = false
    @State private var sideChat: SideChatStore?
    @State private var promotedSideChat: String?


    private var chat: Chat? { model.chat(id: chatId) }

    private var chatSpace: Space? {
        guard let spaceId = chat?.spaceId else { return nil }
        return model.spaces.first { $0.id == spaceId }
    }

    // `body` is split into layers (chrome → sheets → lifecycle) so each
    // type-checks on its own: as one expression it took ~0.9s here, close to
    // what timed out HomeView on CI's older Xcode.
    var body: some View {
        withSheets
            .onChange(of: chatId) { _, _ in workspaceDestination = nil }
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
            .onDisappear {
                if path.last != .chat(chatId) { commentDrafts.reset() }
                model.markSeen(chatId: chatId)
                model.releaseSessionStore(chatId: chatId)
            }
    }

    private var chrome: some View {
        Group {
            if let chat, let store = model.sessionStore(for: chat) {
                content(chat: chat, store: store)
                    .onGeometryChange(for: CGSize.self) { $0.size } action: {
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
            // Keep the title bounded without creating a leading
            // bar-button container or changing native Back behavior.
            .frame(width: max(140, viewWidth - Self.headerChromeInset),
                   alignment: .leading)
        }
        // Bare text on the bar, not a glass capsule.
        .sharedBackgroundVisibility(.hidden)
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button("Files", systemImage: "folder") { workspaceDestination = .files }
                Button("Changes", systemImage: "plus.forwardslash.minus") { workspaceDestination = .changes }
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
           parent.id != chat.id, parent.deviceId == chat.deviceId {
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
                if model.demo != nil, model.launchSheet == "comment" || model.launchSheet == "comments" {
                    let showList = model.launchSheet == "comments"
                    model.launchSheet = nil
                    commentDrafts.begin(quote: "The transcript stays glued to the bottom until you scroll up.")
                    if showList, let source = commentDrafts.editor {
                        _ = commentDrafts.save(source: source, quote: source.text,
                                              comment: "Explain what happens when the keyboard opens.")
                        commentDrafts.showList()
                    }
                }
            }
            .sessionActionPrompts(actions) { _ in leave() }
            .environment(\.transcriptSelectionActions, chat.map(selectionActions))
            .sheet(item: $sideChat, onDismiss: {
                // Navigate once the sheet is gone: a path change during its
                // dismissal can be dropped.
                if let chatId = promotedSideChat {
                    promotedSideChat = nil
                    path = SessionNavigation.opening(chatId, in: path)
                }
            }) { store in
                SideChatSheet(store: store) { chatId in
                    promotedSideChat = chatId
                }
                // Closing discards it on the host; a no-op once it's a chat.
                .onDisappear { store.close() }
            }
            .environment(\.commentDrafts, chat?.config?.harness == "pi" ? commentDrafts : nil)
            .sheet(isPresented: $commentDrafts.presented) {
                CommentsPanel(drafts: commentDrafts)
            }
            .sheet(item: $workspaceDestination) { destination in
                if let chat { WorkspaceBrowserView(model: model, chat: chat, destination: destination) }
            }
    }

    /// Fork and Side Chat on a selection — only what this session can do
    /// right now (transcript.rs fork_gate: Pi, a root chat, not mid-run,
    /// host reachable; side chats need a reachable Pi host too).
    private func selectionActions(for chat: Chat) -> TranscriptSelectionActions {
        let reachable = model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId))
        guard chat.config?.harness == "pi", reachable else { return TranscriptSelectionActions() }
        let live = liveStatus(chat: chat)
        let canFork = !chat.isChild && live != .working && live != .awaitingInput && !forking
        return TranscriptSelectionActions(
            fork: canFork ? { entry in fork(chat, at: entry) } : nil,
            sideChat: { entry, quote in
                sideChat = model.sideChat(parent: chat, quote: quote, anchorEntryId: entry.entryId)
            })
    }

    private func fork(_ chat: Chat, at context: TranscriptEntryContext) {
        guard let anchor = model.sessionStore(for: chat)?.entries.first(where: { $0.id == context.entryId })
        else { return }
        forking = true
        controlError = nil
        Task { @MainActor in
            let outcome = await model.forkSession(chat, anchor: anchor)
            forking = false
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
        let space = model.space(for: chat)?.displayName
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
                       chat.lastMessageAt != nil {
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
                   scroll.distanceFromBottom <= TranscriptView.stickThreshold {
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
                            .allowsHitTesting(false)
                            .lineLimit(1)
                        SubagentsAccessory(parent: chat, store: store,
                                           maxWidth: max(130, viewWidth * 0.5)) { childId in
                            path = SessionNavigation.opening(childId, in: path)
                        }
                        .padding(.trailing, 20)
                    }
                    .frame(minHeight: 44)
                    if forking {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.mini)
                            Text("Forking…").font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                        }
                    } else if let controlError {
                        Text(controlError).font(Theme.sans(12)).foregroundStyle(Theme.danger)
                            .padding(.horizontal, 20)
                    }
                    Group {
                        if let request = store.openInputRequest, chat.config?.harness == "pi" {
                            QuestionPanel(requestId: request.requestId, questions: request.questions,
                                          maximumHeight: min(560, max(180, viewHeight * 0.72)),
                                          canRespond: canControl(chat), stop: {
                                guard canControl(chat) else { return }
                                controlError = store.sendInterrupt() ? nil : "Couldn't queue Stop. Please retry."
                            }) { requestId, answers in
                                guard canControl(chat) else { return }
                                controlError = store.respondInput(requestId: requestId, answers: answers)
                                    ? nil : "Couldn't queue your answer. Please retry."
                            }
                            .id(request.requestId)
                        } else {
                            ComposerView(store: store, chat: chat, runLive: status == .working,
                                         catalog: catalog, connectionRetry: connectionRetry)
                        }
                    }
                    .padding(.bottom, 8)
                }
                // Report the inset's global top edge — the visual line the
                // transcript's bottom pad should meet while pinned. Global
                // frames stay honest when the keyboard's inset math doesn't
                // (see TranscriptView.correctPin).
                .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).minY } action: { [scroll] new in
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
    /// shows the sunrise spinner + rotating flavour word + elapsed; Errored
    /// shows "Run failed"; the strip always reserves its height so the
    /// composer never shifts.
    private func statusStrip(chat: Chat, store: SessionStore, status: SessionStatus?) -> some View {
        let transportReady = model.demo != nil ||
            (model.connected && model.deviceOnline(chat.deviceId) && store.connected)
        let connection = SessionConnectionPhase.resolve(
            transportReady: transportReady,
            needsCatalog: chat.config?.harness == "pi" && store.openInputRequest == nil,
            catalogMatches: catalog.deviceId == chat.deviceId,
            catalogLoading: catalog.loading,
            catalogError: catalog.error,
            modelAvailable: catalog.models(for: chat.deviceId).contains { $0.id == chat.config?.model })
        return TimelineView(.periodic(from: .now, by: 1)) { _ in
            HStack(spacing: 6) {
                if connection != .ready {
                    SessionConnectionIndicator(phase: connection, retryRevision: connectionRetry) {
                        connectionRetry += 1
                        model.foregrounded()
                    }
                } else {
                switch status {
                case .working:
                    WorkingSpinner()
                    let startedAt = sessionStartedAt(chat: chat)
                    let elapsed = (nowMs() - startedAt) / 1000
                    Text("\(Motion.flavourWord(seed: Motion.flavourSeed(chat.id), elapsedSecs: elapsed))…")
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                    Text(Motion.formatElapsed(elapsed))
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.textFaint)
                        .monospacedDigit()
                case .errored:
                    Text("Run failed")
                        .font(Theme.sans(11))
                        .foregroundStyle(Theme.danger)
                default:
                    EmptyView()
                }
                }
            }
            .frame(height: 24)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.leading, 26)  // aligns with the composer's text start
        }
    }

    private func sessionStartedAt(chat: Chat) -> Int64 {
        let row = model.demo?.sessions[chat.id] ?? model.workspace?.sessions[chat.id]
        return row?.startedAt ?? row?.updatedAt ?? nowMs()
    }
}

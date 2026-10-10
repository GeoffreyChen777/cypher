// New session — a real composer page, not a form. Mirrors the old mobile
// app's canvas (faded app icon + "What are we building?" + glass composer with
// picker chips) and the desktop's new-session canvas (composer expanded with
// in-pill pickers). The space already fixes device + folder; the composer
// (A quick chat has no space: just a device, and the host makes a scratch
// folder for it on send — composer.rs's project-less first send.)
// carries the agent/model chip, and sending mints the chat, queues the first
// run, and swaps straight into the live session.

import PhotosUI
import SwiftUI

struct NewSessionView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    let spaceId: String
    @Binding var path: [Route]
    /// Set for a quick chat: the device it runs on, no project.
    var quickDeviceId: String? = nil

    // Sticky run config (the old app persisted these to prefs.db).
    private let harness = "pi"
    @AppStorage("newSessionPiModels") private var storedModels = "{}"
    @AppStorage("newSessionReasoning") private var storedReasoning = ""

    @State private var draft = ""
    @State private var showPicker = false
    @State private var showRefPicker = false
    @State private var showCheckoutPicker = false
    @State private var attachments: [StagedAttachment] = []
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var showPhotoPicker = false
    @State private var attachError: String?
    @State private var catalog = RemotePiCatalog()
    @State private var catalogRevision = 0
    @State private var refs: [RepoRef] = []
    @State private var selectedRef: String?
    @State private var checkoutKind: CheckoutKind = .local
    @State private var busy = false
    @State private var mentionEditor = MentionEditor()
    @State private var mentionSearch = MentionSearch()
    @FocusState private var focused: Bool
    /// Basis for the leading header's fixed width (SessionView's pattern —
    /// iOS 26 proposes leading toolbar items almost nothing).
    @State private var viewWidth: CGFloat = 0

    private var space: Space? {
        guard quickDeviceId == nil else { return nil }
        return model.spaces.first { $0.id == spaceId }
    }

    /// Where the session will run: the quick chat's device or the space's.
    private var targetDeviceId: String? {
        quickDeviceId ?? space?.deviceId
    }

    /// This canvas's own route, swapped for the session on send.
    private var route: Route {
        quickDeviceId.map { .quickChat(deviceId: $0) } ?? .newSession(spaceId: spaceId)
    }

    private var models: [ModelInfo] {
        catalog.models(for: targetDeviceId ?? "")
    }

    private var storedModel: String {
        let picks = (try? JSONDecoder().decode([String: String].self, from: Data(storedModels.utf8))) ?? [:]
        return picks[targetDeviceId ?? ""] ?? ""
    }

    private func rememberModel(_ id: String) {
        guard let deviceId = targetDeviceId else { return }
        var picks = (try? JSONDecoder().decode([String: String].self, from: Data(storedModels.utf8))) ?? [:]
        picks[deviceId] = id
        if let data = try? JSONEncoder().encode(picks), let json = String(data: data, encoding: .utf8) {
            storedModels = json
        }
    }

    private var selectedModel: ModelInfo? {
        models.first { $0.id == storedModel } ?? models.first
    }

    private var reasoning: String? {
        guard let selectedModel else { return nil }
        if selectedModel.reasoningLevels.isEmpty { return nil }
        if selectedModel.reasoningLevels.contains(storedReasoning) { return storedReasoning }
        return HarnessCatalog.defaultReasoning(for: selectedModel)
    }

    // `body` is split into layers (layout → sheets → lifecycle) so each
    // type-checks on its own; one long chain risks CI's older Xcode giving up
    // ("unable to type-check this expression in reasonable time").
    var body: some View {
        withSheets
            .photosPicker(
                isPresented: $showPhotoPicker, selection: $pickerItems,
                maxSelectionCount: 8, matching: .images
            )
            .onChange(of: pickerItems) { _, items in
                guard !items.isEmpty else { return }
                stage(items)
            }
            .onAppear {
                focused = true
                if model.launchAutosend {
                    model.launchAutosend = false
                    draft = "Sketch the plan for porting the diff pane."
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 800_000_000)
                        send()
                    }
                }
            }
    }

    private var layout: some View {
        VStack(spacing: 0) {
            // Canvas — tap dismisses the keyboard, like the old app.
            ZStack {
                Theme.bg
                VStack(spacing: 24) {
                    Image("CypherAppIcon")
                        .renderingMode(.original)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 84, height: 84)
                        .opacity(0.22)
                        .accessibilityHidden(true)
                    Text("What are we building?")
                        .font(Theme.sans(15))
                        .foregroundStyle(Theme.textFaint)
                }
            }
            .contentShape(Rectangle())
            .onTapGesture { focused = false }

            if let targetDeviceId, !model.deviceOnline(targetDeviceId), model.demo == nil {
                offlineNotice(deviceId: targetDeviceId)
            }
            PiCatalogNotice(
                catalog: catalog,
                deviceName: model.deviceName(targetDeviceId ?? "")
            ) {
                catalogRevision += 1
            }

            // Where-it-runs scope row (checkout + base ref), left-aligned
            // above the composer — the composer pill keeps only the agent chip.
            if space?.gitDetected == true {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        chip(icon: checkoutIcon, label: checkoutLabel) {
                            focused = false
                            showCheckoutPicker = true
                        }
                        chip(icon: .gitBranch, label: refLabel) {
                            focused = false
                            showRefPicker = true
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .padding(.bottom, 8)
                .disabled(busy)
            }

            composer
                .padding(.bottom, 8)
        }
        .background(Theme.bg.ignoresSafeArea())
        .onGeometryChange(for: CGFloat.self) {
            $0.size.width
        } action: {
            viewWidth = $0
        }
        .navigationTitle(quickDeviceId == nil ? "New session" : "Quick chat")  // feeds the back menu
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(.hidden, for: .navigationBar)
        .toolbar { titleToolbar }
    }

    @ToolbarContentBuilder
    private var titleToolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            VStack(alignment: .leading, spacing: 1) {
                Text(quickDeviceId == nil ? "New session" : "Quick chat")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text)
                if let quickDeviceId {
                    Text("No project · \(model.deviceName(quickDeviceId))")
                        .font(Theme.sans(10.5))
                        .foregroundStyle(Theme.textMuted.opacity(0.6))
                        .lineLimit(1)
                } else if let space {
                    Text("\(space.displayName) · \(model.deviceName(space.deviceId))")
                        .font(Theme.sans(10.5))
                        .foregroundStyle(Theme.textMuted.opacity(0.6))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            // Bound the title while leaving room for native Back.
            .frame(width: max(140, viewWidth - 170), alignment: .leading)
        }
        // Bare text on the bar, not a glass capsule.
        .sharedBackgroundVisibility(.hidden)
    }

    private var catalogTaskKey: String {
        "\(targetDeviceId ?? "")/\(model.connected)/\(targetDeviceId.map { model.deviceOnline($0) } ?? false)/\(scenePhase)/\(catalogRevision)"
    }

    private var withSheets: some View {
        layout
            .sheet(isPresented: $showRefPicker) {
                RefPickerSheet(refs: refs, selected: selectedRef) { ref in
                    await pickRef(ref)
                }
            }
            .sheet(isPresented: $showCheckoutPicker) {
                CheckoutPickerSheet(
                    kind: checkoutKind,
                    selectedRefHasWorktree: selectedRefRow?.worktreePath != nil
                ) { kind in
                    pickCheckout(kind)
                }
            }
            .task(id: "\(spaceId)/\(space?.deviceId ?? "")") {
                // Load refs for the branch chip (git spaces only).
                guard let space, space.gitDetected else { return }
                if let loaded = await model.listRefs(space: space) {
                    guard !Task.isCancelled, self.space?.deviceId == space.deviceId else { return }
                    refs = loaded
                    if selectedRef == nil {
                        selectedRef = loaded.first(where: \.current)?.name ?? loaded.first?.name
                    }
                }
            }
            .task(id: catalogTaskKey) {
                guard let targetDeviceId else { return }
                await catalog.load(deviceId: targetDeviceId, fetch: model.listPiModels)
            }
            .sheet(isPresented: $showPicker) {
                ModelPickerSheet(
                    models: models, modelId: selectedModel?.id ?? "", reasoning: reasoning,
                    loading: catalog.loading, onRefresh: { catalogRevision += 1 }
                ) { id, level in
                    rememberModel(id)
                    storedReasoning = level ?? ""
                }
            }
            .onChange(of: showPicker) { _, showing in
                if showing { catalogRevision += 1 }
            }
    }

    // MARK: Composer

    private var composer: some View {
        VStack(spacing: 6) {
            if let attachError {
                Text(attachError)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.danger)
                    .lineLimit(2)
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let mentionToken {
                MentionMenuView(
                    search: mentionSearch, query: mentionToken.query,
                    subtitle: model.mentionSubtitle,
                    onPickSession: pickSession,
                    onPickFile: { mentionEditor.accept(link: Mentions.fileLink(path: $0.path, isDir: $0.isDir)) }
                )
                .padding(.horizontal, 16)
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
            ComposerShell(
                draft: $draft,
                placeholder: "Do anything…",
                sendEnabled: targetReady && selectedModel != nil,
                showStop: false,
                busy: busy,
                alwaysExpanded: true,
                onSend: send,
                attachments: attachments,
                onAttach: { showPhotoPicker = true },
                onRemoveAttachment: { id in attachments.removeAll { $0.id == id } },
                mentions: mentionEditor
            ) {
                // One chip for the model and its thinking level (it rides
                // right of the shell's attach button).
                ModelChip(model: selectedModel, reasoning: reasoning) {
                    focused = false
                    showPicker = true
                }
            }
        }
        .task(
            id: mentionToken.map { "\(spaceId)/\(quickDeviceId ?? "")/\(mentionScope.files?.path ?? "")/\($0.query)" }
        ) {
            guard let mentionToken else { return }
            let scope = mentionScope
            await mentionSearch.run(
                query: mentionToken.query, scope: scope, chats: model.allChats,
                fetch: model.searchFiles)
        }
        .motionAnimation(Motion.fadeQuick, value: mentionToken != nil)
    }

    // MARK: Mentions

    private var mentionToken: MentionToken? {
        targetReady ? mentionEditor.token : nil
    }

    /// Files come from the project's checkout — the existing worktree the
    /// session will reuse, when one is picked. A quick chat has no folder
    /// until its first send, so it offers sessions only.
    private var mentionScope: MentionScope {
        guard let space else {
            return MentionScope(currentChat: nil, project: nil, device: quickDeviceId, files: nil)
        }
        let worktree = checkoutKind == .local ? selectedRefRow?.worktreePath : nil
        return MentionScope(
            currentChat: nil, project: space.id, device: space.deviceId,
            files: MentionScope.Files(
                deviceId: space.deviceId, spaceId: space.id,
                path: worktree))
    }

    private func pickSession(_ session: MentionSession) {
        if Mentions.sessionCapReached(existing: Mentions.sessionRefIds(in: draft), candidate: session.chatId) {
            attachError = "Up to 3 session references per message — remove one first."
            return
        }
        attachError = nil
        mentionEditor.accept(link: Mentions.sessionLink(title: session.title, chatId: session.chatId))
    }

    /// The referenced sessions' snapshots, loaded before the chat is minted
    /// so a failure leaves nothing behind; nil (with the error shown) when
    /// one can't be referenced or loaded.
    private func loadReferences(_ prompt: String) async -> [SessionReference]? {
        let refIds = Mentions.sessionRefIds(in: prompt)
        guard !refIds.isEmpty else { return [] }
        if prompt.hasPrefix("/") {
            attachError = "Session references accompany a normal message, not a slash command."
            return nil
        }
        if let error = SessionReferences.validationError(refs: refIds, currentChat: nil, chats: model.allChats) {
            attachError = error
            return nil
        }
        do {
            return try await model.sessionReferences(refIds)
        } catch {
            attachError = "\(error.localizedDescription) Your draft has been kept."
            return nil
        }
    }

    /// Load picked photos into staged attachments (ComposerView.stage's twin —
    /// the upload happens on send, once the chat and its store exist).
    private func stage(_ items: [PhotosPickerItem]) {
        Task { @MainActor in
            var failed = 0
            for item in items {
                guard let data = try? await item.loadTransferable(type: Data.self),
                    let staged = StagedAttachment.stage(data: data)
                else {
                    failed += 1
                    continue
                }
                attachments.append(staged)
            }
            pickerItems = []
            attachError =
                failed > 0
                ? (failed == 1
                    ? "One image couldn't be attached (unsupported or over 24 MB)."
                    : "\(failed) images couldn't be attached (unsupported or over 24 MB).")
                : nil
        }
    }

    private func chip(icon: LineIcon, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                LineIconView(icon, size: 13, color: Theme.textMuted)
                Text(label)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text.opacity(0.9))
                    .lineLimit(1)
            }
            .padding(.horizontal, 12)
            .frame(height: 40)
            .background(whiteAlpha(0.08), in: Capsule())
            .overlay(Capsule().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
        }
        .buttonStyle(ChipPressButtonStyle())
    }

    // MARK: Checkout model (pickers.rs port)

    private var selectedRefRow: RepoRef? {
        refs.first { $0.name == selectedRef }
    }

    /// checkout_label: New worktree / Current worktree / Current checkout.
    private var checkoutLabel: String {
        switch checkoutKind {
        case .newWorktree: return "New worktree"
        case .local: return selectedRefRow?.worktreePath != nil ? "Current worktree" : "Current checkout"
        }
    }

    private var checkoutIcon: LineIcon {
        checkoutKind == .local && selectedRefRow?.worktreePath == nil ? .folder : .folderWithFiles
    }

    /// ref_label: "From <ref>" only when a NEW worktree will be created off it.
    private var refLabel: String {
        guard let name = selectedRef else { return "Select ref" }
        return checkoutKind == .newWorktree ? "From \(name)" : name
    }

    /// pick_ref (draft mode): a worktree'd ref flips to "Current worktree";
    /// base picks just record; a plain non-current ref in Local mode CHECKS
    /// OUT the space folder (it must never silently flip the mode).
    private func pickRef(_ row: RepoRef) async -> String? {
        if row.worktreePath != nil {
            selectedRef = row.name
            checkoutKind = .local
            return nil
        }
        if checkoutKind == .newWorktree || row.current {
            selectedRef = row.name
            return nil
        }
        guard let space else { return nil }
        let error = await model.switchSpaceRef(space: space, refName: row.name)
        if error == nil {
            selectedRef = row.name
            if let reloaded = await model.listRefs(space: space) {
                refs = reloaded
            }
        }
        return error
    }

    /// pick_checkout: dropping back to Local with a plain non-current ref
    /// picked drops the pick — the current branch takes over.
    private func pickCheckout(_ kind: CheckoutKind) {
        if kind == .local, checkoutKind == .newWorktree,
            let row = selectedRefRow, row.worktreePath == nil, !row.current
        {
            selectedRef = refs.first(where: \.current)?.name
        }
        checkoutKind = kind
    }

    private var canSend: Bool {
        guard !busy, targetReady, selectedModel != nil else { return false }
        return !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !attachments.isEmpty
    }

    private var targetReady: Bool {
        guard let targetDeviceId else { return false }
        return model.demo != nil || (model.connected && model.deviceOnline(targetDeviceId))
    }

    private func offlineNotice(deviceId: String) -> some View {
        Text("\(model.deviceName(deviceId)) is offline. Reconnect it to start a session. Your draft stays here.")
            .font(Theme.sans(12))
            .foregroundStyle(Theme.warning.opacity(0.9))
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .background(Theme.warning.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
            .padding(.horizontal, 12)
            .padding(.bottom, 8)
    }

    /// Mint the chat per the checkout plan, queue the first run, swap to the
    /// live session (composer.rs on-send: current checkout as-is, reuse the
    /// picked ref's worktree, or CreateWorktree off the base first).
    private func send() {
        if let quickDeviceId {
            sendQuickChat(deviceId: quickDeviceId)
            return
        }
        guard let space, canSend, let selectedModel else { return }
        let prompt = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        busy = true
        let config = ChatConfig(
            harness: harness, model: selectedModel.id,
            reasoning: reasoning, sandbox: "workspace-write")
        Task { @MainActor in
            defer { busy = false }
            guard let sessions = await loadReferences(prompt) else { return }
            var cwd: String?
            var branch = selectedRef
            switch checkoutKind {
            case .newWorktree:
                guard let base = selectedRef else {
                    attachError = "Select a base branch before creating a worktree."
                    return
                }
                guard let worktreePath = await model.createWorktree(space: space, base: base) else {
                    attachError =
                        "Couldn't create the worktree on \(model.deviceName(space.deviceId)). Your draft has been kept."
                    return
                }
                cwd = worktreePath
                branch = base
            case .local:
                if let worktree = selectedRefRow?.worktreePath {
                    cwd = worktree  // reuse the ref's existing checkout
                }
            }
            guard targetReady, self.space?.deviceId == space.deviceId else {
                attachError = "The project device is no longer available. Your draft has been kept."
                return
            }
            guard
                let chatId = model.createChat(
                    space: space, config: config,
                    branch: branch, cwd: cwd)
            else {
                attachError = "Couldn't create the session. Check your connection and retry."
                busy = false
                return
            }
            await startSession(chatId: chatId, prompt: prompt, sessions: sessions)
        }
    }

    /// Quick chat (composer.rs first send without a project): the host makes
    /// the scratch folder under the new chat's id, the row is minted without
    /// a space, then the first run is queued like any session's.
    private func sendQuickChat(deviceId: String) {
        guard canSend, let selectedModel else { return }
        let prompt = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        busy = true
        let config = ChatConfig(
            harness: harness, model: selectedModel.id,
            reasoning: reasoning, sandbox: "workspace-write")
        Task { @MainActor in
            defer { busy = false }
            guard let sessions = await loadReferences(prompt) else { return }
            do {
                let chatId = try await model.createQuickChat(deviceId: deviceId, config: config)
                await startSession(chatId: chatId, prompt: prompt, sessions: sessions)
            } catch {
                attachError =
                    "Couldn't create the quick chat's folder on \(model.deviceName(deviceId)) — \(error.localizedDescription). Your draft has been kept."
            }
        }
    }

    /// Upload the staged images into the new chat, queue its first run and
    /// swap the canvas for the live session.
    private func startSession(chatId: String, prompt: String, sessions: [SessionReference]) async {
        guard let chat = model.chat(id: chatId),
            let store = model.sessionStore(for: chat)
        else {
            attachError = "Couldn't create the session. Check your connection and retry."
            busy = false
            return
        }
        // Upload staged images now that the chat's store exists; the doc
        // entry must never point at files that don't (ComposerView.send).
        var paths: [String] = []
        for att in attachments {
            do {
                let path = try await store.uploadAttachment(name: att.name, data: att.data)
                AttachmentImageCache.shared.seed(
                    deviceId: chat.deviceId, path: path,
                    name: att.name, data: att.data)
                paths.append(path)
            } catch {
                attachError = "Attachment upload failed — \(error.localizedDescription)"
                busy = false
                return
            }
        }
        let content = paths.isEmpty ? prompt : withAttachments(text: prompt, paths: paths)
        // Without comments the envelope can't fail to build.
        let agentPrompt = try? SessionReferences.agentPrompt(sessions: sessions, comments: [], visible: content)
        guard targetReady,
            store.sendRun(prompt: content, chat: chat, attachments: paths, agentPrompt: agentPrompt)
        else {
            attachError = "Couldn't queue the message. Your draft has been kept."
            return
        }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        draft = ""
        attachments = []
        busy = false
        // Replace the canvas with the live session (in-place swap, no
        // back-through-canvas).
        if path.last == route {
            path.removeLast()
        }
        path.append(.chat(chatId))
    }
}

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
    @State private var showTraitPicker = false
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

    private var harnesses: [HarnessInfo] {
        HarnessCatalog.harnesses
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

    var body: some View {
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
            PiCatalogNotice(catalog: catalog,
                            deviceName: model.deviceName(targetDeviceId ?? "")) {
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
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { viewWidth = $0 }
        .navigationTitle(quickDeviceId == nil ? "New session" : "Quick chat")  // feeds the back menu
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(.hidden, for: .navigationBar)
        .toolbar {
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
        .sheet(isPresented: $showRefPicker) {
            RefPickerSheet(refs: refs, selected: selectedRef) { ref in
                await pickRef(ref)
            }
        }
        .sheet(isPresented: $showCheckoutPicker) {
            CheckoutPickerSheet(kind: checkoutKind,
                                selectedRefHasWorktree: selectedRefRow?.worktreePath != nil) { kind in
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
        .task(id: "\(targetDeviceId ?? "")/\(model.connected)/\(targetDeviceId.map { model.deviceOnline($0) } ?? false)/\(scenePhase)/\(catalogRevision)") {
            guard let targetDeviceId else { return }
            await catalog.load(deviceId: targetDeviceId, fetch: model.listPiModels)
        }
        .sheet(isPresented: $showPicker) {
            ModelPickerSheet(harness: .constant(harness), modelId: Binding(
                get: { selectedModel?.id ?? "" },
                set: { rememberModel($0) }
            ), reasoning: Binding(
                get: { reasoning },
                set: { storedReasoning = $0 ?? "" }
            ), lockedHarness: true, harnesses: harnesses, catalogs: [harness: models],
               loading: catalog.loading, onRefresh: { catalogRevision += 1 })
        }
        .onChange(of: showPicker) { _, showing in
            if showing { catalogRevision += 1 }
        }
        .sheet(isPresented: $showTraitPicker) {
            TraitPickerSheet(reasoning: Binding(
                get: { reasoning },
                set: { storedReasoning = $0 ?? "" }
            ), levels: selectedModel?.reasoningLevels ?? [])
        }
        .photosPicker(isPresented: $showPhotoPicker, selection: $pickerItems,
                      maxSelectionCount: 8, matching: .images)
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
                onRemoveAttachment: { id in attachments.removeAll { $0.id == id } }
            ) {
                // Model + trait chips, split like the desktop's footer pickers
                // (they ride right of the shell's attach button).
                ComposerChip(label: selectedModel?.label ?? "Select model") {
                    focused = false
                    showPicker = true
                }
                if let reasoning {
                    ComposerChip(label: HarnessCatalog.reasoningLabel(reasoning)) {
                        focused = false
                        showTraitPicker = true
                    }
                }
            }
        }
    }

    /// Load picked photos into staged attachments (ComposerView.stage's twin —
    /// the upload happens on send, once the chat and its store exist).
    private func stage(_ items: [PhotosPickerItem]) {
        Task { @MainActor in
            var failed = 0
            for item in items {
                guard let data = try? await item.loadTransferable(type: Data.self),
                      let staged = StagedAttachment.stage(data: data) else {
                    failed += 1
                    continue
                }
                attachments.append(staged)
            }
            pickerItems = []
            attachError = failed > 0
                ? (failed == 1 ? "One image couldn't be attached (unsupported or over 24 MB)."
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
           let row = selectedRefRow, row.worktreePath == nil, !row.current {
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
        let config = ChatConfig(harness: harness, model: selectedModel.id,
                                reasoning: reasoning, sandbox: "workspace-write")
        Task { @MainActor in
            defer { busy = false }
            var cwd: String?
            var branch = selectedRef
            switch checkoutKind {
            case .newWorktree:
                guard let base = selectedRef else {
                    attachError = "Select a base branch before creating a worktree."
                    return
                }
                guard let worktreePath = await model.createWorktree(space: space, base: base) else {
                    attachError = "Couldn't create the worktree on \(model.deviceName(space.deviceId)). Your draft has been kept."
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
            guard let chatId = model.createChat(space: space, config: config,
                                                branch: branch, cwd: cwd) else {
                attachError = "Couldn't create the session. Check your connection and retry."
                busy = false
                return
            }
            await startSession(chatId: chatId, prompt: prompt)
        }
    }

    /// Quick chat (composer.rs first send without a project): the host makes
    /// the scratch folder under the new chat's id, the row is minted without
    /// a space, then the first run is queued like any session's.
    private func sendQuickChat(deviceId: String) {
        guard canSend, let selectedModel else { return }
        let prompt = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        busy = true
        let config = ChatConfig(harness: harness, model: selectedModel.id,
                                reasoning: reasoning, sandbox: "workspace-write")
        Task { @MainActor in
            defer { busy = false }
            do {
                let chatId = try await model.createQuickChat(deviceId: deviceId, config: config)
                await startSession(chatId: chatId, prompt: prompt)
            } catch {
                attachError = "Couldn't create the quick chat's folder on \(model.deviceName(deviceId)) — \(error.localizedDescription). Your draft has been kept."
            }
        }
    }

    /// Upload the staged images into the new chat, queue its first run and
    /// swap the canvas for the live session.
    private func startSession(chatId: String, prompt: String) async {
        guard let chat = model.chat(id: chatId),
              let store = model.sessionStore(for: chat) else {
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
                AttachmentImageCache.shared.seed(deviceId: chat.deviceId, path: path,
                                                 name: att.name, data: att.data)
                paths.append(path)
            } catch {
                attachError = "Attachment upload failed — \(error.localizedDescription)"
                busy = false
                return
            }
        }
        guard targetReady,
              store.sendRun(prompt: paths.isEmpty ? prompt : withAttachments(text: prompt, paths: paths),
                            chat: chat, attachments: paths) else {
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

// MARK: - Composer chip

/// The composer's picker trigger chip: optional brand mark, label, chevron —
/// one per picker, split like the desktop's footer (model | traits).
struct ComposerChip: View {
    let label: String
    var badgeHarness: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let badgeHarness {
                    HarnessBadge(harness: badgeHarness, size: 15)
                }
                Text(label)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text.opacity(0.9))
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textFaint)
            }
            .padding(.horizontal, 13)
            .frame(height: 40)
            .background(whiteAlpha(0.08), in: Capsule())
            .overlay(Capsule().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
        }
        .buttonStyle(ChipPressButtonStyle())
    }
}

// MARK: - Model picker sheet

/// Detent bottom sheet, grouped by PROVIDER like the desktop picker: a
/// horizontal rail of provider chips (brand mark · name · count) across the
/// top selects the group, and the list below shows only that provider's
/// models — a flat list of every gateway model was unreadable on a phone.
/// The rail opens on the current model's provider. Effort lives in its own
/// TraitPickerSheet, split like the desktop's footer pickers.
struct ModelPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Binding var harness: String
    @Binding var modelId: String
    @Binding var reasoning: String?
    /// True when reconfiguring a live chat: the harness can't change mid-chat.
    var lockedHarness = false
    /// Pi-only; legacy harnesses are displayable in history, never selectable.
    var harnesses: [HarnessInfo] = []
    /// Empty means unavailable, never a static fallback.
    var catalogs: [String: [ModelInfo]] = [:]
    var loading = false
    var onRefresh: (() -> Void)?

    /// The provider whose models the list shows; nil = the current model's.
    @State private var pickedProvider: String?

    private var models: [ModelInfo] { catalogs["pi"] ?? [] }
    private var groups: [HarnessCatalog.ProviderGroup] { HarnessCatalog.providerGroups(models) }

    private var viewedProvider: String? {
        if let pickedProvider, groups.contains(where: { $0.id == pickedProvider }) { return pickedProvider }
        let current = HarnessCatalog.providerId(of: modelId)
        if groups.contains(where: { $0.id == current }) { return current }
        return groups.first?.id
    }

    private var viewedGroup: HarnessCatalog.ProviderGroup? {
        groups.first { $0.id == viewedProvider }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if loading {
                        // Centered in the sheet's visible area, not tucked
                        // into the list's top-left corner.
                        VStack(spacing: 10) {
                            ProgressView()
                                .controlSize(.regular)
                                .tint(Theme.textMuted)
                            Text("Loading models…")
                                .font(Theme.sans(13))
                                .foregroundStyle(Theme.textMuted)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 72)
                    } else if groups.isEmpty {
                        Text("No models loaded. Close this picker and retry from the session.")
                            .font(Theme.sans(13))
                            .foregroundStyle(Theme.textMuted)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 72)
                            .padding(.horizontal, 12)
                    } else {
                        if groups.count > 1 {
                            providerRail
                        }
                        VStack(alignment: .leading, spacing: 4) {
                            if let viewedGroup {
                                SheetLabel(groups.count > 1 ? viewedGroup.name : "Model")
                                ForEach(viewedGroup.models) { m in
                                    PickRow(title: m.label,
                                            subtitle: HarnessCatalog.contextLabel(m),
                                            selected: m.id == modelId) {
                                        select(model: m)
                                    }
                                }
                            }
                        }
                    }
                }
                .padding(20)
                .padding(.bottom, 12)
                // The loading/empty content has a narrow intrinsic width;
                // don't wait for full-width PickRows to size the sheet.
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(SheetStyle.panel)
            .navigationTitle("Select model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if let onRefresh {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Refresh", action: onRefresh).disabled(loading)
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    /// The provider rail: one chip per provider in catalog order. The viewed
    /// chip is the filled high-contrast pill (the rows' selected language);
    /// the others carry a faint count.
    private var providerRail: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(groups) { group in
                    providerChip(group, viewed: group.id == viewedProvider)
                }
            }
            .padding(.horizontal, 2)
        }
        .scrollClipDisabled()
        .accessibilityIdentifier("model-provider-rail")
    }

    private func providerChip(_ group: HarnessCatalog.ProviderGroup, viewed: Bool) -> some View {
        Button {
            UISelectionFeedbackGenerator().selectionChanged()
            withAnimation(Motion.collapse) { pickedProvider = group.id }
        } label: {
            HStack(spacing: 6) {
                if let badge = HarnessCatalog.providerBadgeHarness(group.id) {
                    HarnessBadge(harness: badge, size: 14, neutral: viewed ? Theme.bg : Theme.text)
                } else {
                    Image(systemName: "globe")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(viewed ? Theme.bg : Theme.textMuted)
                }
                Text(group.name)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(viewed ? Theme.bg : Theme.text)
                    .lineLimit(1)
                Text("\(group.models.count)")
                    .font(Theme.sans(11))
                    .foregroundStyle(viewed ? Theme.bg.opacity(0.65) : Theme.textFaint)
            }
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(viewed ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.06)),
                        in: Capsule())
            .overlay(Capsule().strokeBorder(whiteAlpha(viewed ? 0 : 0.08), lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(ChipPressButtonStyle())
        .accessibilityIdentifier("model-provider-\(group.id)")
        .accessibilityAddTraits(viewed ? .isSelected : [])
    }

    private func select(model m: ModelInfo) {
        UISelectionFeedbackGenerator().selectionChanged()
        if harness != "pi" {
            harness = "pi"
        }
        modelId = m.id
        if let current = reasoning, m.reasoningLevels.contains(current) {
            return
        }
        reasoning = HarnessCatalog.defaultReasoning(for: m)
    }
}

// MARK: - Trait (effort) picker sheet

/// The effort ladder in its own detent sheet — the composer's second picker
/// chip, split from the model list like the desktop's Traits dropdown.
struct TraitPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Binding var reasoning: String?
    let levels: [String]

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    SheetLabel("Effort")
                    ForEach(levels, id: \.self) { level in
                        PickRow(title: HarnessCatalog.reasoningLabel(level),
                                subtitle: Self.effortHint(level),
                                selected: reasoning == level) {
                            UISelectionFeedbackGenerator().selectionChanged()
                            reasoning = level
                        }
                    }
                }
                .padding(20)
                .padding(.bottom, 12)
            }
            .background(SheetStyle.panel)
            .navigationTitle("Traits")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    /// One-line hints for the ladder (the special modes deserve explanation).
    static func effortHint(_ level: String) -> String? {
        switch level {
        case "minimal": return "Quickest, lightest touch"
        case "low": return "Fastest responses"
        case "medium": return "Balanced speed and depth"
        case "high": return "Thorough reasoning"
        case "xhigh": return "Extended reasoning"
        case "max": return "Maximum reasoning budget"
        case "ultra": return "Highest Codex tier"
        case "ultracode": return "X-High plus the ultracode setting"
        case "ultrathink": return "Deep-thinking prompt mode"
        default: return nil
        }
    }
}

// MARK: - Shared picker row

/// t3's ModelRow — the ONE row style every picker sheet uses (model, trait,
/// ref, checkout): the selected row is a filled high-contrast pill with a
/// trailing checkmark; unselected rows sit almost flat on the sheet. Optional
/// leading line icon and a busy spinner for rows whose pick runs async (git
/// checkouts).
struct PickRow: View {
    let title: String
    var subtitle: String?
    var icon: LineIcon?
    var busy = false
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                if let icon {
                    LineIconView(icon, size: 15,
                                 color: selected ? Theme.bg : Theme.textMuted)
                        .frame(width: 20)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(Theme.sans(15, weight: .medium))
                        .foregroundStyle(selected ? Theme.bg : Theme.text)
                    if let subtitle {
                        Text(subtitle)
                            .font(Theme.sans(12))
                            .foregroundStyle(selected ? Theme.bg.opacity(0.65) : Theme.textMuted)
                    }
                }
                Spacer(minLength: 8)
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(selected ? Theme.bg : Theme.textMuted)
                } else {
                    Image(systemName: "checkmark")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.bg)
                        .opacity(selected ? 1 : 0)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 11)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(selected ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.03)),
                        in: RoundedRectangle(cornerRadius: 12))
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(SheetRowButtonStyle())
    }
}

// MARK: - Ref picker sheet

/// Base-ref selector (the desktop footer's branch popover): branch rows with
/// current-checkout / worktree markers. Picks that require a git checkout run
/// inline — a spinner on the row, git's error surfaced in place on failure
/// (dirty tree, held ref), success dismisses.
struct RefPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    let refs: [RepoRef]
    let selected: String?
    /// Returns an error message to keep the sheet open, or nil to close.
    let onPick: (RepoRef) async -> String?

    @State private var switching: String?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    SheetLabel("Ref")
                    if refs.isEmpty {
                        Text("Loading refs from the device…")
                            .font(Theme.sans(13))
                            .foregroundStyle(Theme.textFaint)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 28)
                    } else {
                        ForEach(refs, id: \.name) { ref in
                            row(ref)
                        }
                    }
                    if let error {
                        Text(error)
                            .font(Theme.sans(12.5))
                            .foregroundStyle(Theme.danger)
                            .padding(.horizontal, 4)
                            .padding(.top, 4)
                    }
                }
                .padding(20)
            }
            .background(SheetStyle.panel)
            .navigationTitle("Select ref")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    private func row(_ ref: RepoRef) -> some View {
        PickRow(title: ref.name,
                subtitle: subtitle(for: ref),
                busy: switching == ref.name,
                selected: ref.name == selected) {
            guard switching == nil else { return }
            UISelectionFeedbackGenerator().selectionChanged()
            error = nil
            switching = ref.name
            Task { @MainActor in
                let result = await onPick(ref)
                switching = nil
                if let result {
                    error = result
                } else {
                    dismiss()
                }
            }
        }
    }

    private func subtitle(for ref: RepoRef) -> String? {
        if ref.current { return "Current checkout" }
        if ref.worktreePath != nil { return "Checked out in a worktree" }
        return nil
    }
}

// MARK: - Checkout picker sheet

/// Where the session runs (the desktop's checkout popover): the space's
/// folder as-is (or the picked ref's existing worktree), or a fresh isolated
/// worktree created off the base ref on send.
struct CheckoutPickerSheet: View {
    @Environment(\.dismiss) private var dismiss
    let kind: CheckoutKind
    let selectedRefHasWorktree: Bool
    let onPick: (CheckoutKind) -> Void

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    SheetLabel("Checkout")
                    row(.local,
                        title: selectedRefHasWorktree ? "Current worktree" : "Current checkout",
                        subtitle: selectedRefHasWorktree
                            ? "Reuse the picked ref's existing worktree"
                            : "Run in the space's folder as-is")
                    row(.newWorktree, title: "New worktree",
                        subtitle: "A fresh isolated worktree created off the picked base ref")
                }
                .padding(20)
            }
            .background(SheetStyle.panel)
            .navigationTitle("Checkout")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationCornerRadius(32)
    }

    private func row(_ rowKind: CheckoutKind, title: String, subtitle: String) -> some View {
        PickRow(title: title, subtitle: subtitle, selected: rowKind == kind) {
            UISelectionFeedbackGenerator().selectionChanged()
            onPick(rowKind)
            dismiss()
        }
    }
}

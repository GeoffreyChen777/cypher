// Composer — the floating glass shell in the t3 mobile composer's shape: a
// collapsed capsule (editor + send circle) that morphs into an expanded card
// with a toolbar ROW below it (attach circle · scrolling chips · pinned send)
// when the editor takes focus. Carries the desktop's Send→Steer→Stop
// semantics: live run + text = steer (same up-arrow), live run + empty = stop.
//
// Expansion is focus-driven like t3's, with the old deterministic content
// triggers kept as a floor (attachments, newline, >26 chars) — content-size
// measurement oscillates at the boundary, so it is never measured.

import PhotosUI
import SwiftUI

/// Each accepted send ends an editing generation. Late callbacks from the
/// retired native editor must not restore its old text or erase a new draft.
@MainActor
@Observable
final class ComposerDraft {
    var text = ""
    private(set) var revision = 0

    var binding: Binding<String> {
        let generation = revision
        return Binding(
            get: { self.text },
            set: { value in
                guard self.revision == generation else { return }
                self.text = value
            }
        )
    }

    func clearAfterSend() {
        revision += 1
        text = ""
    }
}

/// Shared glass shell + input + action row. `chips` render in the expanded
/// toolbar row between the attach and send circles.
struct ComposerShell<Chips: View>: View {
    @Binding var draft: String
    /// Changes only after an accepted send, never on individual keystrokes.
    var editorRevision = 0
    var placeholder = "Message"
    var sendEnabled: Bool
    var showStop: Bool
    var busy = false
    var hasComments = false
    /// New-session composers stay expanded — the picker chips ARE the page.
    var alwaysExpanded = false
    /// Hold the expanded layout while a picker sheet is up: presenting the
    /// sheet blurs the editor, and collapsing on that blur flaps the
    /// transcript's bottom inset mid-presentation (t3 derives expansion from
    /// focus OR sheet-active for exactly this reason).
    var keepExpanded = false
    var onSend: () -> Void
    var onStop: () -> Void = {}
    /// Staged image attachments (attachment-ui.tsx AttachmentStrip inside the
    /// pill). Non-empty forces the expanded layout, like focus.
    var attachments: [StagedAttachment] = []
    /// Present the photo picker; nil hides the attach button.
    var onAttach: (() -> Void)? = nil
    var onRemoveAttachment: (String) -> Void = { _ in }
    /// Screenshot rig (-focuscomposer): take keyboard focus shortly after
    /// appearing, so the keyboard-up transcript states can be driven headless.
    var autoFocus = false
    @ViewBuilder var chips: Chips

    @State private var focus = ComposerFocus()
    @State private var editorID = "composer-editor-\(UUID().uuidString)"

    private var editing: Bool { focus.isFocused }

    private var expanded: Bool {
        alwaysExpanded || keepExpanded || editing || !attachments.isEmpty || hasComments
            || draft.contains("\n") || draft.count > 26
    }

    /// One animatable shape for background/glass/hairline: capsule-radius
    /// collapsed (46pt tall pill), 20pt card expanded (t3's 999↔20 morph).
    private var surfaceShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: expanded ? 20 : 24)
    }

    // Switching between VStack/HStack via AnyLayout (rather than an if/else
    // that swaps container types) keeps `input`'s view identity stable across
    // the compact↔expanded flip — an if/else here would tear down and rebuild
    // the editor, dropping keyboard focus mid-type.
    private var shellLayout: AnyLayout {
        expanded
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 0))
            : AnyLayout(HStackLayout(alignment: .center, spacing: 12))
    }

    var body: some View {
        surface
            // Focus-widen: margins pull in slightly while typing (chat-session.tsx).
            .padding(.horizontal, editing ? 10 : 16)
            .motionAnimation(Motion.resize, value: editing)
            .motionAnimation(Motion.collapse, value: expanded)
            .task(id: editorRevision) {
                guard editorRevision > 0 else { return }
                // Reattach keyboard focus to the new editor, not the retired
                // UIKit instance. This task never writes the draft.
                focus.isFocused = false
                await Task.yield()
                guard !Task.isCancelled else { return }
                focus.isFocused = true
            }
            .onAppear {
                guard autoFocus else { return }
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    focus.isFocused = true
                }
            }
    }

    /// The glass surface: collapsed = editor + send in one capsule row;
    /// expanded = attachment strip over a tall editor, with the control row
    /// (attach circle · scrolling chips · pinned send) along the card's
    /// bottom edge — everything stays inside the glass.
    private var surface: some View {
        shellLayout {
            if expanded, !attachments.isEmpty {
                AttachmentStripView(attachments: attachments, remove: onRemoveAttachment)
                    .padding(.bottom, 10)
                    .disabled(busy)
            }
            input
                .padding(.leading, expanded ? 4 : 13)
                .padding(.trailing, expanded ? 4 : 0)
                .padding(.vertical, expanded ? 4 : 5)
                .frame(minHeight: expanded ? 64 : nil, alignment: .topLeading)
            if expanded {
                HStack(spacing: 8) {
                    if onAttach != nil {
                        attachButton
                    }
                    // Chips scroll; the send button stays pinned.
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            chips
                                .disabled(busy)
                        }
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("composer-model-controls")
                    }
                    .scrollClipDisabled(false)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    actionButton
                }
                .padding(.top, 8)
            } else {
                actionButton
            }
        }
        .padding(.horizontal, expanded ? 12 : 5)
        .padding(.vertical, expanded ? 12 : 5)
        .background(whiteAlpha(0.04), in: surfaceShape)
        .glassEffect(.regular.interactive(), in: surfaceShape)
        .overlay(surfaceShape.strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        // The whole glass surface focuses the editor, not just the TextField's
        // own text box: the collapsed pill is mostly padding, and a tap that
        // misses the text box falls through to the transcript underneath —
        // whose tap-to-blur then RESIGNS the keyboard. That's the "have to
        // press a few times" miss. Buttons and chips still win their taps.
        // Masked to .subviews while focused so cursor-placement taps inside
        // the editor stay fully native.
        .contentShape(surfaceShape)
        .gesture(TapGesture().onEnded { focus.isFocused = true },
                 including: editing ? .subviews : .all)
    }

    private var input: some View {
        ComposerTextInput(text: $draft, focus: focus, editorID: editorID, enabled: !busy,
                          placeholder: placeholder)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .topLeading) {
                if draft.isEmpty {
                    Text(placeholder)
                        .font(Theme.sans(16))
                        .foregroundStyle(Theme.textFaint)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            // A live Steer does not transition the session's running state.
            // Explicitly retire the native editor's cached text on success.
            .id(editorRevision)
    }

    private var attachButton: some View {
        Button {
            onAttach?()
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 40, height: 40)
                .background(whiteAlpha(0.06), in: Circle())
                .overlay(Circle().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(busy)
    }

    /// Attachments count as content: an image-only send is a send, never a stop.
    private var hasContent: Bool {
        CommentPrompt.hasSendContent(text: draft, attachmentCount: attachments.count,
                                     commentCount: hasComments ? 1 : 0)
    }

    private var actionButton: some View {
        Button {
            if showStop, !hasContent {
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                onStop()
            } else {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onSend()
            }
        } label: {
            Group {
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(Theme.bg)
                } else if showStop, !hasContent {
                    RoundedRectangle(cornerRadius: 3.5)
                        .fill(Theme.bg)
                        .frame(width: 12, height: 12)
                } else {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(buttonActive ? Theme.bg : Theme.textFaint)
                }
            }
            .frame(width: 40, height: 40)
            .background(buttonActive ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.10)),
                        in: Circle())
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!buttonActive)
        .motionAnimation(Motion.fadeQuick, value: showStop)
    }

    private var buttonActive: Bool {
        if showStop, !hasContent { return true }
        return sendEnabled && hasContent && !busy
    }
}

/// The live-chat composer: input, the photo attach button, the model + trait
/// picker chips (harness stays locked mid-chat; picks merge into the chat's
/// config row for the next dispatch), and the morphing action button.
struct ComposerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.commentDrafts) private var commentDrafts
    let store: SessionStore
    let chat: Chat
    let runLive: Bool
    let catalog: RemotePiCatalog
    var connectionRetry = 0

    @State private var draftState = ComposerDraft()
    private var text: String { draftState.text }
    @State private var attachments: [StagedAttachment] = []
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var showPicker = false
    @State private var uploading = false
    @State private var uploadError: String?
    @State private var showModelPicker = false
    @State private var showTraitPicker = false
    @State private var catalogRevision = 0

    private var harness: String { chat.config?.harness ?? "" }

    private var models: [ModelInfo] {
        catalog.models(for: chat.deviceId)
    }

    private var currentModel: ModelInfo? {
        models.first { $0.id == chat.config?.model }
    }

    private var canControl: Bool {
        harness == "pi" && (model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId)))
    }

    private var canSend: Bool {
        canControl && (runLive || currentModel != nil)
    }

    private var currentReasoning: String? {
        guard let currentModel else { return nil }
        guard !currentModel.reasoningLevels.isEmpty else { return nil }
        if let r = chat.config?.reasoning, currentModel.reasoningLevels.contains(r) { return r }
        return HarnessCatalog.defaultReasoning(for: currentModel)
    }

    var body: some View {
        VStack(spacing: 6) {
            if harness != "pi" {
                Text("Read-only session · iOS supports Pi")
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 8)
            }
            if let uploadError {
                Text(uploadError)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.danger)
                    .lineLimit(2)
                    .padding(.horizontal, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let commentDrafts {
                PendingCommentsBar(drafts: commentDrafts)
            }
            ComposerShell(
                draft: draftState.binding,
                editorRevision: draftState.revision,
                sendEnabled: canSend,
                showStop: runLive && canControl,
                busy: uploading,
                hasComments: !(commentDrafts?.comments.isEmpty ?? true),
                keepExpanded: showModelPicker || showTraitPicker,
                onSend: send,
                onStop: {
                    guard canControl else { return }
                    if !store.sendInterrupt() { uploadError = "Couldn't queue Stop. Please retry." }
                },
                attachments: attachments,
                onAttach: { showPicker = true },
                onRemoveAttachment: { id in attachments.removeAll { $0.id == id } },
                autoFocus: model.launchFocusComposer
            ) {
                ComposerChip(label: currentModel?.label ?? chat.config?.model ?? "Select model") {
                    showModelPicker = true
                }
                .disabled(harness != "pi" || !canControl)
                if let currentReasoning {
                    ComposerChip(label: HarnessCatalog.reasoningLabel(currentReasoning)) {
                        showTraitPicker = true
                    }
                }
            }
        }
        .photosPicker(isPresented: $showPicker, selection: $pickerItems,
                      maxSelectionCount: 8, matching: .images)
        .onChange(of: pickerItems) { _, items in
            guard !items.isEmpty else { return }
            stage(items)
        }
        .sheet(isPresented: $showModelPicker) {
            ModelPickerSheet(
                harness: .constant(harness),
                modelId: Binding(
                    get: { currentModel?.id ?? "" },
                    set: { writeConfig(model: $0, reasoning: chat.config?.reasoning) }
                ),
                reasoning: Binding(
                    get: { chat.config?.reasoning },
                    set: { writeConfig(model: chat.config?.model, reasoning: $0) }
                ),
                lockedHarness: true,
                catalogs: [harness: models],
                loading: catalog.loading,
                onRefresh: { catalogRevision += 1 }
            )
        }
        .sheet(isPresented: $showTraitPicker) {
            TraitPickerSheet(
                reasoning: Binding(
                    get: { currentReasoning },
                    set: { writeConfig(model: chat.config?.model, reasoning: $0) }
                ),
                levels: currentModel?.reasoningLevels ?? []
            )
        }
        .task(id: "\(chat.id)/\(chat.deviceId)/\(harness)/\(canControl)/\(scenePhase)/\(catalogRevision)/\(connectionRetry)") {
            guard harness == "pi" else { return }
            await catalog.load(deviceId: chat.deviceId, fetch: model.listPiModels)
        }
        .onChange(of: showModelPicker) { _, showing in
            if showing { catalogRevision += 1 }
        }
        .onAppear {
            if model.launchSheet == "config" {
                model.launchSheet = nil
                showModelPicker = true
            }
        }
    }

    /// Merge a model/effort change into the chat's config row (LWW; the host
    /// picks it up on the next run dispatch). Copies preserve modelOptions.
    private func writeConfig(model newModel: String?, reasoning newReasoning: String?) {
        guard canControl, let selected = models.first(where: { $0.id == newModel }) else { return }
        var config = chat.config ?? ChatConfig(harness: harness, model: nil,
                                               reasoning: nil, sandbox: "workspace-write")
        config.model = newModel
        config.reasoning = newReasoning.flatMap { selected.reasoningLevels.contains($0) ? $0 : nil }
        model.setChatConfig(chatId: chat.id, config: config)
    }

    /// Load picked photos into staged attachments (HEIC transcodes to JPEG;
    /// unsupported/oversized picks surface as an error line).
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
            if failed > 0 {
                uploadError = failed == 1
                    ? "One image couldn't be attached (unsupported or over 24 MB)."
                    : "\(failed) images couldn't be attached (unsupported or over 24 MB)."
            } else {
                uploadError = nil
            }
        }
    }

    private func send() {
        guard canSend, !uploading else { return }
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let staged = attachments
        let batch = commentDrafts?.snapshot()
        let hasComments = !(batch?.comments.isEmpty ?? true)
        guard CommentPrompt.hasSendContent(text: prompt, attachmentCount: staged.count,
                                           commentCount: batch?.comments.count ?? 0) else { return }
        guard !CommentPrompt.blocksSlash(prompt, hasComments: hasComments) else {
            uploadError = "Comments accompany a normal message, not a slash command. Send or remove the comments first."
            return
        }

        if staged.isEmpty {
            if deliver(content: prompt, paths: [], comments: batch) { clearDraft() }
            return
        }
        // Upload first, send after: the refs trailer needs the committed
        // paths, and the doc entry must never point at files that don't
        // exist. The shell shows the spinner (`busy`) while chunks stream.
        uploading = true
        uploadError = nil
        Task { @MainActor in
            defer { uploading = false }
            do {
                var paths: [String] = []
                for att in staged {
                    let path = try await store.uploadAttachment(name: att.name, data: att.data)
                    // Seed the cache so our own bubble renders from local
                    // bytes instead of a round-trip.
                    AttachmentImageCache.shared.seed(deviceId: chat.deviceId, path: path,
                                                     name: att.name, data: att.data)
                    paths.append(path)
                }
                if deliver(content: withAttachments(text: prompt, paths: paths), paths: paths, comments: batch) {
                    attachments = []
                    clearDraft()
                }
            } catch {
                uploadError = "Attachment upload failed — \(error.localizedDescription)"
            }
        }
    }

    private func deliver(content: String, paths: [String], comments batch: CommentBatch?) -> Bool {
        if let batch, commentDrafts?.generation != batch.generation {
            uploadError = "The session changed. The message wasn't sent."
            return false
        }
        guard canSend else {
            uploadError = "The device is no longer available. Your draft has been kept."
            return false
        }
        let agentPrompt: String?
        do {
            agentPrompt = try CommentPrompt.agentPrompt(batch?.comments ?? [], visible: content)
        } catch {
            uploadError = "Couldn't prepare the comments. Your draft has been kept."
            return false
        }
        let queued = runLive
            ? store.sendSteer(prompt: content, agentPrompt: agentPrompt)
            : store.sendRun(prompt: content, chat: chat, attachments: paths, agentPrompt: agentPrompt)
        if !queued { uploadError = "Couldn't queue the message. Your draft has been kept." }
        else {
            uploadError = nil
            if let batch { commentDrafts?.consume(batch) }
        }
        return queued
    }

    private func clearDraft() {
        draftState.clearAfterSend()
    }
}

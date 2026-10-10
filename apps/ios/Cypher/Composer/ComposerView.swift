// The live-chat composer: input, photo attachments, model and trait chips,
// slash and @ menus, and the desktop's Send→Steer→Stop semantics (live run +
// text = steer, live run + empty = stop) inside the glass ComposerShell.

import PhotosUI
import SwiftUI

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
    /// A side chat: it runs the parent's model — no model/effort chips, no
    /// context arc (there's no session row to read or config to write).
    var sideChat = false
    /// The `/` menu's height cap: what's free above the composer.
    var slashMenuMaxHeight = SlashMenuView.defaultMaxHeight

    @State private var draftState = ComposerDraft()
    private var text: String { draftState.text }
    @State private var attachments: [StagedAttachment] = []
    @State private var pickerItems: [PhotosPickerItem] = []
    @State private var showPicker = false
    @State private var uploading = false
    @State private var uploadError: String?
    @State private var showModelPicker = false
    @State private var catalogRevision = 0
    @State private var commands = RemoteCommandCatalog()
    @State private var modes = SlashModesCatalog()
    @State private var mentionEditor = MentionEditor()
    @State private var mentionSearch = MentionSearch()

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

    /// Slash commands skip the model gate, as on the desktop: `/compact`
    /// must work on a chat whose model the host no longer offers.
    private var canSend: Bool {
        canControl && (runLive || currentModel != nil || isSlashDraft)
    }

    private var isSlashDraft: Bool {
        text.trimmingCharacters(in: .whitespacesAndNewlines).hasPrefix("/")
    }

    private var sessionRow: SessionRow? { model.sessionRow(chatId: chat.id) }

    private var compactAvailability: CompactAvailability {
        if harness != "pi" { return .unsupported }
        guard canControl else { return .offline }
        let status = effectiveStatus(sessionRow, now: nowMs())
        if runLive || uploading || status == .working || status == .awaitingInput { return .busy }
        return .ready
    }

    /// Compact is its own Run (`/compact`, never a steer); the draft,
    /// attachments and pending comments stay where they are.
    private func compact() {
        guard compactAvailability == .ready else { return }
        uploadError =
            store.sendRun(prompt: "/compact", chat: chat)
            ? nil : "Couldn't queue Compact. Please retry."
    }

    private func loadCommands(force: Bool = false) async {
        await commands.load(deviceId: chat.deviceId, force: force, fetch: model.listCommands)
    }

    /// The `/` menu's list for the draft, nil while it's closed.
    private var slashLevel: SlashLevel? {
        canControl ? SlashMenu.level(in: text, commands: commands.commands) : nil
    }

    /// The `@query` under the caret, while the `/` menu isn't up. A side
    /// chat has no checkout row of its own to search, so it offers none.
    private var mentionToken: MentionToken? {
        guard canControl, !sideChat, slashLevel == nil else { return nil }
        return mentionEditor.token
    }

    private var mentionScope: MentionScope {
        MentionScope(
            currentChat: chat.id, project: chat.spaceId, device: chat.deviceId,
            files: MentionScope.Files(deviceId: chat.deviceId, chatId: chat.id))
    }

    /// A picked session: up to three distinct ones per message.
    private func pickSession(_ session: MentionSession) {
        if Mentions.sessionCapReached(existing: Mentions.sessionRefIds(in: text), candidate: session.chatId) {
            uploadError = "Up to 3 session references per message — remove one first."
            return
        }
        uploadError = nil
        mentionEditor.accept(link: Mentions.sessionLink(title: session.title, chatId: session.chatId))
    }

    /// What the menu's badges can say about this chat. A side chat has no Pi
    /// switches, session row or subagents of its own (composer.rs: the main
    /// transport only).
    private var slashFacts: SlashFacts {
        guard !sideChat else { return SlashFacts() }
        return SlashFacts(
            modes: modes.chatId == chat.id ? modes.modes : nil,
            context: sessionRow?.contextUsage,
            runningSubagents: sessionRow?.subagents.filter { $0.status == .running }.count ?? 0)
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
            if let slashLevel {
                SlashMenuView(
                    catalog: commands, level: slashLevel, facts: slashFacts,
                    onPickCommand: { draftState.replace(with: SlashMenu.accept($0)) },
                    onPickChoice: { draftState.replace(with: SlashMenu.accept($0, in: text)) },
                    onRetry: { Task { await loadCommands(force: true) } },
                    maxHeight: slashMenuMaxHeight
                )
                .padding(.horizontal, 16)
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            } else if let mentionToken {
                MentionMenuView(
                    search: mentionSearch, query: mentionToken.query,
                    subtitle: model.mentionSubtitle,
                    onPickSession: pickSession,
                    onPickFile: { mentionEditor.accept(link: Mentions.fileLink(path: $0.path, isDir: $0.isDir)) },
                    maxHeight: slashMenuMaxHeight
                )
                .padding(.horizontal, 16)
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
            ComposerShell(
                draft: draftState.binding,
                editorRevision: draftState.revision,
                caretRequest: draftState.caretRequest,
                sendEnabled: canSend,
                showStop: runLive && canControl,
                busy: uploading,
                hasComments: !(commentDrafts?.comments.isEmpty ?? true),
                keepExpanded: showModelPicker,
                onSend: send,
                onStop: {
                    guard canControl else { return }
                    if !store.sendInterrupt() { uploadError = "Couldn't queue Stop. Please retry." }
                },
                attachments: attachments,
                onAttach: { showPicker = true },
                onRemoveAttachment: { id in attachments.removeAll { $0.id == id } },
                autoFocus: model.launchFocusComposer,
                contextGauge: sideChat
                    ? nil
                    : sessionRow?.contextUsage.map {
                        ContextGauge(usage: $0, availability: compactAvailability, onCompact: compact)
                    },
                mentions: sideChat ? nil : mentionEditor
            ) {
                if !sideChat {
                    ModelChip(
                        model: currentModel,
                        fallbackLabel: chat.config?.model ?? "Select model",
                        reasoning: currentReasoning
                    ) {
                        showModelPicker = true
                    }
                    .disabled(harness != "pi" || !canControl)
                }
            }
        }
        .photosPicker(
            isPresented: $showPicker, selection: $pickerItems,
            maxSelectionCount: 8, matching: .images
        )
        .onChange(of: pickerItems) { _, items in
            guard !items.isEmpty else { return }
            stage(items)
        }
        .sheet(isPresented: $showModelPicker) {
            // One write per pick, model and level together: two writes from
            // this render's `chat` would let the second undo the first.
            ModelPickerSheet(
                models: models,
                modelId: currentModel?.id ?? "",
                reasoning: currentReasoning,
                loading: catalog.loading,
                onRefresh: { catalogRevision += 1 },
                onSelect: { writeConfig(model: $0, reasoning: $1) }
            )
        }
        .task(
            id:
                "\(chat.id)/\(chat.deviceId)/\(harness)/\(canControl)/\(scenePhase)/\(catalogRevision)/\(connectionRetry)"
        ) {
            guard harness == "pi" else { return }
            // Prefetch, so the first `/` opens a filled menu.
            async let commandList: Void = canControl ? loadCommands() : ()
            await catalog.load(deviceId: chat.deviceId, fetch: model.listPiModels)
            await commandList
        }
        .task(id: "\(chat.id)/\(slashLevel != nil)") {
            // Each opening asks the host afresh: a `/fast` sent since flips
            // the badge.
            guard slashLevel != nil, !sideChat else { return }
            let deviceId = chat.deviceId
            await modes.load(chatId: chat.id) { try await model.piSessionModes(deviceId: deviceId, chatId: $0) }
        }
        .task(id: mentionToken.map { "\(chat.id)/\($0.query)" }) {
            guard let mentionToken else { return }
            let scope = mentionScope
            await mentionSearch.run(
                query: mentionToken.query, scope: scope, chats: model.allChats,
                fetch: model.searchFiles)
        }
        .motionAnimation(Motion.fadeQuick, value: slashLevel != nil)
        .motionAnimation(Motion.fadeQuick, value: mentionToken != nil)
        .onChange(of: showModelPicker) { _, showing in
            if showing { catalogRevision += 1 }
        }
        .onAppear {
            // A fork before a prompt hands that prompt back for editing.
            if draftState.text.isEmpty, let seeded = model.takePendingDraft(chatId: chat.id) {
                draftState.replace(with: seeded)
            }
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
        var config =
            chat.config
            ?? ChatConfig(
                harness: harness, model: nil,
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
                    let staged = StagedAttachment.stage(data: data)
                else {
                    failed += 1
                    continue
                }
                attachments.append(staged)
            }
            pickerItems = []
            if failed > 0 {
                uploadError =
                    failed == 1
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
        guard
            CommentPrompt.hasSendContent(
                text: prompt, attachmentCount: staged.count,
                commentCount: batch?.comments.count ?? 0)
        else { return }
        guard !CommentPrompt.blocksSlash(prompt, hasComments: hasComments) else {
            uploadError = "Comments accompany a normal message, not a slash command. Send or remove the comments first."
            return
        }
        // Referenced sessions are checked before anything uploads; the
        // draft, attachments and comments stay put on any failure.
        let refIds = Mentions.sessionRefIds(in: prompt)
        if !refIds.isEmpty {
            if prompt.hasPrefix("/") {
                uploadError = "Session references accompany a normal message, not a slash command."
                return
            }
            if let error = SessionReferences.validationError(
                refs: refIds, currentChat: chat.id,
                chats: model.allChats)
            {
                uploadError = error
                return
            }
        }

        if staged.isEmpty, refIds.isEmpty {
            if deliver(content: prompt, paths: [], comments: batch) { clearDraft() }
            return
        }
        // Load references, then upload, then send: the refs trailer needs
        // the committed paths, and the doc entry must never point at files
        // that don't exist. The shell shows the spinner (`busy`) meanwhile.
        uploading = true
        uploadError = nil
        Task { @MainActor in
            defer { uploading = false }
            let sessions: [SessionReference]
            do {
                sessions = try await model.sessionReferences(refIds)
            } catch {
                uploadError = "\(error.localizedDescription) Your draft has been kept."
                return
            }
            if staged.isEmpty {
                if deliver(content: prompt, paths: [], comments: batch, sessions: sessions) { clearDraft() }
                return
            }
            do {
                var paths: [String] = []
                for att in staged {
                    let path = try await store.uploadAttachment(name: att.name, data: att.data)
                    // Seed the cache so our own bubble renders from local
                    // bytes instead of a round-trip.
                    AttachmentImageCache.shared.seed(
                        deviceId: chat.deviceId, path: path,
                        name: att.name, data: att.data)
                    paths.append(path)
                }
                if deliver(
                    content: withAttachments(text: prompt, paths: paths), paths: paths,
                    comments: batch, sessions: sessions)
                {
                    attachments = []
                    clearDraft()
                }
            } catch {
                uploadError = "Attachment upload failed — \(error.localizedDescription)"
            }
        }
    }

    private func deliver(
        content: String, paths: [String], comments batch: CommentBatch?,
        sessions: [SessionReference] = []
    ) -> Bool {
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
            agentPrompt = try SessionReferences.agentPrompt(
                sessions: sessions, comments: batch?.comments ?? [],
                visible: content)
        } catch {
            uploadError = "Couldn't prepare the comments. Your draft has been kept."
            return false
        }
        let queued =
            runLive
            ? store.sendSteer(prompt: content, agentPrompt: agentPrompt)
            : store.sendRun(prompt: content, chat: chat, attachments: paths, agentPrompt: agentPrompt)
        if !queued {
            uploadError = "Couldn't queue the message. Your draft has been kept."
        } else {
            uploadError = nil
            if let batch { commentDrafts?.consume(batch) }
        }
        return queued
    }

    private func clearDraft() {
        draftState.clearAfterSend()
    }
}

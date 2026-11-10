// One memory store's page and one file of it (crates/ui/src/shell/bots/
// memory.rs). A page watches its store (`watchMemory`) while it shows and
// stops when it goes; every change is one `editMemory`, whose sentence shows
// at the foot for a moment. The store reads top to bottom: writes held for
// review, MEMORY.md rendered, the other files, consolidation, then recent
// changes, each undoable.

import SwiftUI

// MARK: - Watching and editing

/// A page's live store and its edits in flight.
@MainActor
@Observable
final class MemoryPageState {
    /// The store as last seen; nil until its first frame.
    var view: MemoryView?
    /// Why the watch stopped.
    var error: String?
    /// Bumped by Retry: restarts the watch.
    var attempt = 0
    /// Edits on their way, by the control that made them.
    var busy: Set<String> = []
    /// The last reply's sentence, shown for a moment.
    var notice: (text: String, error: Bool)?
    /// Why the last edit failed: an alert.
    var failure: String?
    private var noticeClear: Task<Void, Never>?

    /// Follow `scope` until cancelled (the page went away).
    func watch(_ data: (any WorkspaceData)?, scope: MemoryScope) async {
        error = nil
        guard let data else {
            error = "Not connected."
            return
        }
        do {
            for try await next in data.watchMemory(scope: scope) {
                view = next
                error = nil
            }
        } catch {
            guard !Task.isCancelled else { return }
            self.error = (error as? BotActionError)?.message ?? error.localizedDescription
        }
    }

    /// One edit; its reply (or failure) shows as the notice. True when it
    /// went through.
    @discardableResult
    func edit(_ data: (any WorkspaceData)?, scope: MemoryScope, _ edit: MemoryEdit, key: String) async -> Bool {
        guard !busy.contains(key) else { return false }
        guard let data else {
            failure = "Not connected."
            return false
        }
        busy.insert(key)
        defer { busy.remove(key) }
        do {
            let reply = try await data.editMemory(scope: scope, edit: edit)
            if !reply.isEmpty { say(reply, error: false) }
            return true
        } catch {
            failure = (error as? BotActionError)?.message ?? error.localizedDescription
            return false
        }
    }

    func say(_ text: String, error: Bool) {
        notice = (text, error)
        noticeClear?.cancel()
        noticeClear = Task { [weak self] in
            try? await Task.sleep(for: .seconds(4))
            guard !Task.isCancelled else { return }
            withAnimation(Motion.fadeQuick) { self?.notice = nil }
        }
    }
}

extension View {
    /// A memory page's failed edit as an alert.
    fileprivate func memoryFailureAlert(_ state: MemoryPageState) -> some View {
        alert(
            "Couldn't do that",
            isPresented: Binding(get: { state.failure != nil }, set: { if !$0 { state.failure = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(state.failure ?? "")
        }
    }
}

/// The notice line at a memory page's foot.
private struct MemoryNotice: View {
    let notice: (text: String, error: Bool)?

    var body: some View {
        if let notice {
            Text(notice.text)
                .font(Theme.sans(14, relativeTo: .footnote))
                .foregroundStyle(notice.error ? Theme.danger : Theme.text)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .glassEffect(.regular, in: Capsule())
                .padding(.horizontal, 24)
                .padding(.bottom, 8)
                .transition(.opacity.combined(with: .move(edge: .bottom)))
                .accessibilityIdentifier("memory-notice")
        }
    }
}

/// A store's title: the workspace board's own memory is "Memory"; others
/// are named by what owns them.
@MainActor
func memoryStoreTitle(_ scope: MemoryScope, botId: String, data: (any WorkspaceData)?) -> String {
    switch scope {
    case .you: return "Your memory"
    case .project(let spaceId):
        return data?.spaces.first { $0.id == spaceId }?.displayName ?? "A removed project"
    case .workspace(let id):
        if id == botId { return "Memory" }
        return data?.bot(id: id)?.name ?? "A removed workspace"
    case .role(let agent, _): return agent
    }
}

// MARK: - Store

struct MemoryPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let scope: MemoryScope
    @State private var state = MemoryPageState()
    @State private var editor: MemoryEditorTarget?
    /// A file just created: opened on top of this page.
    @State private var created: BotPage?

    var body: some View {
        List {
            if let view = state.view {
                if view.files.isEmpty && view.pending.isEmpty && view.changes.isEmpty {
                    empty
                } else {
                    content(view)
                }
            } else if let error = state.error {
                if model.data?.memoryStore(scope: scope) == nil {
                    // Nothing has written this store yet: it's empty, not
                    // broken.
                    empty
                } else {
                    unavailable(error)
                }
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading…").foregroundStyle(Theme.textMuted)
                }
                .frame(maxWidth: .infinity)
                .listRowBackground(Color.clear)
            }
        }
        .botPageStyle(title: memoryStoreTitle(scope, botId: botId, data: model.data))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    editor = MemoryEditorTarget(path: nil, text: "")
                } label: {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("New file")
                .accessibilityIdentifier("memory-new-file")
            }
        }
        .safeAreaInset(edge: .bottom) {
            MemoryNotice(notice: state.notice)
        }
        .animation(Motion.fadeQuick, value: state.notice?.text)
        .sheet(item: $editor) { target in
            MemoryEditorSheet(
                scope: scope, target: target, existing: Set(state.view?.files.map(\.path) ?? [])
            ) { path, reply in
                if !reply.isEmpty { state.say(reply, error: false) }
                if target.path == nil { created = .memoryFile(scope, path: path) }
            }
        }
        .navigationDestination(item: $created) { BotPageView(botId: botId, page: $0) }
        .memoryFailureAlert(state)
        .task(id: "\(state.attempt)") {
            await state.watch(model.data, scope: scope)
        }
    }

    private var empty: some View {
        ContentUnavailableView {
            Label("No memory yet", systemImage: "book.closed")
        } description: {
            Text(MemoryFormat.emptyHint(scope))
        } actions: {
            Button("New File") { editor = MemoryEditorTarget(path: nil, text: "") }
                .buttonStyle(.glass)
                .accessibilityIdentifier("memory-empty-new-file")
        }
        .listRowBackground(Color.clear)
    }

    private func unavailable(_ error: String) -> some View {
        ContentUnavailableView {
            Label("Memory isn't available", systemImage: "icloud.slash")
        } description: {
            Text(error)
        } actions: {
            Button("Retry") { state.attempt += 1 }
                .buttonStyle(.glass)
                .accessibilityIdentifier("memory-retry")
        }
        .listRowBackground(Color.clear)
    }

    @ViewBuilder
    private func content(_ view: MemoryView) -> some View {
        Section {
        } footer: {
            Text(MemoryFormat.readers(scope))
        }
        if !view.pending.isEmpty {
            Section {
                ForEach(view.pending.sorted { $0.at < $1.at }) { write in
                    pendingRow(write).groupedRowStyle()
                }
            } header: {
                ListSectionHeader(title: "To review")
            } footer: {
                Text("From runs that read untrusted content.")
            }
        }
        indexSection(view)
        let others = view.files.filter { $0.path != MemoryLimits.index }
        if !others.isEmpty {
            Section {
                ForEach(others) { file in
                    NavigationLink(value: BotPage.memoryFile(scope, path: file.path)) {
                        fileRow(file)
                    }
                    .groupedRowStyle()
                }
            } header: {
                ListSectionHeader(title: "Files") {
                    Text(
                        "\(MemoryFormat.count(others.count, "file", "files")) · \(MemoryFormat.size(others.reduce(0) { $0 + $1.text.utf8.count }))"
                    )
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textFaint)
                }
            }
        }
        consolidation(view.store)
        if !view.changes.isEmpty {
            Section {
                ForEach(view.changes.prefix(MemoryFormat.changesShown)) { change in
                    changeRow(change, exists: view.files.contains { $0.path == change.path })
                        .groupedRowStyle()
                }
            } header: {
                ListSectionHeader(title: "Recent changes") {
                    Text(MemoryFormat.count(view.changes.count, "change", "changes"))
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                }
            }
        }
    }

    // MARK: Held writes

    private func pendingRow(_ write: MemoryPending) -> some View {
        let (saveKey, discardKey) = ("accept-\(write.id)", "discard-\(write.id)")
        let busy = state.busy.contains(saveKey) || state.busy.contains(discardKey)
        return VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(MemoryFormat.displayPath(write.path))
                        .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                        .foregroundStyle(Theme.text)
                        .lineLimit(2)
                    Spacer(minLength: 8)
                    Text(BotTime.ago(write.at))
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                }
                Text("\(write.author.label) · \(write.reason.trimmingCharacters(in: .whitespacesAndNewlines))")
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
            }
            Group {
                if let preview = write.preview, !preview.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    MemoryMarkdown(text: preview, cacheKey: "memory-pending-\(write.id)")
                        .padding(12)
                        .background(
                            Theme.text.opacity(0.04), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                } else {
                    Text(write.preview == nil ? "Deletes the file." : "Leaves the file empty.")
                        .font(Theme.sans(14, relativeTo: .body))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            HStack(spacing: 10) {
                Spacer()
                MemoryPillButton(title: "Don't save", primary: false, busy: false) {
                    Task { await state.edit(model.data, scope: scope, .discard(id: write.id), key: discardKey) }
                }
                .accessibilityIdentifier("memory-discard-\(write.id)")
                MemoryPillButton(title: "Save", primary: true, busy: state.busy.contains(saveKey)) {
                    Task { await state.edit(model.data, scope: scope, .accept(id: write.id), key: saveKey) }
                }
                .accessibilityIdentifier("memory-accept-\(write.id)")
            }
            .disabled(busy)
        }
        .padding(.vertical, 6)
    }

    // MARK: MEMORY.md

    private func indexSection(_ view: MemoryView) -> some View {
        let index = view.files.first { $0.path == MemoryLimits.index }
        return Section {
            if let index, !index.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                MemoryMarkdown(text: index.text, cacheKey: "memory-index")
                    .padding(.vertical, 6)
                    .groupedRowStyle()
            } else {
                Text("Empty. It's the index every run reads first; the other files are read on demand.")
                    .font(Theme.sans(14, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .groupedRowStyle()
            }
        } header: {
            ListSectionHeader(title: "MEMORY.md") {
                Button("Edit") {
                    editor = MemoryEditorTarget(path: MemoryLimits.index, text: index?.text ?? "")
                }
                .font(Theme.sans(14, weight: .medium, relativeTo: .footnote))
                .accessibilityLabel("Edit MEMORY.md")
                .accessibilityIdentifier("memory-index-edit")
            }
        } footer: {
            if let index { Text(MemoryFormat.byLine(index)) }
        }
    }

    // MARK: Files

    private func fileRow(_ file: MemoryFile) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "doc.text")
                .font(.system(size: 15))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(MemoryFormat.displayPath(file.path))
                    .font(Theme.sans(16, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
                Text("\(MemoryFormat.size(file.text.utf8.count)) · \(MemoryFormat.byLine(file))")
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
    }

    // MARK: Consolidation

    @ViewBuilder
    private func consolidation(_ store: MemoryStore) -> some View {
        let setting = store.consolidation
        let waiting = MemoryFormat.passWaiting(store)
        Section {
            Toggle(
                isOn: Binding(
                    get: { setting.enabled },
                    set: { on in
                        Task {
                            await state.edit(
                                model.data, scope: scope, .setConsolidation(enabled: on, hour: setting.hour),
                                key: "consolidation")
                        }
                    })
            ) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Nightly pass")
                    Text("Rereads recent sessions to merge, correct and prune what's here. Skipped when nothing new happened.")
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            .disabled(state.busy.contains("consolidation"))
            .groupedRowStyle()
            .accessibilityIdentifier("memory-consolidation")
            if setting.enabled {
                Picker(
                    "Time",
                    selection: Binding(
                        get: { setting.hour },
                        set: { hour in
                            Task {
                                await state.edit(
                                    model.data, scope: scope, .setConsolidation(enabled: true, hour: hour),
                                    key: "consolidation")
                            }
                        })
                ) {
                    ForEach(0..<24, id: \.self) { Text(MemoryFormat.hour($0)).tag($0) }
                }
                .pickerStyle(.menu)
                .groupedRowStyle()
                .accessibilityIdentifier("memory-consolidation-hour")
            }
            if let pass = store.lastPass {
                lastPass(pass).groupedRowStyle()
            }
            if let month = store.month, month.passes > 0 {
                BotFact(
                    label: "This month",
                    value: "\(MemoryFormat.tokens(month.tokens)) · \(MemoryFormat.count(month.passes, "pass", "passes"))"
                )
                .groupedRowStyle()
            }
            Button {
                Task { await state.edit(model.data, scope: scope, .runPass, key: "run-pass") }
            } label: {
                Label(waiting ? "Running…" : "Run Now", systemImage: "play")
            }
            .disabled(waiting || state.busy.contains("run-pass"))
            .groupedRowStyle()
            .accessibilityIdentifier("memory-run-pass")
            if let pass = store.lastPass, pass.outcome == .done, pass.undo != nil {
                Button {
                    Task { await state.edit(model.data, scope: scope, .undoPass(passId: pass.id), key: "undo-pass") }
                } label: {
                    Label("Undo This Pass", systemImage: "arrow.uturn.backward")
                }
                .disabled(state.busy.contains("undo-pass"))
                .groupedRowStyle()
                .accessibilityIdentifier("memory-undo-pass")
            }
        } header: {
            ListSectionHeader(title: "Consolidation") {
                Text(setting.enabled ? "Nightly at \(MemoryFormat.hour(setting.hour))" : "Off")
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textFaint)
            }
        }
    }

    private func lastPass(_ pass: MemoryPass) -> some View {
        let summary =
            pass.outcome == .skipped
            ? "Nothing new since the last pass."
            : pass.summary?.trimmingCharacters(in: .whitespacesAndNewlines)
        var when = BotTime.past(pass.endedAt ?? pass.at)
        if pass.manual { when += " · run by hand" }
        if let cost = MemoryFormat.passCost(pass) { when += " · \(cost)" }
        return VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text("Last pass")
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                Spacer()
                BotStatusText(MemoryFormat.outcome(pass.outcome))
            }
            Text(when)
                .font(Theme.sans(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
            if let summary, !summary.isEmpty {
                Text(summary)
                    .font(Theme.sans(14, relativeTo: .body))
                    .foregroundStyle(Theme.text)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    // MARK: Changes

    @ViewBuilder
    private func changeRow(_ change: MemoryChange, exists: Bool) -> some View {
        let undoKey = "undo-\(change.id)"
        let row = HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: MemoryFormat.symbol(change.action))
                .font(.system(size: 14))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(MemoryFormat.title(change))
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
                Text(changeSub(change))
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            if MemoryFormat.undoable(change) {
                Button {
                    undo(change)
                } label: {
                    Image(systemName: "arrow.uturn.backward")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 32, height: 32)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .disabled(state.busy.contains(undoKey))
                .accessibilityLabel("Undo")
                .accessibilityIdentifier("memory-undo-\(change.id)")
            }
        }
        .opacity(change.undone ? 0.55 : 1)
        .padding(.vertical, 2)
        Group {
            if exists {
                NavigationLink(value: BotPage.memoryFile(scope, path: change.path)) { row }
            } else {
                row
            }
        }
        .swipeActions {
            if MemoryFormat.undoable(change) {
                Button("Undo") { undo(change) }
            }
        }
    }

    /// "Atlas · 2h ago", with "undone" and an amber "edited your text".
    private func changeSub(_ change: MemoryChange) -> AttributedString {
        var line = AttributedString("\(change.author.label) · \(BotTime.ago(change.at))")
        if change.undone { line += AttributedString(" · undone") }
        if change.ownerText && !change.undone {
            var tail = AttributedString(" · edited your text")
            tail.foregroundColor = Theme.warning
            line += tail
        }
        return line
    }

    private func undo(_ change: MemoryChange) {
        Task { await state.edit(model.data, scope: scope, .undo(changeId: change.id), key: "undo-\(change.id)") }
    }
}

/// A small pill: the held write's Save (filled) and Don't save.
private struct MemoryPillButton: View {
    let title: String
    let primary: Bool
    let busy: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Text(title).opacity(busy ? 0 : 1)
                if busy { ProgressView().controlSize(.small).tint(Theme.bg) }
            }
            .font(Theme.sans(15, weight: .semibold, relativeTo: .body))
            .padding(.horizontal, 16)
            .frame(minHeight: 34)
            .foregroundStyle(primary ? Theme.bg : Theme.text)
            .background(primary ? Theme.text : Theme.text.opacity(0.08), in: Capsule())
        }
        .buttonStyle(.borderless)
    }
}

// MARK: - File

struct MemoryFilePage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let botId: String
    let scope: MemoryScope
    let path: String
    @State private var state = MemoryPageState()
    /// The file's path now: a rename moves the page with it.
    @State private var current: String?
    @State private var editor: MemoryEditorTarget?
    @State private var renaming = false
    @State private var renameText = ""
    @State private var confirmDelete = false

    private var filePath: String { current ?? path }

    var body: some View {
        let file = state.view?.files.first { $0.path == filePath }
        List {
            if let file {
                Section {
                    BotPageHeader(title: MemoryFormat.displayPath(file.path), lines: nil) {
                        BotHeaderGlyph(symbol: "doc.text")
                    }
                    .groupedRowStyle()
                }
                Section {
                    if file.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text("Empty.").foregroundStyle(Theme.textMuted).groupedRowStyle()
                    } else {
                        MemoryMarkdown(text: file.text, cacheKey: "memory-file-\(file.path)")
                            .padding(.vertical, 6)
                            .groupedRowStyle()
                    }
                } footer: {
                    Text(
                        "\(MemoryFormat.size(file.text.utf8.count)) · \(file.author.label) · \(BotTime.past(max(file.updatedAt, file.createdAt)))"
                    )
                }
            } else if state.view != nil {
                ContentUnavailableView(
                    "This file is gone", systemImage: "doc.questionmark",
                    description: Text("It was deleted or renamed."))
                    .listRowBackground(Color.clear)
            } else if let error = state.error {
                ContentUnavailableView {
                    Label("Memory isn't available", systemImage: "icloud.slash")
                } description: {
                    Text(error)
                } actions: {
                    Button("Retry") { state.attempt += 1 }.buttonStyle(.glass)
                }
                .listRowBackground(Color.clear)
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading…").foregroundStyle(Theme.textMuted)
                }
                .frame(maxWidth: .infinity)
                .listRowBackground(Color.clear)
            }
        }
        // The header names the file; the bar keeps its buttons.
        .botPageStyle(title: "")
        .toolbar {
            if let file {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Edit") { editor = MemoryEditorTarget(path: file.path, text: file.text) }
                        .accessibilityIdentifier("memory-file-edit")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button("Rename…", systemImage: "pencil") {
                            renameText = MemoryFormat.displayPath(file.path)
                            renaming = true
                        }
                        .disabled(file.path == MemoryLimits.index)
                        Button("Copy", systemImage: "doc.on.doc") {
                            UIPasteboard.general.string = file.text
                            state.say("Copied.", error: false)
                        }
                        Button("Delete…", systemImage: "trash", role: .destructive) { confirmDelete = true }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .accessibilityLabel("More")
                    .accessibilityIdentifier("memory-file-more")
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            MemoryNotice(notice: state.notice)
        }
        .animation(Motion.fadeQuick, value: state.notice?.text)
        .sheet(item: $editor) { target in
            MemoryEditorSheet(
                scope: scope, target: target, existing: Set(state.view?.files.map(\.path) ?? [])
            ) { _, reply in
                if !reply.isEmpty { state.say(reply, error: false) }
            }
        }
        .alert("Rename file", isPresented: $renaming) {
            TextField("Name", text: $renameText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Cancel", role: .cancel) {}
            Button("Rename") { rename() }
        } message: {
            Text("A name in this memory, like decisions/rounding.")
        }
        .confirmationDialog(
            "Delete \(MemoryFormat.fileName(filePath))?", isPresented: $confirmDelete, titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) { delete() }
        } message: {
            Text("The file leaves this memory. Recent changes can put it back.")
        }
        .memoryFailureAlert(state)
        .task(id: "\(state.attempt)") {
            await state.watch(model.data, scope: scope)
        }
    }

    private func rename() {
        guard let to = MemoryFormat.path(typed: renameText) else {
            state.failure = "Not usable: no part of it may be empty or start with a dot."
            return
        }
        let from = filePath
        guard to != from else { return }
        if state.view?.files.contains(where: { $0.path == to }) == true {
            state.failure = "\(MemoryFormat.displayPath(to)) already exists."
            return
        }
        Task {
            if await state.edit(model.data, scope: scope, .rename(from: from, to: to), key: "rename") {
                current = to
            }
        }
    }

    private func delete() {
        let path = filePath
        Task {
            if await state.edit(model.data, scope: scope, .delete(path: path), key: "delete") {
                dismiss()
            }
        }
    }
}

// MARK: - Editor

/// What the editor opens on: a file (its path and text) or a new one.
struct MemoryEditorTarget: Identifiable {
    /// nil: a new file, named in the editor.
    var path: String?
    var text: String
    let id = UUID()
}

/// Write a file: its name (a new one), then its text. Saves through
/// `editMemory(.write)` and hands back the path and the engine's sentence.
struct MemoryEditorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let scope: MemoryScope
    let target: MemoryEditorTarget
    /// The store's paths: a new file can't take one.
    let existing: Set<String>
    let onSaved: (_ path: String, _ reply: String) -> Void
    @State private var name = ""
    @State private var text = ""
    @State private var busy = false
    @State private var error: String?
    @FocusState private var focus: Field?

    private enum Field { case name, text }

    private var isNew: Bool { target.path == nil }
    private var bytes: Int { text.utf8.count }

    var body: some View {
        NavigationStack {
            Form {
                if isNew {
                    Section {
                        TextField("build-and-test", text: $name)
                            .font(Theme.mono(15))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .focused($focus, equals: .name)
                            .submitLabel(.next)
                            .onSubmit { focus = .text }
                            .accessibilityIdentifier("memory-editor-name")
                    } header: {
                        Text("Name")
                    } footer: {
                        Text(nameHint)
                    }
                    .groupedRowStyle()
                }
                Section {
                    TextEditor(text: $text)
                        .font(Theme.mono(14))
                        .frame(minHeight: 260)
                        .focused($focus, equals: .text)
                        .accessibilityIdentifier("memory-editor-text")
                } header: {
                    Text(isNew ? "Text" : MemoryFormat.displayPath(target.path ?? ""))
                        .textCase(nil)
                } footer: {
                    if bytes > MemoryLimits.fileMax * 7 / 8 {
                        Text("\(MemoryFormat.size(bytes)) of 64 KB")
                            .foregroundStyle(bytes > MemoryLimits.fileMax ? Theme.danger : Theme.textMuted)
                    } else if isNew {
                        Text("What's worth keeping, in markdown.")
                    }
                }
                .groupedRowStyle()
            }
            .scrollContentBackground(.hidden)
            .background(Theme.surface.ignoresSafeArea())
            .alert(
                "Couldn't save",
                isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })
            ) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(error ?? "")
            }
            .navigationTitle(isNew ? "New file" : "Edit \(MemoryFormat.fileName(target.path ?? ""))")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if busy {
                        ProgressView()
                    } else {
                        Button(isNew ? "Create" : "Save") { save() }
                            .disabled(isNew && MemoryFormat.path(typed: name) == nil)
                            .accessibilityIdentifier("memory-editor-save")
                    }
                }
            }
            .interactiveDismissDisabled(busy || text != target.text)
        }
        .onAppear {
            text = target.text
            focus = isNew ? .name : .text
        }
    }

    /// Where a new file is saved, or why the name can't be one.
    private var nameHint: String {
        if let path = MemoryFormat.path(typed: name) { return "Saved as \(path)" }
        if name.trimmingCharacters(in: .whitespaces).isEmpty {
            return "A topic file, linked from MEMORY.md. Folders are fine: decisions/rounding."
        }
        return "Not usable: no part of it may be empty or start with a dot."
    }

    private func save() {
        let path: String
        if let known = target.path {
            path = known
        } else {
            guard let typed = MemoryFormat.path(typed: name) else {
                error = "Name the file, like build-and-test."
                return
            }
            guard !existing.contains(typed) else {
                error = "\(MemoryFormat.displayPath(typed)) already exists."
                return
            }
            path = typed
        }
        guard bytes <= MemoryLimits.fileMax else {
            error = "Too long: a file keeps up to 64 KB."
            return
        }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                let reply = try await model.workspaceData().editMemory(scope: scope, edit: .write(path: path, text: text))
                onSaved(path, reply)
                dismiss()
            } catch {
                self.error = (error as? BotActionError)?.message ?? error.localizedDescription
            }
        }
    }
}

// MARK: - Layout

extension View {
    /// A memory page: a board page's list.
    func memoryPageStyle(title: String) -> some View {
        botPageStyle(title: title)
    }
}

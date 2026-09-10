import SwiftUI

enum WorkspaceDestination: String, Identifiable {
    case files, changes
    var id: String { rawValue }
    var title: String { self == .files ? "Files" : "Changes" }
}

struct WorkspaceBrowserView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var session: WorkspaceBrowserSession
    let destination: WorkspaceDestination

    init(model: AppModel, chat: Chat, destination: WorkspaceDestination) {
        _session = State(initialValue: WorkspaceBrowserSession(model: model, chat: chat))
        self.destination = destination
    }

    var body: some View {
        NavigationStack {
            Group {
                if destination == .files {
                    WorkspaceDirectoryView(session: session, path: "")
                } else {
                    WorkspaceChangesView(session: session)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Theme.bg)
            .navigationTitle(destination.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button { dismiss() } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 18, weight: .regular))
                            .frame(width: 22, height: 22)
                    }
                        .accessibilityLabel("Close")
                        .accessibilityIdentifier("workspace-close")
                }
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}

private struct WorkspaceDirectoryView: View {
    let session: WorkspaceBrowserSession
    let path: String
    @State private var listing: WorkspaceDirectory?
    @State private var error: String?
    @State private var filter = ""
    @State private var revision = 0
    @State private var generation = UUID()

    var body: some View {
        Group {
            if let listing {
                List {
                    if listing.truncated {
                        Text("Showing first 1,000 items")
                            .font(Theme.sans(12)).foregroundStyle(Theme.warning)
                    }
                    let entries = listing.entries.filter {
                        filter.isEmpty || $0.name.localizedCaseInsensitiveContains(filter)
                    }
                    if entries.isEmpty {
                        Text(filter.isEmpty ? "Empty folder" : "No matches")
                            .foregroundStyle(Theme.textMuted)
                    }
                    ForEach(entries) { entry in
                        if let next = WorkspaceFilePath.child(entry.name, in: path) {
                            NavigationLink {
                                if entry.isDir {
                                    WorkspaceDirectoryView(session: session, path: next)
                                        .navigationTitle(entry.name)
                                } else {
                                    WorkspaceFileView(session: session, path: next)
                                }
                            } label: {
                                Label(entry.name, systemImage: entry.isDir ? "folder" : "doc.text")
                                    .font(Theme.sans(14))
                                    .foregroundStyle(Theme.text)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                            .accessibilityIdentifier("workspace-item-\(next)")
                        }
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .searchable(text: $filter, prompt: "Filter files")
            } else {
                WorkspaceLoadState(error: error, title: "Files unavailable") { revision += 1 }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Refresh", systemImage: "arrow.clockwise") { revision += 1 }
                    .font(.system(size: 18, weight: .regular))
                    .disabled(listing == nil && error == nil)
            }
        }
        .task(id: "\(path)/\(revision)") {
            let ticket = UUID()
            generation = ticket
            listing = nil
            error = nil
            do {
                let result = try await session.directory(path)
                guard !Task.isCancelled, generation == ticket else { return }
                listing = result
            } catch {
                guard !Task.isCancelled, generation == ticket else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }
}

private struct WorkspaceChangesView: View {
    let session: WorkspaceBrowserSession
    @State private var snapshot: WorkspaceChanges?
    @State private var entries: [WorkspaceDiffEntry]?
    @State private var error: String?
    @State private var revision = 0
    @State private var generation = UUID()

    var body: some View {
        Group {
            if let snapshot, let entries {
                if entries.isEmpty {
                    ContentUnavailableView("No changes", systemImage: "checkmark")
                } else {
                    WorkspaceDiffView(path: "Changes", patch: snapshot.patch, partial: snapshot.truncated,
                        entries: entries, loadSources: snapshot.truncated ? nil : { path in
                            try await session.diffSources(snapshot: snapshot, path: path)
                        })
                }
            } else {
                WorkspaceLoadState(error: error, title: "Changes unavailable") { revision += 1 }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Refresh", systemImage: "arrow.clockwise") { revision += 1 }
                    .font(.system(size: 18, weight: .regular))
                    .disabled(snapshot == nil && error == nil)
            }
        }
        .task(id: revision) {
            let ticket = UUID()
            generation = ticket
            snapshot = nil
            entries = nil
            error = nil
            do {
                let result = try await session.changes()
                let prepared = await Task.detached {
                    let patches = result.patches()
                    var seen = Set<String>()
                    return result.files.compactMap { file -> WorkspaceDiffEntry? in
                        guard seen.insert(file.path).inserted else { return nil }
                        return WorkspaceDiffEntry(path: file.path, patch: patches[file.path],
                            additions: file.additions, deletions: file.deletions, binary: file.binary)
                    }
                }.value
                guard !Task.isCancelled, generation == ticket else { return }
                snapshot = result
                entries = prepared
            } catch {
                guard !Task.isCancelled, generation == ticket else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }
}

private struct WorkspaceFileView: View {
    let session: WorkspaceBrowserSession
    let path: String
    @State private var content: WorkspaceFileContent?
    @State private var error: String?
    @State private var revision = 0
    @State private var generation = UUID()
    var body: some View {
        Group {
            if let content {
                if let text = content.text, !content.binary {
                    WorkspaceSourceView(path: path, text: text, partial: content.truncated)
                } else {
                    ContentUnavailableView("No text preview", systemImage: "doc",
                        description: Text("This is a binary or non-UTF-8 file (\(content.bytes) bytes)."))
                }
            } else {
                WorkspaceLoadState(error: error, title: "File unavailable") { revision += 1 }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .navigationTitle((path as NSString).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Refresh", systemImage: "arrow.clockwise") { revision += 1 }
                    .disabled(content == nil && error == nil)
            }
        }
        .task(id: "\(path)/\(revision)") {
            let ticket = UUID()
            generation = ticket
            content = nil
            error = nil
            do {
                let result = try await session.file(path)
                guard !Task.isCancelled, generation == ticket else { return }
                content = result
            } catch {
                guard !Task.isCancelled, generation == ticket else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }
}

private struct WorkspaceLoadState: View {
    let error: String?
    let title: String
    let retry: () -> Void
    var body: some View {
        if let error {
            ContentUnavailableView {
                Label(title, systemImage: "exclamationmark.circle")
            } description: {
                Text(error)
            } actions: {
                Button("Retry", action: retry)
            }
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

struct WorkspaceTextPreview {
    static let limit = 512 * 1024
    let text: String
    let truncated: Bool
    init(_ source: String) {
        truncated = source.utf8.count > Self.limit
        if truncated {
            var bytes = Data(source.utf8.prefix(Self.limit))
            while String(data: bytes, encoding: .utf8) == nil { bytes.removeLast() }
            text = String(decoding: bytes, as: UTF8.self)
        } else { text = source }
    }
}

struct WorkspaceDocumentView: View {
    let title: String
    let partial: Bool
    let preview: WorkspaceTextPreview

    init(title: String, text: String, partial: Bool) {
        self.title = title
        self.partial = partial
        preview = WorkspaceTextPreview(text)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title).font(Theme.mono(11)).foregroundStyle(Theme.textMuted)
                .lineLimit(3).textSelection(.enabled).padding(12)
            if partial || preview.truncated {
                Text("Preview truncated")
                    .font(Theme.sans(12)).foregroundStyle(Theme.warning).padding(.horizontal, 12)
            }
            if preview.text.isEmpty {
                ContentUnavailableView("Empty file", systemImage: "doc.text")
            } else {
                WorkspaceCodeView(text: preview.text)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = preview.text }
                    .disabled(preview.text.isEmpty)
                    .accessibilityLabel("Copy loaded text")
            }
        }
    }
}

struct WorkspaceCodeView: UIViewRepresentable {
    let text: String
    var readerStyle = false
    @Environment(\.colorScheme) private var colorScheme
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = true
        view.alwaysBounceVertical = true
        view.dataDetectorTypes = []
        view.textContainerInset = UIEdgeInsets(top: 12, left: 8, bottom: 24, right: 8)
        view.accessibilityIdentifier = "workspace-code"
        return view
    }
    func updateUIView(_ view: UITextView, context: Context) {
        let coordinator = context.coordinator
        guard coordinator.text != text || coordinator.scheme != colorScheme || coordinator.readerStyle != readerStyle else { return }
        coordinator.text = text
        coordinator.scheme = colorScheme
        coordinator.readerStyle = readerStyle
        view.backgroundColor = UIColor(Theme.bg)
        view.tintColor = UIColor(Theme.accent)
        view.textContainerInset = UIEdgeInsets(top: 12, left: readerStyle ? 16 : 8, bottom: 24, right: readerStyle ? 16 : 8)
        view.textContainer.lineFragmentPadding = readerStyle ? 0 : 5
        let paragraph = NSMutableParagraphStyle()
        if readerStyle { paragraph.minimumLineHeight = 20; paragraph.maximumLineHeight = 20 }
        let value = NSMutableAttributedString(string: text, attributes: [
            .font: readerStyle ? UIFont.monospacedSystemFont(ofSize: 12, weight: .regular) : Theme.monoUI(12),
            .foregroundColor: UIColor(Theme.text), .paragraphStyle: paragraph,
        ])
        let selection = view.selectedRange
        view.attributedText = value
        if NSMaxRange(selection) <= value.length { view.selectedRange = selection }
    }
    final class Coordinator {
        var text: String?
        var scheme: ColorScheme?
        var readerStyle = false
    }
}

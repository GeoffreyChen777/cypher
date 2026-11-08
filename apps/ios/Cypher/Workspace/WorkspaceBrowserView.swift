// Files / Changes browser sheet for a session's working directory on its host
// device: directory listing, changes list and file preview.

import SwiftUI

/// What the Files / Changes sheet opens on.
enum WorkspaceDestination: Identifiable, Hashable {
    case files, changes
    /// A recorded round's diff, scrolled to `focus` (the whole round when nil).
    case round(TurnChanges, focus: String?)
    /// A file a reply cites or links: found in the checkout, then shown at
    /// its lines.
    case reference(WorkspaceReference)

    /// The two the session menu (and a demo launch's `-sheet`) names.
    init?(rawValue: String) {
        switch rawValue {
        case "files": self = .files
        case "changes": self = .changes
        default: return nil
        }
    }

    var id: String {
        switch self {
        case .files: "files"
        case .changes: "changes"
        case .round(let changes, let focus): "round:\(changes.base):\(changes.head):\(focus ?? "")"
        case .reference(let reference): "reference:\(reference.path):\(reference.lines.map { "\($0)" } ?? "")"
        }
    }

    var title: String {
        switch self {
        case .files: "Files"
        case .changes: "Changes"
        case .round: "Turn changes"
        case .reference(let reference): (reference.expandedPath as NSString).lastPathComponent
        }
    }
}

/// A file a reply cites (a reference chip) or links to.
struct WorkspaceReference: Hashable {
    /// As written: relative to the checkout (or to some folder in it), or
    /// absolute.
    var path: String
    var lines: ClosedRange<Int>? = nil
    /// Written as a folder (a trailing slash).
    var dir = false
    /// The other paths the same reply cites: a bare name is read as the one
    /// longer path among them that ends in it.
    var context: [String] = []

    var expandedPath: String {
        FileReference(path: path, lines: lines).expanded(with: context) ?? path
    }
}

extension EnvironmentValues {
    /// Opens the session's Files / Changes sheet; nil where there is none
    /// (side chats).
    @Entry var openWorkspace: ((WorkspaceDestination) -> Void)? = nil
}

struct WorkspaceBrowserView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var session: WorkspaceBrowserSession
    let destination: WorkspaceDestination
    /// The session's pending comments: a selection in a diff or a file can
    /// be commented on like the transcript's. nil where it can't (not Pi).
    let drafts: CommentDrafts?
    /// Close for the iPad inspector, which isn't dismissed like a sheet.
    let onClose: (() -> Void)?

    init(
        model: AppModel, chat: Chat, destination: WorkspaceDestination, drafts: CommentDrafts? = nil,
        onClose: (() -> Void)? = nil
    ) {
        _session = State(initialValue: WorkspaceBrowserSession(model: model, chat: chat))
        self.destination = destination
        self.drafts = drafts
        self.onClose = onClose
    }

    var body: some View {
        NavigationStack {
            Group {
                switch destination {
                case .files:
                    WorkspaceDirectoryView(session: session, path: "")
                case .changes:
                    WorkspaceChangesView(session: session, scope: .working)
                case .round(let changes, let focus):
                    WorkspaceChangesView(session: session, scope: .round(changes), focus: focus)
                case .reference(let reference):
                    WorkspaceReferenceView(session: session, reference: reference)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Theme.bg)
            .navigationTitle(destination.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        if let onClose { onClose() } else { dismiss() }
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 18, weight: .regular))
                            .frame(width: 22, height: 22)
                    }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityLabel("Close")
                    .accessibilityIdentifier("workspace-close")
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let drafts { PendingCommentsBar(drafts: drafts).background(Theme.bg) }
        }
        // Links in a rendered Markdown file are relative to it, not to the
        // chat: they don't reopen this sheet.
        .environment(\.openWorkspace, nil)
        .environment(\.commentDrafts, drafts)
        // The comment editor shows over this sheet while it is up.
        .sheet(
            isPresented: Binding(
                get: { drafts.map { $0.presented && $0.presenter == .workspace } ?? false },
                set: { drafts?.presented = $0 })
        ) {
            if let drafts { CommentsPanel(drafts: drafts) }
        }
        .onAppear { drafts?.presenter = .workspace }
        .onDisappear { drafts?.presenter = .session }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}

struct WorkspaceDirectoryView: View {
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
    var scope: WorkspaceChangesScope = .working
    /// The file to open the document at.
    var focus: String? = nil
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
                    WorkspaceDiffView(
                        path: "Changes", patch: snapshot.patch, partial: snapshot.truncated,
                        entries: entries,
                        loadSources: snapshot.truncated
                            ? nil
                            : { [scope] path in
                                try await session.diffSources(snapshot: snapshot, path: path, scope: scope)
                            },
                        focus: focus)
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
                let result = try await session.changes(scope)
                let prepared = await Task.detached {
                    let patches = result.patches()
                    var seen = Set<String>()
                    return result.files.compactMap { file -> WorkspaceDiffEntry? in
                        guard seen.insert(file.path).inserted else { return nil }
                        return WorkspaceDiffEntry(
                            path: file.path, patch: patches[file.path],
                            additions: file.additions, deletions: file.deletions, binary: file.binary)
                    }
                }.value
                guard !Task.isCancelled, generation == ticket else { return }
                snapshot = result
                entries = prepared
            } catch {
                guard !Task.isCancelled, generation == ticket else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error, scope: scope)
            }
        }
    }
}

/// A reference or link from the chat: found in the checkout, then opened —
/// the file at its cited lines, a folder's listing, or a choice between the
/// files the reference could mean.
struct WorkspaceReferenceView: View {
    let session: WorkspaceBrowserSession
    let reference: WorkspaceReference
    @State private var resolution: WorkspaceResolution?
    @State private var error: String?
    @State private var revision = 0

    var body: some View {
        Group {
            switch resolution {
            case .file(let path):
                WorkspaceFileView(session: session, path: path, lines: reference.lines, showsFolder: true)
            case .directory(let path):
                WorkspaceDirectoryView(session: session, path: path)
                    .navigationTitle(path.isEmpty ? "Files" : (path as NSString).lastPathComponent)
            case .ambiguous(let paths):
                List {
                    Section {
                        ForEach(paths, id: \.self) { path in
                            NavigationLink {
                                WorkspaceFileView(
                                    session: session, path: path, lines: reference.lines, showsFolder: true)
                            } label: {
                                WorkspacePathLabel(path: path)
                            }
                            .accessibilityIdentifier("workspace-candidate-\(path)")
                        }
                    } header: {
                        Text("Several files end in \(reference.expandedPath)")
                    }
                }
                .listStyle(.insetGrouped)
                .scrollContentBackground(.hidden)
                .navigationTitle(reference.path)
            case .missing(let path):
                ContentUnavailableView(
                    "Not in this checkout", systemImage: "questionmark.folder",
                    description: Text("No file at \(path), and none whose path ends in it."))
            case .outside(let path):
                ContentUnavailableView(
                    "Outside this checkout", systemImage: "folder.badge.minus",
                    description: Text("\(path) isn't in this session's working directory, so Files can't open it."))
            case nil:
                WorkspaceLoadState(error: error, title: "File unavailable") { revision += 1 }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .task(id: revision) {
            error = nil
            do {
                let result = try await session.resolve(reference)
                guard !Task.isCancelled else { return }
                resolution = result
            } catch {
                guard !Task.isCancelled else { return }
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }
}

/// A path as a list row: the file name, its folder under it.
private struct WorkspacePathLabel: View {
    let path: String

    var body: some View {
        let name = (path as NSString).lastPathComponent
        let dir = (path as NSString).deletingLastPathComponent
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(Theme.sans(14)).foregroundStyle(Theme.text)
            if !dir.isEmpty {
                Text(dir).font(Theme.sans(12)).foregroundStyle(Theme.textMuted)
                    .lineLimit(1).truncationMode(.head)
            }
        }
    }
}

struct WorkspaceFileView: View {
    let session: WorkspaceBrowserSession
    let path: String
    /// Cited lines to select and bring into view.
    var lines: ClosedRange<Int>? = nil
    /// Opened from the chat rather than the folder list: a button lists the
    /// file's folder.
    var showsFolder = false
    @State private var content: WorkspaceFileContent?
    @State private var error: String?
    @State private var revision = 0
    @State private var generation = UUID()
    /// Markdown, HTML and SVG open rendered unless lines are cited.
    @State private var source: Bool

    init(session: WorkspaceBrowserSession, path: String, lines: ClosedRange<Int>? = nil, showsFolder: Bool = false) {
        self.session = session
        self.path = path
        self.lines = lines
        self.showsFolder = showsFolder
        _source = State(initialValue: lines != nil)
    }

    private var kind: WorkspaceFileKind { WorkspaceFileKind(path: path) }

    var body: some View {
        Group {
            if kind == .image {
                // Images skip the text read: their bytes come in slices.
                WorkspaceImageView(session: session, path: path, revision: revision)
            } else if let content {
                if let text = content.text, !content.binary {
                    if kind.previewable, !source {
                        WorkspaceRenderedView(kind: kind, text: text, partial: content.truncated)
                    } else {
                        WorkspaceSourceView(path: path, text: text, partial: content.truncated, lines: lines)
                    }
                } else {
                    ContentUnavailableView(
                        "No text preview", systemImage: "doc",
                        description: Text(
                            "This is a binary or non-UTF-8 file (\(ByteCountFormatter.string(fromByteCount: Int64(content.bytes), countStyle: .file)))."
                        ))
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
            if kind.previewable, content?.text != nil {
                ToolbarItem(placement: .primaryAction) {
                    Picker("View", selection: $source) {
                        Label("Preview", systemImage: "eye").tag(false)
                        Label("Source", systemImage: "chevron.left.forwardslash.chevron.right").tag(true)
                    }
                    .pickerStyle(.menu)
                    .accessibilityIdentifier("workspace-preview-toggle")
                }
            }
            if showsFolder {
                ToolbarItem(placement: .primaryAction) {
                    NavigationLink {
                        let folder = (path as NSString).deletingLastPathComponent
                        WorkspaceDirectoryView(session: session, path: folder)
                            .navigationTitle(folder.isEmpty ? "Files" : (folder as NSString).lastPathComponent)
                    } label: {
                        Image(systemName: "folder")
                    }
                    .accessibilityLabel("Show in folder")
                    .accessibilityIdentifier("workspace-show-folder")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button("Refresh", systemImage: "arrow.clockwise") { revision += 1 }
                    .disabled(kind != .image && content == nil && error == nil)
            }
        }
        .task(id: "\(path)/\(revision)") {
            guard kind != .image else { return }
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

struct WorkspaceLoadState: View {
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
        } else {
            text = source
        }
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
    /// Lines to wash and scroll to (a cited reference's), 1-based.
    var highlight: ClosedRange<Int>? = nil
    /// The edit menu's Comment: takes the selected text.
    var comment: ((String) -> Void)? = nil
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
        view.delegate = context.coordinator
        return view
    }
    func updateUIView(_ view: UITextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.comment = comment
        guard coordinator.text != text || coordinator.scheme != colorScheme || coordinator.readerStyle != readerStyle
        else { return }
        coordinator.text = text
        coordinator.scheme = colorScheme
        coordinator.readerStyle = readerStyle
        view.backgroundColor = UIColor(Theme.bg)
        view.tintColor = UIColor(Theme.accent)
        view.textContainerInset = UIEdgeInsets(
            top: 12, left: readerStyle ? 16 : 8, bottom: 24, right: readerStyle ? 16 : 8)
        view.textContainer.lineFragmentPadding = readerStyle ? 0 : 5
        let paragraph = NSMutableParagraphStyle()
        if readerStyle {
            paragraph.minimumLineHeight = 20
            paragraph.maximumLineHeight = 20
        }
        let value = NSMutableAttributedString(
            string: text,
            attributes: [
                .font: readerStyle ? UIFont.monospacedSystemFont(ofSize: 12, weight: .regular) : Theme.monoUI(12),
                .foregroundColor: UIColor(Theme.text), .paragraphStyle: paragraph,
            ])
        let lineRange = highlight.flatMap { Self.characterRange(of: $0, in: text) }
        if let lineRange {
            value.addAttribute(.backgroundColor, value: UIColor(Theme.accent.opacity(0.14)), range: lineRange)
        }
        let selection = view.selectedRange
        view.attributedText = value
        if NSMaxRange(selection) <= value.length { view.selectedRange = selection }
        if let lineRange, !coordinator.revealed {
            coordinator.revealed = true
            DispatchQueue.main.async { [weak view] in
                view?.layoutIfNeeded()
                view?.scrollRangeToVisible(lineRange)
            }
        }
    }

    /// The UTF-16 range lines `lines` (1-based) take in `text`.
    static func characterRange(of lines: ClosedRange<Int>, in text: String) -> NSRange? {
        let string = text as NSString
        var start: Int?
        var line = 1
        var at = 0
        while at <= string.length {
            let range = string.lineRange(for: NSRange(location: min(at, string.length), length: 0))
            if line == lines.lowerBound { start = range.location }
            if line == lines.upperBound || NSMaxRange(range) >= string.length {
                guard let start else { return nil }
                return NSRange(location: start, length: NSMaxRange(range) - start)
            }
            line += 1
            at = NSMaxRange(range)
        }
        return nil
    }
    final class Coordinator: NSObject, UITextViewDelegate {
        var text: String?
        var scheme: ColorScheme?
        var readerStyle = false
        var revealed = false
        var comment: ((String) -> Void)?

        func textView(
            _ textView: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]
        ) -> UIMenu? {
            guard let comment else { return nil }
            let quote = CommentPrompt.selectedText(textView.text ?? "", range: range)
            guard !CommentPrompt.normalize(quote).isEmpty else { return nil }
            let action = UIAction(title: "Comment", image: UIImage(systemName: "text.bubble")) {
                [weak textView] _ in
                textView?.selectedRange = NSRange(location: 0, length: 0)
                comment(quote)
            }
            return UIMenu(children: [action] + suggestedActions)
        }
    }
}

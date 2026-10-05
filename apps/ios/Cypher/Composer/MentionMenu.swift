// The `@` menu — composer.rs render_mention_popup on the phone: sessions
// (filtered locally from the synced registry) above files (the host's
// SearchFiles, relay-forwarded to the device that owns the checkout), as a
// list above the composer like the `/` menu. Picking a row turns the typed
// `@query` into a chip; nothing is sent.

import SwiftUI

/// Where a composer's `@` looks: which chat it's in (never offered), what
/// ranks nearest, and the checkout files come from.
struct MentionScope: Equatable {
    /// SearchFiles' target: a live chat's checkout, or a space (plus the
    /// existing worktree a new session will reuse).
    struct Files: Equatable {
        var deviceId: String
        var chatId: String?
        var spaceId: String?
        var path: String?

        var params: [String: Any] {
            var params: [String: Any] = [:]
            if let chatId { params["chatId"] = chatId }
            if let spaceId { params["spaceId"] = spaceId }
            if let path { params["path"] = path }
            return params
        }
    }

    var currentChat: String?
    var project: String?
    var device: String?
    /// nil: no checkout to search yet (a quick chat before its first send).
    var files: Files?
}

/// One composer's `@` results. A newer query retires older replies (the
/// view's `.task(id:)` cancels them); a file search failure is shown, never
/// passed off as "no matching files".
@MainActor @Observable
final class MentionSearch {
    private(set) var sessions: [MentionSession] = []
    private(set) var files: [FileSearchMatch] = []
    private(set) var loading = false
    private(set) var error: String?
    @ObservationIgnored private var filesScope: MentionScope.Files?

    func run(query: String, scope: MentionScope, chats: [Chat],
             fetch: (MentionScope.Files, String) async throws -> [FileSearchMatch]) async {
        sessions = Mentions.sessionCandidates(chats, query: query, currentChat: scope.currentChat,
                                              project: scope.project, device: scope.device)
        if filesScope != scope.files {
            filesScope = scope.files
            files = []
        }
        guard let target = scope.files else {
            loading = false
            error = nil
            return
        }
        loading = true
        // One search per pause in typing, not per keystroke.
        try? await Task.sleep(nanoseconds: 120_000_000)
        guard !Task.isCancelled else { return }
        do {
            let result = try await fetch(target, String(query.prefix(Mentions.maxQueryChars)))
            guard !Task.isCancelled else { return }
            files = result
            error = nil
        } catch {
            guard !Task.isCancelled else { return }
            files = []
            self.error = Mentions.searchErrorMessage(error)
        }
        loading = false
    }
}

struct MentionMenuView: View {
    let search: MentionSearch
    let query: String
    /// Where a session lives: "Archived · project · device · offline".
    let subtitle: (MentionSession) -> String
    let onPickSession: (MentionSession) -> Void
    let onPickFile: (FileSearchMatch) -> Void
    var maxHeight: CGFloat = SlashMenuView.defaultMaxHeight

    private static let rowInset: CGFloat = 16

    var body: some View {
        Group {
            if search.sessions.isEmpty, search.files.isEmpty {
                if search.loading {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        status("Searching…", color: Theme.textMuted)
                    }
                    .padding(.vertical, 14)
                } else {
                    status(search.error ?? (query.isEmpty ? "No files available" : "No matching sessions or files"),
                           color: search.error == nil ? Theme.textMuted : Theme.danger)
                        .padding(.vertical, 14)
                }
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        if !search.sessions.isEmpty {
                            heading("Sessions")
                            ForEach(search.sessions) { session in
                                sessionRow(session)
                            }
                        }
                        filesSection
                    }
                    .padding(.vertical, 6)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(maxHeight: maxHeight)
                .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity)
        .glassEffect(.regular.tint(Theme.surface.opacity(0.72)),
                     in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityIdentifier("mention-menu")
    }

    /// Files under their heading; with sessions above, a pending search or
    /// its failure is a note here instead of hiding them.
    @ViewBuilder
    private var filesSection: some View {
        if !search.files.isEmpty {
            if !search.sessions.isEmpty { heading("Files") }
            ForEach(search.files, id: \.self) { file in
                fileRow(file)
            }
        } else if search.loading || search.error != nil {
            heading("Files")
            if let error = search.error {
                status(error, color: Theme.danger)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, 8)
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    status("Searching…", color: Theme.textMuted)
                }
                .padding(.horizontal, Self.rowInset)
                .padding(.vertical, 8)
            }
        }
    }

    private func heading(_ title: String) -> some View {
        Text(title.uppercased())
            .font(Theme.sans(11, weight: .medium))
            .tracking(1)
            .foregroundStyle(Theme.textMuted.opacity(0.6))
            .padding(.horizontal, Self.rowInset)
            .padding(.top, 10)
            .padding(.bottom, 4)
            .accessibilityAddTraits(.isHeader)
    }

    private func sessionRow(_ session: MentionSession) -> some View {
        let detail = subtitle(session)
        return row(action: { onPickSession(session) }) {
            LineIconView(.chatRoundLine, size: 16, color: Theme.textMuted)
        } label: {
            Text(session.title)
                .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
            Spacer(minLength: 8)
            if !detail.isEmpty {
                Text(detail)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textFaint)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .layoutPriority(-1)
            }
        }
        .accessibilityLabel(detail.isEmpty ? session.title : "\(session.title), \(detail)")
        .accessibilityIdentifier("mention-session-\(session.chatId)")
    }

    private func fileRow(_ file: FileSearchMatch) -> some View {
        let parts = file.path.split(separator: "/").map(String.init)
        let name = parts.last ?? file.path
        let folder = parts.dropLast().joined(separator: "/")
        return row(action: { onPickFile(file) }) {
            LineIconView(file.isDir ? .folder : .document, size: 16, color: Theme.textMuted)
        } label: {
            Text(file.isDir ? name + "/" : name)
                .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .layoutPriority(1)
            if !folder.isEmpty {
                Text(folder)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textFaint)
                    .lineLimit(1)
                    .truncationMode(.head)
            }
            Spacer(minLength: 0)
        }
        .accessibilityLabel(file.path)
        .accessibilityIdentifier("mention-file-\(file.path)")
    }

    /// The `/` menu's row: the glyph in a fixed slot, then the label.
    private func row<Leading: View, Label: View>(
        action: @escaping () -> Void,
        @ViewBuilder leading: () -> Leading, @ViewBuilder label: () -> Label
    ) -> some View {
        Button {
            UISelectionFeedbackGenerator().selectionChanged()
            action()
        } label: {
            HStack(spacing: 10) {
                Color.clear
                    .frame(width: 18, height: 18)
                    .overlay { leading() }
                HStack(alignment: .firstTextBaseline, spacing: 8) { label() }
            }
            .padding(.horizontal, Self.rowInset)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 12))
        .padding(.horizontal, 4)
    }

    private func status(_ text: String, color: Color) -> some View {
        Text(text)
            .font(Theme.sans(13))
            .foregroundStyle(color)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 16)
    }
}

extension AppModel {
    /// composer.rs `session_row_subtitle`: where a session lives, with
    /// "offline" when its device is (its snapshot then comes from this
    /// phone's synced copy).
    func mentionSubtitle(_ session: MentionSession) -> String {
        var parts: [String] = []
        if session.archived { parts.append("Archived") }
        if let project = session.project, let space = spaces.first(where: { $0.id == project }) {
            parts.append(space.displayName)
        }
        parts.append(deviceName(session.deviceId))
        if !deviceOnline(session.deviceId) { parts.append("offline") }
        return parts.joined(separator: " · ")
    }
}

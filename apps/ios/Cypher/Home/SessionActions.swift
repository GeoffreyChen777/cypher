// Session row actions — the desktop sidebar row's context menu (shell.rs:
// Rename… · Archive · Delete…) on the phone: a long-press menu and swipe
// actions on every session row, plus the rename and delete prompts. Archive
// stays confirmation-free (it's reversible); delete asks first, in the
// desktop's words.

import SwiftUI

/// The prompts one screen is showing. Rows reach it through the
/// environment; the screen hosting the list presents the prompts.
@MainActor @Observable
final class SessionActions {
    /// The prompts' targets are kept apart from their presentation flags:
    /// an alert resets `isPresented` before running the tapped button's
    /// action, so the target must not be cleared by that reset.
    private(set) var renaming: Chat?
    var renamePresented = false
    private(set) var deleting: Chat?
    var deletePresented = false
    /// A partial-cleanup notice after a delete.
    var notice: String?

    func rename(_ chat: Chat) {
        renaming = chat
        renamePresented = true
    }

    func delete(_ chat: Chat) {
        deleting = chat
        deletePresented = true
    }
}

extension View {
    /// Long-press menu + trailing swipes for an active session row. The
    /// full swipe archives, as before.
    func sessionRowActions(_ chat: Chat) -> some View {
        modifier(SessionRowActions(chat: chat, archived: false))
    }

    /// The archived shelf's rows: Unarchive instead of Archive.
    func archivedSessionRowActions(_ chat: Chat) -> some View {
        modifier(SessionRowActions(chat: chat, archived: true))
    }

    /// Presents `actions`' rename and delete prompts. `onDeleted` runs once
    /// the delete is confirmed (e.g. to leave the deleted session's screen).
    func sessionActionPrompts(_ actions: SessionActions,
                              onDeleted: @escaping (Chat) -> Void = { _ in }) -> some View {
        modifier(SessionActionPrompts(actions: actions, onDeleted: onDeleted))
            .environment(actions)
    }
}

private struct SessionRowActions: ViewModifier {
    @Environment(AppModel.self) private var model
    @Environment(SessionActions.self) private var actions: SessionActions?
    let chat: Chat
    let archived: Bool

    func body(content: Content) -> some View {
        content
            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                archiveButton
                if let actions {
                    // Not `.destructive`: that role animates the row away
                    // before the confirmation has been answered.
                    Button {
                        actions.delete(chat)
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                    .tint(Theme.danger)
                }
            }
            .contextMenu {
                if let actions {
                    Button("Rename…", systemImage: "pencil") { actions.rename(chat) }
                }
                archiveButton
                if let actions {
                    Divider()
                    Button("Delete…", systemImage: "trash", role: .destructive) { actions.delete(chat) }
                }
            }
    }

    private var archiveButton: some View {
        Button {
            // withAnimation, not a value-keyed .animation: the row leaves
            // its section and lands in the other — one coordinated List diff.
            withAnimation(Motion.resort) {
                if archived { model.unarchive(chatId: chat.id) } else { model.archive(chatId: chat.id) }
            }
        } label: {
            if archived {
                Label("Unarchive", systemImage: "tray.and.arrow.up")
            } else {
                Label("Archive", systemImage: "archivebox")
            }
        }
        .tint(.gray)
    }
}

private struct SessionActionPrompts: ViewModifier {
    @Environment(AppModel.self) private var model
    @Bindable var actions: SessionActions
    let onDeleted: (Chat) -> Void
    /// The rename field's text, seeded from the title when the alert opens.
    @State private var renameText = ""

    // One alert per host view, so each presentation is independent.
    func body(content: Content) -> some View {
        content
            .background {
                Color.clear
                    .alert("Rename session", isPresented: $actions.renamePresented) {
                        TextField("Title", text: $renameText)
                        Button("Cancel", role: .cancel) {}
                        Button("Rename") {
                            if let chat = actions.renaming {
                                model.renameChat(chatId: chat.id, title: renameText)
                            }
                        }
                    }
                    .onChange(of: actions.renamePresented) { _, presented in
                        if presented { renameText = actions.renaming?.title ?? "" }
                    }
            }
            .background {
                Color.clear
                    .alert("Delete session?", isPresented: $actions.deletePresented,
                           presenting: actions.deleting) { chat in
                        Button("Cancel", role: .cancel) {}
                        Button("Delete", role: .destructive) {
                            UINotificationFeedbackGenerator().notificationOccurred(.warning)
                            onDeleted(chat)
                            Task { @MainActor in
                                actions.notice = await model.deleteChat(chat)
                            }
                        }
                    } message: { chat in
                        Text("“\(chat.displayTitle)” will be permanently deleted. This can’t be undone.")
                    }
            }
            .alert("Session deleted", isPresented: Binding(
                get: { actions.notice != nil },
                set: { if !$0 { actions.notice = nil } }
            )) {
                Button("OK") { actions.notice = nil }
            } message: {
                Text(actions.notice ?? "")
            }
    }
}

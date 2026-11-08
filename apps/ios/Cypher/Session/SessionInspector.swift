// The session's inspector on iPad — the desktop's right dock: Files, Changes
// and the open side chat beside the transcript instead of in sheets over it.
// A regular-width SessionView shows it; compact keeps the sheets.

import SwiftUI

/// What the inspector shows: the Files / Changes browser or the side chat.
enum SessionInspectorPane: Hashable {
    case workspace, sideChat
}

/// The iPad detail column's inspector. The frontmost session hands its
/// panel here and HomeView shows it beside the detail column's stack —
/// outside it, so the panel keeps its own navigation bar (Files drills into
/// folders) and the session keeps its toolbar.
@MainActor
@Observable
final class DetailInspector {
    /// The chat whose panel is up.
    private(set) var owner: String?
    private(set) var content: AnyView?
    @ObservationIgnored private var onHide: (() -> Void)?

    var shown: Bool { content != nil }

    func present(owner: String, content: AnyView, onHide: @escaping () -> Void) {
        self.owner = owner
        self.content = content
        self.onHide = onHide
    }

    /// Take the owner's panel down (it closed, or its session left).
    func withdraw(owner: String) {
        guard self.owner == owner else { return }
        self.owner = nil
        content = nil
        onHide = nil
    }

    /// The system closed the inspector: tell its session.
    func hide() {
        let onHide = onHide
        if let owner { withdraw(owner: owner) }
        onHide?()
    }
}

extension EnvironmentValues {
    /// The split layout's inspector host; nil on the phone.
    @Entry var detailInspector: DetailInspector? = nil
}

struct SessionInspector: View {
    @Environment(AppModel.self) private var model
    let chat: Chat
    /// The browser's destination; nil opens on Changes.
    @Binding var destination: WorkspaceDestination?
    @Binding var pane: SessionInspectorPane
    let sideChat: SideChatStore?
    let drafts: CommentDrafts?
    /// Hide the inspector.
    let close: () -> Void
    /// End the side chat (closing it discards it on the host).
    let closeSideChat: () -> Void
    /// The side chat became a normal chat: open it.
    let promoted: (String) -> Void

    /// Ideal width; the session hides the sidebar when the chat would be
    /// left narrower than `SessionView.minimumChatWidth` beside it.
    static let idealWidth: CGFloat = 360

    private enum Tab: Hashable { case files, changes, sideChat }

    var body: some View {
        VStack(spacing: 0) {
            Picker("Panel", selection: tab) {
                Text("Files").tag(Tab.files)
                Text("Changes").tag(Tab.changes)
                if sideChat != nil {
                    Text("Side Chat").tag(Tab.sideChat)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .accessibilityIdentifier("inspector-tabs")
            content
        }
        .background(Theme.bg.ignoresSafeArea())
        .inspectorColumnWidth(min: 300, ideal: Self.idealWidth, max: 560)
    }

    @ViewBuilder private var content: some View {
        if pane == .sideChat, let sideChat {
            SideChatSheet(store: sideChat, onPromoted: promoted, onClose: closeSideChat)
                .id(ObjectIdentifier(sideChat))
        } else {
            let destination = destination ?? .changes
            WorkspaceBrowserView(
                model: model, chat: chat, destination: destination,
                drafts: drafts, onClose: close
            )
            // A new destination is a new browser, as a new sheet was.
            .id("\(chat.id)/\(chat.deviceId)/\(chat.cwd ?? "")/\(destination.id)")
        }
    }

    /// The segment a destination belongs to: a cited file under Files, a
    /// round's diff under Changes.
    private var tab: Binding<Tab> {
        Binding(
            get: {
                if pane == .sideChat, sideChat != nil { return .sideChat }
                switch destination {
                case .files, .reference: return .files
                case .changes, .round, nil: return .changes
                }
            },
            set: { tab in
                switch tab {
                case .sideChat:
                    pane = .sideChat
                case .files:
                    pane = .workspace
                    if !(destination == .files || destination.map(Self.isReference) == true) {
                        destination = .files
                    }
                case .changes:
                    pane = .workspace
                    if destination == nil || destination == .files || destination.map(Self.isReference) == true {
                        destination = .changes
                    }
                }
            })
    }

    private static func isReference(_ destination: WorkspaceDestination) -> Bool {
        if case .reference = destination { return true }
        return false
    }
}

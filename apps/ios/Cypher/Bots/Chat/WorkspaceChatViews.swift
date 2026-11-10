// The pieces the session screen adds in a workspace chat: its title (the
// workspace's avatar and name), the status row's "2 need you" capsule and
// pause / offline line, the shortcut chips over the composer, the start
// screen a new workspace opens on, and the banner over a hidden thread.

import SwiftUI

/// The navigation title: avatar, name, and "Workspace · <device>".
struct WorkspaceTitle: View {
    let bot: Bot
    let device: String

    var body: some View {
        HStack(spacing: 8) {
            BotAvatar(bot: bot, size: 26)
            VStack(alignment: .leading, spacing: 1) {
                Text(bot.name)
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Text("Workspace · \(device)")
                    .font(Theme.sans(10.5))
                    .foregroundStyle(Theme.textMuted.opacity(0.6))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// The workspace's part of the status row: what needs you, as an amber
/// capsule that opens Needs you, and a quiet line only when something is
/// off (paused, with Resume; its device offline).
struct WorkspaceStatusAccessory: View {
    let bot: Bot
    let needs: Int
    let hostOnline: Bool
    let device: String
    let openNeeds: () -> Void
    let resume: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            if bot.paused {
                HStack(spacing: 6) {
                    Image(systemName: "pause.fill")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Theme.warning)
                    Text("\(bot.name) is paused")
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                    Button("Resume", action: resume)
                        .font(Theme.sans(12, weight: .semibold))
                        .foregroundStyle(Theme.text)
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("workspace-resume")
                }
                .modifier(StatusCapsule())
            } else if !hostOnline {
                HStack(spacing: 6) {
                    Image(systemName: "icloud.slash")
                        .font(.system(size: 10, weight: .semibold))
                    Text("\(device) is offline")
                        .font(Theme.sans(12))
                        .lineLimit(1)
                }
                .foregroundStyle(Theme.textMuted)
                .modifier(StatusCapsule())
            }
            if needs > 0 {
                Button(action: openNeeds) {
                    HStack(spacing: 6) {
                        Circle().fill(Theme.warning).frame(width: 6, height: 6)
                        Text(WorkspaceChatText.needsYou(needs))
                            .font(Theme.sans(12, weight: .medium))
                            .foregroundStyle(Theme.warning)
                            .lineLimit(1)
                            .fixedSize()
                    }
                    .modifier(StatusCapsule(interactive: true))
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityHint("Opens what needs you")
                .accessibilityIdentifier("workspace-needs-you")
            }
        }
    }
}

/// Shortcuts over the composer: each chip sends its request as if typed
/// (the draft stays); one with a blank asks for its text first. Long press
/// moves or deletes it. Only the agent makes shortcuts.
struct ShortcutBar: View {
    let shortcuts: [BotShortcut]
    /// Sends a request; false when it couldn't be queued.
    let send: (String) -> Bool
    let reorder: ([String]) -> Void
    let delete: (BotShortcut) -> Void

    @State private var asking: BotShortcut?
    @State private var input = ""
    /// The chip whose request just went out: its check, briefly.
    @State private var sent: String?
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(shortcuts) { chip($0) }
                }
            }
            .contentMargins(.horizontal, 16, for: .scrollContent)
            .scrollClipDisabled()
            if let failure {
                Text(failure)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.danger)
                    .padding(.horizontal, 20)
                    .onTapGesture { self.failure = nil }
            }
        }
        .alert(
            asking?.label ?? "Shortcut",
            isPresented: Binding(get: { asking != nil }, set: { if !$0 { asking = nil } }),
            presenting: asking
        ) { shortcut in
            TextField("Text for \(shortcut.label)", text: $input)
            Button("Cancel", role: .cancel) {}
            Button("Send") { fire(shortcut, input: input) }
                .disabled(input.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        } message: { shortcut in
            Text(clip(shortcut.request, max: 160))
        }
        .accessibilityIdentifier("workspace-shortcuts")
    }

    private func chip(_ shortcut: BotShortcut) -> some View {
        let ids = shortcuts.map(\.id)
        let done = sent == shortcut.id
        return Button {
            if shortcut.wantsInput {
                input = ""
                asking = shortcut
            } else {
                fire(shortcut, input: "")
            }
        } label: {
            Label {
                Text(shortcut.label).lineLimit(1)
            } icon: {
                Image(systemName: done ? "checkmark" : BotStyle.shortcutSymbol(shortcut.icon))
                    .contentTransition(.symbolEffect(.replace))
            }
            .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
            .foregroundStyle(Theme.text)
        }
        .buttonStyle(.glass)
        .contextMenu {
            if let left = WorkspaceChatText.moved(ids, shortcut.id, by: -1) {
                Button("Move Left", systemImage: "arrow.left") { reorder(left) }
            }
            if let right = WorkspaceChatText.moved(ids, shortcut.id, by: 1) {
                Button("Move Right", systemImage: "arrow.right") { reorder(right) }
            }
            Divider()
            Button("Delete", systemImage: "trash", role: .destructive) { delete(shortcut) }
        }
        .accessibilityHint(shortcut.wantsInput ? "Asks for text, then sends" : "Sends this request")
        .accessibilityIdentifier("shortcut-\(shortcut.id)")
    }

    private func fire(_ shortcut: BotShortcut, input: String) {
        let request = shortcut.wantsInput ? shortcut.request(with: input) : shortcut.request
        guard send(request) else {
            failure = "Couldn't send \(shortcut.label). Please retry."
            return
        }
        failure = nil
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        withAnimation(Motion.fadeQuick) { sent = shortcut.id }
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(1.6))
            if sent == shortcut.id { withAnimation(Motion.fadeQuick) { sent = nil } }
        }
    }
}

/// A new workspace's start screen: who it is, what it can see, and
/// questions to start with (a tap asks one).
struct WorkspaceStartView: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    let bot: Bot
    let device: String
    let access: String
    let suggestions: [(symbol: String, text: String)]
    let ask: (String) -> Void
    let openSettings: () -> Void

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                BotAvatar(bot: bot, size: 64)
                Text("Ask \(bot.name)")
                    .font(Theme.sans(22, weight: .semibold, relativeTo: .title2))
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.center)
                    .padding(.top, 14)
                Text("\(WorkspaceChatText.role(bot.capabilities)) · runs on \(device)")
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .multilineTextAlignment(.center)
                    .padding(.top, 4)
                Label(access, systemImage: "checkmark.shield")
                    .font(Theme.sans(12.5, relativeTo: .footnote))
                    .foregroundStyle(Theme.textFaint)
                    .multilineTextAlignment(.center)
                    .padding(.top, 8)
                    .accessibilityIdentifier("workspace-access")
                if WorkspaceChatText.hasAccess(bot) {
                    // Two columns, one at the largest text sizes.
                    let columns = Array(
                        repeating: GridItem(.flexible(), spacing: 8), count: typeSize.isAccessibilitySize ? 1 : 2)
                    LazyVGrid(columns: columns, spacing: 8) {
                        ForEach(suggestions, id: \.text) { suggestion in
                            Button {
                                ask(suggestion.text)
                            } label: {
                                VStack(alignment: .leading, spacing: 8) {
                                    Image(systemName: suggestion.symbol)
                                        .font(.system(size: 14, weight: .medium))
                                        .foregroundStyle(Theme.textMuted)
                                    Spacer(minLength: 0)
                                    Text(suggestion.text)
                                        .font(Theme.sans(13.5, relativeTo: .subheadline))
                                        .foregroundStyle(Theme.text)
                                        .multilineTextAlignment(.leading)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                .frame(maxWidth: .infinity, minHeight: 84, alignment: .topLeading)
                                .padding(12)
                                .background(
                                    Theme.text.opacity(0.045),
                                    in: RoundedRectangle(cornerRadius: 14, style: .continuous)
                                )
                                .contentShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                            }
                            .buttonStyle(PressWashButtonStyle(cornerRadius: 14))
                            .accessibilityIdentifier("workspace-suggestion")
                        }
                    }
                    .padding(.top, 28)
                } else {
                    Text("It can't see any of your sessions yet.")
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textMuted)
                        .padding(.top, 20)
                    Button("Choose what it can see", action: openSettings)
                        .buttonStyle(.glass)
                        .padding(.top, 12)
                }
            }
            .frame(maxWidth: 520)
            .padding(.horizontal, 20)
            .padding(.vertical, 32)
            .frame(maxWidth: .infinity)
        }
        .scrollBounceBehavior(.basedOnSize)
        // Centred while it fits; from its top when large text overflows.
        .defaultScrollAnchor(.center, for: .alignment)
        .defaultScrollAnchor(.top, for: .initialOffset)
        .accessibilityIdentifier("workspace-start")
    }
}

/// The banner over a hidden thread: what it belongs to, opening that
/// item's page. The thread itself is read-only.
struct ThreadBanner: View {
    let origin: ThreadOrigin
    let open: (() -> Void)?

    var body: some View {
        Button {
            open?()
        } label: {
            HStack(spacing: 8) {
                Image(systemName: origin.symbol)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                Text(origin.text)
                    .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if open != nil {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                }
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .glassEffect(open != nil ? .regular.interactive() : .regular, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(open == nil)
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .accessibilityHint(open != nil ? "Opens what this session belongs to" : "")
        .accessibilityIdentifier("thread-banner")
    }
}

/// The read-only foot of a hidden thread, where the composer would be.
struct ThreadReadOnlyNote: View {
    var body: some View {
        Label("Only the workspace writes here.", systemImage: "lock")
            .font(Theme.sans(12, relativeTo: .footnote))
            .foregroundStyle(Theme.textFaint)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .accessibilityIdentifier("thread-read-only")
    }
}

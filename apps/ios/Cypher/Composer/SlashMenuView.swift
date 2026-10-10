// Slash menu popup and its mode badges (composer.rs render_slash_popup and
// slash_badge); the menu's state and filtering live in SlashCommands.swift.

import SwiftUI

/// The popup over the composer: commands under their group's heading, each
/// with its glyph, what it does and, where the menu knows, what it controls;
/// or one command's choices under what is in effect, the current one checked.
struct SlashMenuView: View {
    let catalog: RemoteCommandCatalog
    let level: SlashLevel
    let facts: SlashFacts
    let onPickCommand: (SlashCommand) -> Void
    let onPickChoice: (SlashChoice) -> Void
    let onRetry: () -> Void
    /// The tallest the list may grow before it scrolls; the session passes
    /// what's left above the composer (the keyboard takes the rest).
    var maxHeight: CGFloat = SlashMenuView.defaultMaxHeight

    static let defaultMaxHeight: CGFloat = 320

    var body: some View {
        Group {
            switch level {
            case .commands(let query):
                commandList(query: query)
            case .choices(let command, let query):
                choiceList(command: command, query: query)
            }
        }
        .frame(maxWidth: .infinity)
        .glassEffect(
            .regular.tint(Theme.surface.opacity(0.72)),
            in: RoundedRectangle(cornerRadius: 20, style: .continuous)
        )
        .accessibilityIdentifier("slash-menu")
    }

    // MARK: Levels

    @ViewBuilder
    private func commandList(query: String) -> some View {
        let sections = SlashMenu.sections(catalog.commands, query: query)
        if catalog.loading {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                status("Loading commands…")
            }
            .padding(.vertical, 14)
        } else if let error = catalog.error {
            Button(action: onRetry) {
                VStack(spacing: 4) {
                    status(error)
                    Text("Tap to retry")
                        .font(Theme.sans(12, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                }
                .padding(.vertical, 12)
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.plain)
        } else if sections.isEmpty {
            status(emptyMessage(query: query))
                .padding(.vertical, 14)
        } else {
            // The chevron has a column of its own on every row once any
            // command has choices, so the badges end at one edge.
            let chevronColumn = sections.contains { section in
                section.commands.contains { !SlashMenu.choices(for: $0.name).isEmpty }
            }
            scrolling {
                ForEach(sections, id: \.group) { section in
                    heading(section.group.title)
                    ForEach(section.commands, id: \.name) { command in
                        commandRow(command, chevronColumn: chevronColumn)
                    }
                }
            }
        }
    }

    private func choiceList(command: String, query: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text("/\(command)")
                    .font(Theme.sans(13, weight: .medium))
                    .foregroundStyle(Theme.text)
                if let summary = SlashMenu.choiceSummary(of: command, facts: facts) {
                    Text(summary)
                        .font(Theme.sans(13))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
            }
            .padding(.horizontal, Self.rowInset)
            .padding(.top, 12)
            .padding(.bottom, 8)
            Rectangle().fill(Theme.border).frame(height: 1)
            scrolling {
                ForEach(SlashMenu.choiceRows(command: command, query: query), id: \.value) { choice in
                    choiceRow(choice, of: command)
                }
            }
        }
    }

    private func scrolling<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0, content: content)
                .padding(.vertical, 6)
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(maxHeight: maxHeight)
        .fixedSize(horizontal: false, vertical: true)
    }

    private func emptyMessage(query: String) -> String {
        if catalog.commands.isEmpty { return "This agent has no slash commands" }
        return query.isEmpty ? "Type a command's name to find it" : "No matching commands"
    }

    // MARK: Rows

    private static let rowInset: CGFloat = 16

    /// popover.rs `menu_heading`: small, tracked capitals, a shade under the
    /// muted text.
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

    private func commandRow(_ command: SlashCommand, chevronColumn: Bool) -> some View {
        let badge = SlashMenu.badge(for: command.name, facts: facts)
        let hasChoices = !SlashMenu.choices(for: command.name).isEmpty
        return row(title: "/\(command.name)", hint: command.detail, action: { onPickCommand(command) }) {
            LineIconView(SlashMenu.icon(for: command.name), size: 16, color: Theme.textMuted)
        } trailing: {
            if let badge { SlashBadgeView(badge: badge) }
            if chevronColumn {
                Group {
                    if hasChoices {
                        LineIconView(.chevronRight, size: 13, color: Theme.textMuted.opacity(0.7))
                    }
                }
                .frame(width: 13)
            }
        }
        .accessibilityIdentifier("slash-command-\(command.name)")
    }

    private func choiceRow(_ choice: SlashChoice, of command: String) -> some View {
        let inEffect = SlashMenu.choiceInEffect(choice, of: command, facts: facts)
        return row(title: choice.value, hint: choice.description, action: { onPickChoice(choice) }) {
            // The check marks the choice in effect; the slot keeps every
            // value aligned either way.
            if inEffect {
                StatusGlyph(data: "M3.5 8.5l3 3 6-7")
                    .stroke(
                        Theme.success,
                        style: StrokeStyle(
                            lineWidth: 1.6 * 13 / 16,
                            lineCap: .round, lineJoin: .round)
                    )
                    .frame(width: 13, height: 13)
            }
        } trailing: {
            EmptyView()
        }
        .accessibilityValue(inEffect ? "In effect" : "")
        .accessibilityIdentifier("slash-choice-\(choice.value)")
    }

    /// A one-line row: the glyph in a fixed slot, the name, then the
    /// trailing badge and chevron. What it does is left to VoiceOver's hint.
    private func row<Leading: View, Trailing: View>(
        title: String, hint: String?, action: @escaping () -> Void,
        @ViewBuilder leading: () -> Leading, @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        Button {
            UISelectionFeedbackGenerator().selectionChanged()
            action()
        } label: {
            HStack(spacing: 10) {
                // The slot holds its width even when empty (an unchecked
                // choice), so every name starts at one edge.
                Color.clear
                    .frame(width: 18, height: 18)
                    .overlay { leading() }
                Text(title)
                    .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                Spacer(minLength: 8)
                trailing()
            }
            .padding(.horizontal, Self.rowInset)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 12))
        .accessibilityHint(hint ?? "")
        .padding(.horizontal, 4)
    }

    private func status(_ text: String) -> some View {
        Text(text)
            .font(Theme.sans(13))
            .foregroundStyle(Theme.textMuted)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 16)
    }
}

/// composer.rs `slash_badge`: green when something is on or running, amber
/// for a reading worth acting on, quiet otherwise. "On" and "Off" share a
/// width, so flipping one doesn't shift it.
struct SlashBadgeView: View {
    let badge: SlashBadge

    var body: some View {
        Text(badge.label)
            .font(Theme.sans(11.5, weight: .medium))
            .foregroundStyle(foreground)
            .lineLimit(1)
            .padding(.horizontal, 7)
            .frame(minWidth: 34, minHeight: 21)
            .background(background, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private var foreground: Color {
        switch badge.tone {
        case .on: return Theme.success
        case .warning: return Theme.warning
        case .neutral: return Theme.textMuted
        case .off: return Theme.textMuted.opacity(0.75)
        }
    }

    private var background: Color {
        switch badge.tone {
        case .on: return Theme.success.opacity(0.14)
        case .warning: return Theme.warning.opacity(0.16)
        case .neutral: return whiteAlpha(0.06)
        case .off: return whiteAlpha(0.04)
        }
    }
}

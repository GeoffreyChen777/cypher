// The pieces every workspace page is built from, so the pages share one
// layout (docs/design/agent-bots.md §13.1 "Every panel page shares one
// layout"): a header with the item's glyph and title, label-and-value facts,
// status as coloured words with a small glyph (never a badge), decisions in
// a bar pinned to the foot with the primary button last, and a quiet empty
// state.

import SwiftUI

/// One fact row: the label left and muted, the value right, wrapping onto
/// more lines when long. Sits in an inset-grouped List section.
struct BotFact<Value: View>: View {
    let label: String
    @ViewBuilder var value: Value

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(label)
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.textMuted)
                .layoutPriority(1)
            Spacer(minLength: 0)
            value
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .multilineTextAlignment(.trailing)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .accessibilityElement(children: .combine)
    }
}

extension BotFact where Value == Text {
    init(label: String, value: String) {
        self.label = label
        self.value = Text(value)
    }
}

extension BotFact where Value == BotStatusText {
    init(label: String = "Status", status: BotStatusLabel) {
        self.label = label
        self.value = BotStatusText(status)
    }
}

/// A state as coloured words with a small glyph (bot pages never badge).
struct BotStatusText: View {
    let status: BotStatusLabel
    var size: CGFloat = 15

    init(_ status: BotStatusLabel, size: CGFloat = 15) {
        self.status = status
        self.size = size
    }

    init(_ text: String, tone: BotTone, size: CGFloat = 15) {
        self.init(BotStatusLabel(text, tone), size: size)
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            if let symbol = status.symbol ?? status.tone.symbol {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.8, weight: .semibold))
                    .accessibilityHidden(true)
            }
            Text(status.text)
                .font(Theme.sans(size, weight: .medium, relativeTo: .body))
        }
        .foregroundStyle(status.tone.color)
    }
}

/// A page's head: the item's glyph and its title, no subtitle. Actions on
/// the item go in the toolbar, not here.
struct BotPageHeader<Leading: View>: View {
    let title: String
    /// Lines the title may take; a question or an action reads in full.
    var lines: Int? = 3
    @ViewBuilder var leading: Leading

    init(title: String, lines: Int? = 3, @ViewBuilder leading: () -> Leading) {
        self.title = title
        self.lines = lines
        self.leading = leading()
    }

    var body: some View {
        HStack(alignment: lines == 3 ? .center : .firstTextBaseline, spacing: 12) {
            leading
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 7 }
            Text(title)
                .font(Theme.sans(lines == 3 ? 22 : 19, weight: .semibold, relativeTo: .title2))
                .foregroundStyle(Theme.text)
                .lineLimit(lines)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
    }
}

extension BotPageHeader where Leading == BotAvatar {
    /// A workspace's own page.
    init(bot: Bot, title: String? = nil) {
        self.title = title ?? bot.name
        self.leading = BotAvatar(bot: bot, size: 36)
    }
}

extension BotPageHeader where Leading == BotHeaderGlyph {
    /// An item's page: its SF Symbol on a tinted tile.
    init(title: String, symbol: String, tone: BotTone = .muted, lines: Int? = 3) {
        self.title = title
        self.lines = lines
        self.leading = BotHeaderGlyph(symbol: symbol, tint: tone == .muted ? Theme.textMuted : tone.color)
    }
}

/// The glyph tile a page header leads with.
struct BotHeaderGlyph: View {
    let symbol: String
    var tint: Color = Theme.textMuted

    var body: some View {
        RoundedRectangle(cornerRadius: 10, style: .continuous)
            .fill(tint.opacity(0.14))
            .frame(width: 36, height: 36)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(tint)
            }
            .accessibilityHidden(true)
    }
}

/// One button of a `BotDecisionBar`.
struct BotDecision: Identifiable {
    enum Role {
        /// The page's main decision (Approve, Send, Save): filled.
        case primary
        case secondary
        /// Stops, cancels or declines something in progress.
        case destructive
    }

    let title: String
    var role: Role = .secondary
    var enabled = true
    let action: () -> Void

    var id: String { title }
}

/// Decisions pinned to the page's foot: up to three buttons, the primary
/// last, over at most one short note. Attach with
/// `.safeAreaInset(edge: .bottom) { BotDecisionBar(…) }`.
struct BotDecisionBar: View {
    var note: String?
    let decisions: [BotDecision]
    /// A decision is on its way: buttons disable and the primary spins.
    var busy = false

    var body: some View {
        VStack(spacing: 10) {
            if let note {
                Text(note)
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
            }
            HStack(spacing: 10) {
                ForEach(decisions.prefix(3)) { decision in
                    button(decision)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 8)
        .background(.bar)
        .overlay(alignment: .top) {
            Rectangle().fill(Theme.border).frame(height: 0.5)
        }
    }

    private func button(_ decision: BotDecision) -> some View {
        Button(action: decision.action) {
            ZStack {
                Text(decision.title).opacity(busy && decision.role == .primary ? 0 : 1)
                if busy && decision.role == .primary {
                    ProgressView().tint(Theme.bg)
                }
            }
            .font(Theme.sans(16, weight: .semibold, relativeTo: .body))
            .frame(maxWidth: .infinity)
            .frame(height: 46)
            .foregroundStyle(foreground(decision))
            .background(background(decision), in: Capsule())
        }
        .buttonStyle(.plain)
        .disabled(busy || !decision.enabled)
        .opacity(decision.enabled ? 1 : 0.5)
        .accessibilityIdentifier("decision-\(decision.title.lowercased())")
    }

    private func foreground(_ decision: BotDecision) -> Color {
        switch decision.role {
        case .primary: Theme.bg
        case .secondary: Theme.text
        case .destructive: Theme.danger
        }
    }

    private func background(_ decision: BotDecision) -> Color {
        decision.role == .primary ? Theme.text : Theme.text.opacity(0.08)
    }
}

/// A page with nothing to list: a quiet glyph, a short line, a hint, and an
/// action where one helps.
struct BotEmptyState: View {
    let symbol: String
    let title: String
    var hint: String?
    var action: (title: String, perform: () -> Void)?

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            if let hint { Text(hint) }
        } actions: {
            if let action {
                Button(action.title, action: action.perform)
            }
        }
    }
}

#Preview("Kit") {
    NavigationStack {
        List {
            Section {
                BotPageHeader(title: "Fix invoice rounding in CSV exports", symbol: "list.number", tone: .waiting)
            }
            Section {
                BotFact(status: BotStatusLabel("Awaiting your approval · 3 steps", .waiting))
                BotFact(label: "Project", value: "cypher · MacBook Pro")
                BotFact(
                    label: "Request",
                    value: "The CSV export and the web app disagree on invoice totals again. Find out why.")
                BotFact(label: "Last run") { BotStatusText("Failed", tone: .failed) }
                BotFact(label: "Ran", value: BotTime.past(nowMs() - 3 * 3_600_000))
            }
        }
        .listStyle(.insetGrouped)
        .safeAreaInset(edge: .bottom) {
            BotDecisionBar(
                note: "Nothing runs until you approve.",
                decisions: [
                    BotDecision(title: "Cancel", role: .destructive) {},
                    BotDecision(title: "Edit") {},
                    BotDecision(title: "Approve", role: .primary) {},
                ])
        }
    }
}

#Preview("Empty") {
    BotEmptyState(
        symbol: "clock.arrow.circlepath", title: "No routines",
        hint: "Ask Atlas in the chat to run something on a schedule.",
        action: ("New routine", {}))
}

// The card a Needs you item stands in (needs.rs `render_needs_page`), on
// the Needs you page and at the head of an item's own page, drawn the way
// iOS draws an inset-grouped section: a borderless card on a grouped page,
// one continuous corner (`Theme.cardRadius`) with every plate inside it
// concentric, a head (the item's tinted symbol, where it comes from, when),
// its title, what it says, and its decisions as system glass buttons under
// a little space — no hairline. Nothing inside draws a card of its own.

import SwiftUI

enum NeedsCardMetrics {
    /// A card's padding, and the gap between cards.
    static let pad: CGFloat = Theme.cardPad
    static let gap: CGFloat = Theme.cardGap
    /// Between a card's text and its buttons.
    static let actionsGap: CGFloat = 16
    /// Between two buttons of a card.
    static let buttonGap: CGFloat = 10
    /// The page's cards stop growing past this width (an iPad's full width).
    static let maxWidth: CGFloat = 640
}

extension View {
    /// A card's frame: its padding, the grouped card's paint and its one
    /// continuous corner. No border: the card stands off the grouped page
    /// by its paint, as an inset-grouped section does.
    func needsCard(padding: CGFloat = NeedsCardMetrics.pad) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.groupedCard, in: .rect(cornerRadius: Theme.cardRadius, style: .continuous))
    }

    /// A plate inside a card (a command, a list of facts): the grouped
    /// inset paint, its corner concentric with the card's.
    func needsCardPlate(padding: CGFloat = 12) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.groupedInset, in: .rect(cornerRadius: Theme.cardInsetRadius, style: .continuous))
    }

    /// A card's system button: glass (prominent for the main decision),
    /// a capsule, the cards' one control size, in the app's ink.
    func needsCardButtonStyle(prominent: Bool) -> some View {
        Group {
            if prominent {
                self.buttonStyle(.glassProminent)
            } else {
                self.buttonStyle(.glass)
            }
        }
        .buttonBorderShape(.capsule)
        .controlSize(.regular)
        .tint(Theme.text)
    }
}

/// A page of cards (the Needs you page, an approval's, a session's
/// question): scrolling, on the grouped page paint, its title inline like
/// every board page's.
struct BotCardPage<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: NeedsCardMetrics.gap) {
                content
            }
            .padding(.horizontal, 16)
            .padding(.top, BotPageMetrics.topMargin + 8)
            .padding(.bottom, 20)
            .frame(maxWidth: NeedsCardMetrics.maxWidth)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Theme.groupedBackground.ignoresSafeArea())
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// A titled card on an item's page ("From", "Latest in the session"): the
/// title over the card as an inset-grouped section's header, lined up with
/// the card's text.
struct BotCardSection<Content: View>: View {
    let title: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
                .padding(.horizontal, NeedsCardMetrics.pad)
                .accessibilityAddTraits(.isHeader)
            content
        }
    }
}

/// What a tap on a card's head opens.
enum NeedsCardLink {
    case page(BotPage)
    case perform(() -> Void)
}

/// One item's card: the head, the title, `content`, and `actions` under a
/// little space.
struct NeedsYouCard<Content: View, Actions: View>: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    let symbol: String
    var tint: Color = Theme.warning
    let source: String
    var at: Int64?
    let title: String
    /// Lines the title may take; nil on the item's own page.
    var titleLines: Int? = 3
    var link: NeedsCardLink?
    var linkId: String?
    @ViewBuilder var content: Content
    @ViewBuilder var actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            head
            if Content.self != EmptyView.self {
                content
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, 8)
            }
            if Actions.self != EmptyView.self {
                NeedsCardActions { actions }
            }
        }
        .needsCard()
    }

    @ViewBuilder
    private var head: some View {
        switch link {
        case .page(let page)?:
            NavigationLink(value: page) { headLabel(opens: true) }
                .buttonStyle(.plain)
                .accessibilityIdentifier(linkId ?? "")
        case .perform(let perform)?:
            Button(action: perform) { headLabel(opens: true) }
                .buttonStyle(.plain)
                .accessibilityIdentifier(linkId ?? "")
        case nil:
            headLabel(opens: false)
        }
    }

    private func headLabel(opens: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: symbol)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(tint)
                    .accessibilityHidden(true)
                Text(source)
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                Spacer(minLength: 6)
                if let at {
                    Text(BotTime.ago(at))
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                        .monospacedDigit()
                        .fixedSize()
                }
                if opens {
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Theme.textFaint)
                        .accessibilityHidden(true)
                }
            }
            Text(title)
                .font(Theme.sans(17, weight: .semibold, relativeTo: .headline))
                .foregroundStyle(Theme.text)
                .lineLimit(titleLines.map { typeSize.isAccessibilitySize ? $0 + 2 : $0 })
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .contentShape(Rectangle())
    }
}

extension NeedsYouCard where Actions == EmptyView {
    init(
        symbol: String, tint: Color = Theme.warning, source: String, at: Int64? = nil, title: String,
        titleLines: Int? = 3, link: NeedsCardLink? = nil, linkId: String? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            symbol: symbol, tint: tint, source: source, at: at, title: title, titleLines: titleLines, link: link,
            linkId: linkId, content: content, actions: { EmptyView() })
    }
}

/// A card's foot: its buttons, set off by space alone.
struct NeedsCardActions<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, NeedsCardMetrics.actionsGap)
    }
}

/// A card's copy under its title (needs.rs `card_copy`).
struct NeedsCardCopy: View {
    let text: String
    var lines: Int? = 4

    var body: some View {
        Text(text)
            .font(Theme.sans(15, relativeTo: .subheadline))
            .foregroundStyle(Theme.textMuted)
            .lineLimit(lines)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// What a card's button is for: its weight and its label's colour.
enum NeedsCardButtonKind {
    /// The card's main decision: prominent glass.
    case primary
    case secondary
    /// A refusal: plain glass, its label red.
    case destructive

    var foreground: Color {
        switch self {
        case .primary: Theme.bg
        case .secondary: Theme.text
        case .destructive: Theme.danger
        }
    }
}

/// A card button's label: the title in the card's button type, filling
/// what the row gives it, a spinner in its place while busy.
struct NeedsCardButtonLabel: View {
    let title: String
    var kind = NeedsCardButtonKind.secondary
    var busy = false

    var body: some View {
        ZStack {
            Text(title)
                .multilineTextAlignment(.center)
                .opacity(busy ? 0 : 1)
            if busy {
                ProgressView().controlSize(.small).tint(kind.foreground)
            }
        }
        .font(Theme.sans(15, weight: .semibold, relativeTo: .subheadline))
        .foregroundStyle(kind.foreground)
        .frame(maxWidth: .infinity)
    }
}

/// A card's button (a decision, an answer to pick): a system glass button.
struct NeedsCardButton: View {
    let title: String
    var kind = NeedsCardButtonKind.secondary
    var busy = false
    let action: () -> Void

    var body: some View {
        Button(role: kind == .destructive ? .destructive : nil, action: action) {
            NeedsCardButtonLabel(title: title, kind: kind, busy: busy)
        }
        .needsCardButtonStyle(prominent: kind == .primary)
        .accessibilityIdentifier("card-\(title.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

/// A card's buttons side by side, each as wide as the widest, filling the
/// row; one under another (the main one first) when one line can't hold
/// them at that width (a large text size, long labels).
struct NeedsCardButtonRow<Content: View, Stacked: View>: View {
    var content: Content
    /// The stacked fallback, its main button first.
    var stacked: Stacked

    init(@ViewBuilder content: () -> Content, @ViewBuilder stacked: () -> Stacked) {
        self.content = content()
        self.stacked = stacked()
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            EqualWidthRow(spacing: NeedsCardMetrics.buttonGap) { content.lineLimit(1) }
            VStack(spacing: NeedsCardMetrics.buttonGap) { stacked }
        }
    }
}

/// Lays its views out on one line at one width — the widest one's ideal,
/// or more to fill the row — so a row of buttons reads as one control.
struct EqualWidthRow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard !subviews.isEmpty else { return .zero }
        let widest = subviews.map { $0.sizeThatFits(.unspecified).width }.max() ?? 0
        let needed = widest * CGFloat(subviews.count) + spacing * CGFloat(subviews.count - 1)
        let width = max(needed, proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? needed)
        let column = column(width, subviews.count)
        let height = subviews.map { $0.sizeThatFits(ProposedViewSize(width: column, height: nil)).height }.max() ?? 0
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let column = column(bounds.width, subviews.count)
        var x = bounds.minX
        for subview in subviews {
            subview.place(
                at: CGPoint(x: x, y: bounds.minY), anchor: .topLeading,
                proposal: ProposedViewSize(width: column, height: bounds.height))
            x += column + spacing
        }
    }

    private func column(_ width: CGFloat, _ count: Int) -> CGFloat {
        max(0, (width - spacing * CGFloat(count - 1)) / CGFloat(count))
    }
}

extension NeedsCardButtonRow where Stacked == Content {
    /// The same buttons in the same order either way.
    init(@ViewBuilder content: () -> Content) {
        self.content = content()
        self.stacked = content()
    }
}

/// A row of a card's decisions: the refusals first, the approvals after
/// with the primary one last, as one row of equal buttons; stacked, the
/// primary first, when one line can't hold them.
struct NeedsCardDecisions: View {
    let refusals: [BotDecision]
    let approvals: [BotDecision]
    var busy = false

    var body: some View {
        NeedsCardButtonRow {
            ForEach(refusals + approvals) { button($0) }
        } stacked: {
            ForEach(approvals.reversed() + refusals) { button($0) }
        }
        .disabled(busy)
    }

    private func button(_ decision: BotDecision) -> some View {
        NeedsCardButton(
            title: decision.title,
            kind: decision.role == .primary ? .primary : decision.role == .destructive ? .destructive : .secondary,
            busy: busy && decision.role == .primary, action: decision.action)
    }
}

/// A card's way into another page ("Review the answer"): a plain glass
/// button across the card — it opens something, it doesn't decide.
struct NeedsCardLinkButton: View {
    let title: String
    let page: BotPage

    var body: some View {
        NavigationLink(value: page) {
            NeedsCardButtonLabel(title: title)
        }
        .needsCardButtonStyle(prominent: false)
        .accessibilityIdentifier("card-\(title.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

/// A quiet line in place of a card's buttons ("Answer sent.").
struct NeedsCardNote: View {
    let text: String

    var body: some View {
        Text(text)
            .font(Theme.sans(13, relativeTo: .footnote))
            .foregroundStyle(Theme.textMuted)
            .fixedSize(horizontal: false, vertical: true)
    }
}

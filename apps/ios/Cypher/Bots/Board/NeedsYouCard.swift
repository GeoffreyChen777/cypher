// The card a Needs you item stands in (needs.rs `render_needs_page`), on
// the Needs you page and at the head of an item's own page: one bordered
// card on the page's surface with the theme's one panel radius, a head
// (the item's mark, where it comes from, when), its title, what it says,
// and its decisions under a hairline, refusals at the left and approvals at
// the right. Nothing inside draws a card of its own.

import SwiftUI

enum NeedsCardMetrics {
    /// A card's padding, and the gap between cards (needs.rs `CARD_PAD`).
    static let pad: CGFloat = 14
    static let gap: CGFloat = 10
    /// The page's cards stop growing past this width (an iPad's full width).
    static let maxWidth: CGFloat = 640
}

extension View {
    /// A card's frame: its padding, the grouped row's paint, a hairline
    /// border and the theme's panel radius.
    func needsCard(padding: CGFloat = NeedsCardMetrics.pad) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.groupedRow, in: RoundedRectangle(cornerRadius: Theme.panelRadius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.panelRadius, style: .continuous)
                    .strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A page of cards (the Needs you page, an approval's, a session's
/// question): scrolling, on the app's surface, its title inline like every
/// board page's.
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
        .background(Theme.surface.ignoresSafeArea())
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// A label over a group of cards on an item's page ("From", "Latest in
/// the session").
struct BotCardLabel: View {
    let title: String

    var body: some View {
        Text(title)
            .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
            .foregroundStyle(Theme.textMuted)
            .padding(.horizontal, 4)
            .padding(.top, 10)
            .accessibilityAddTraits(.isHeader)
    }
}

/// What a tap on a card's head opens.
enum NeedsCardLink {
    case page(BotPage)
    case perform(() -> Void)
}

/// One item's card: the head, the title, `content`, and `actions` under a
/// hairline.
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
                        .font(Theme.sans(12, relativeTo: .caption))
                        .foregroundStyle(Theme.textFaint)
                        .monospacedDigit()
                        .fixedSize()
                }
                if opens {
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(Theme.textFaint)
                        .accessibilityHidden(true)
                }
            }
            Text(title)
                .font(Theme.sans(16, weight: .medium, relativeTo: .body))
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

/// A card's foot: its buttons under a hairline.
struct NeedsCardActions<Content: View>: View {
    @Environment(\.displayScale) private var displayScale
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Rectangle().fill(Theme.border).frame(height: 1 / displayScale)
            content
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 10)
        }
        .padding(.top, 12)
    }
}

/// A card's copy under its title (needs.rs `card_copy`).
struct NeedsCardCopy: View {
    let text: String
    var lines: Int? = 4

    var body: some View {
        Text(text)
            .font(Theme.sans(14, relativeTo: .subheadline))
            .foregroundStyle(Theme.textMuted)
            .lineLimit(lines)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A row of a card's decisions: the refusals at the left, the approvals at
/// the right with the primary one last; stacked when one line can't hold
/// them (a large text size).
struct NeedsCardDecisions: View {
    let refusals: [BotDecision]
    let approvals: [BotDecision]
    var busy = false

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                ForEach(refusals) { button($0) }
                Spacer(minLength: 8)
                ForEach(approvals) { button($0) }
            }
            VStack(alignment: .trailing, spacing: 8) {
                ForEach(approvals.reversed() + refusals) { button($0) }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .disabled(busy)
    }

    private func button(_ decision: BotDecision) -> some View {
        BotCardButton(
            title: decision.title,
            role: decision.role == .primary ? .primary : decision.role == .destructive ? .destructive : .secondary,
            busy: busy && decision.role == .primary, action: decision.action)
    }
}

/// A card's link styled as its primary button ("Review the answer").
struct NeedsCardLinkButton: View {
    let title: String
    let page: BotPage

    var body: some View {
        NavigationLink(value: page) {
            Text(title)
                .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
                .foregroundStyle(Theme.bg)
                .padding(.horizontal, 16)
                .padding(.vertical, 6)
                .frame(minHeight: 34)
                .background(Theme.text, in: Capsule())
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
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

/// An option to pick, outlined (a question's answer, a broader approval).
struct NeedsCardOutlineButton: View {
    let title: String
    var fill = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                .foregroundStyle(Theme.text)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .frame(maxWidth: fill ? .infinity : nil, minHeight: 34)
                .overlay(Capsule().strokeBorder(Theme.borderStrong, lineWidth: 1))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
    }
}

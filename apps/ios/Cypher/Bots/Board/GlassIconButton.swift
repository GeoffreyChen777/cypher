// A decision as a small round glass button: a symbol in a system glass
// circle, named for VoiceOver by the option's full label — and, in a card,
// a short name under it. Every option of a permission prompt and a Needs
// you row's Decline / Approve stand on screen this way, side by side, so
// none hides behind a long-press or a More menu. One size and spacing
// everywhere (Theme's icon button tokens).

import SwiftUI

/// What an icon button's symbol is painted in.
enum GlassIconTone: Equatable {
    /// The app's ink.
    case ink
    /// A refusal: red.
    case destructive
    /// The main decision: prominent glass, the symbol in the page's paint.
    case prominent
}

/// A round system glass button holding one symbol.
struct GlassIconButton: View {
    let symbol: String
    /// The option's full name: what VoiceOver says.
    let label: String
    var tone = GlassIconTone.ink
    let action: () -> Void

    /// The label's side: the glass adds the regular control's padding
    /// around it, which makes the circle `Theme.iconButtonSize`.
    private static let labelSide = Theme.iconButtonSize - 2 * Theme.iconButtonGlassPadding

    var body: some View {
        Button(role: tone == .destructive ? .destructive : nil, action: action) {
            Image(systemName: symbol)
                .font(.system(size: Theme.iconButtonSymbol, weight: .semibold))
                .foregroundStyle(foreground)
                .frame(width: Self.labelSide, height: Self.labelSide)
                .contentShape(Circle())
        }
        .glassIconButtonStyle(prominent: tone == .prominent)
        .accessibilityLabel(label)
        // The circle keeps its size at every text size, as a bar button
        // does; a long press at an accessibility size shows it large.
        .accessibilityShowsLargeContentViewer {
            Label(label, systemImage: symbol)
        }
    }

    private var foreground: Color {
        switch tone {
        case .ink: Theme.text
        case .destructive: Theme.danger
        case .prominent: Theme.bg
        }
    }
}

extension View {
    /// The icon buttons' style: system glass (prominent for the main
    /// decision) in a circle, the regular control size, the app's ink.
    func glassIconButtonStyle(prominent: Bool) -> some View {
        Group {
            if prominent {
                self.buttonStyle(.glassProminent)
            } else {
                self.buttonStyle(.glass)
            }
        }
        .buttonBorderShape(.circle)
        .controlSize(.regular)
        .tint(Theme.text)
    }
}

/// An icon button with its short name under it, in a column of one width
/// so a row of them reads as one control (as iOS sets the buttons of a
/// call or a share sheet). The name is for the eye: VoiceOver reads the
/// button's full label. Like a tab bar's, the name grows with the text
/// size only up to `Theme.iconCaptionLargestType` — past it the buttons
/// would stand one to a line — and a long press shows the option large.
struct GlassIconOption: View {
    let symbol: String
    let label: String
    let caption: String
    var tone = GlassIconTone.ink
    let action: () -> Void

    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        VStack(spacing: Theme.iconOptionCaptionGap) {
            GlassIconButton(symbol: symbol, label: label, tone: tone, action: action)
            Text(caption)
                .font(Theme.sans(11, weight: .medium, relativeTo: .caption2))
                .foregroundStyle(tone == .destructive ? Theme.danger : Theme.textMuted)
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                .dynamicTypeSize(...Theme.iconCaptionLargestType)
                .accessibilityHidden(true)
        }
        .frame(width: max(column, Theme.iconButtonSize))
    }

    /// The column grows with the name, up to the same text size.
    private var column: CGFloat {
        let size = UIContentSizeCategory(min(typeSize, Theme.iconCaptionLargestType))
        return UIFontMetrics(forTextStyle: .caption2).scaledValue(
            for: Theme.iconOptionColumn, compatibleWith: UITraitCollection(preferredContentSizeCategory: size))
    }
}

/// Icon options on an even grid across the width it is given: as few lines
/// as fit, the options shared out evenly between them (four in three
/// columns stand two and two, not three and one), each centred in its
/// cell, further lines below in the same columns — wrapping icon buttons
/// that never clip and never fold into a menu. Its views are of one width
/// (`GlassIconOption`).
struct IconOptionGrid: Layout {
    var spacing: CGFloat = Theme.iconButtonGap
    var lineSpacing: CGFloat = Theme.iconButtonGap

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard !subviews.isEmpty else { return .zero }
        let item = item(subviews)
        let ideal = item.width * CGFloat(subviews.count) + spacing * CGFloat(subviews.count - 1)
        let width = proposal.width.flatMap { $0.isFinite ? max($0, item.width) : nil } ?? ideal
        let columns = columns(width, item, count: subviews.count)
        let lines = (subviews.count + columns - 1) / columns
        return CGSize(width: width, height: item.height * CGFloat(lines) + lineSpacing * CGFloat(lines - 1))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard !subviews.isEmpty else { return }
        let item = item(subviews)
        let columns = columns(bounds.width, item, count: subviews.count)
        let cell = bounds.width / CGFloat(columns)
        for (index, subview) in subviews.enumerated() {
            let column = index % columns
            let line = index / columns
            subview.place(
                at: CGPoint(
                    x: bounds.minX + cell * (CGFloat(column) + 0.5),
                    y: bounds.minY + (item.height + lineSpacing) * CGFloat(line)),
                anchor: .top, proposal: ProposedViewSize(item))
        }
    }

    /// The widest and tallest of its views.
    private func item(_ subviews: Subviews) -> CGSize {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        return CGSize(width: sizes.map(\.width).max() ?? 0, height: sizes.map(\.height).max() ?? 0)
    }

    private func columns(_ width: CGFloat, _ item: CGSize, count: Int) -> Int {
        Self.balanced(count: count, columns: Self.columns(fitting: width, item: item.width, spacing: spacing))
    }

    /// The columns `count` views take when at most `columns` fit side by
    /// side: no more lines than that needs, and as few columns as fill
    /// them, so the last line isn't left with one.
    static func balanced(count: Int, columns: Int) -> Int {
        guard count > 0, columns > 0 else { return max(1, columns) }
        let lines = (count + columns - 1) / columns
        return (count + lines - 1) / lines
    }

    /// How many views `item` wide fit side by side in `width`: one at least.
    static func columns(fitting width: CGFloat, item: CGFloat, spacing: CGFloat) -> Int {
        guard item > 0, width.isFinite else { return 1 }
        return max(1, Int((width + spacing) / (item + spacing)))
    }
}

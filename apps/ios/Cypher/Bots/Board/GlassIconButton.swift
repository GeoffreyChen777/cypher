// Every decision as one control: a system glass button of one height
// (`Theme.decisionHeight`), round with a symbol or a capsule with a title.
// A permission prompt's options, a card's Reject / Approve, an answer to
// pick, a Needs you row's Decline / Approve and a page's decision bar all
// go through `decisionControl` and `decisionButtonStyle` here, so none sets
// a height or a padding of its own and they stand one height side by side
// at every text size. A round button may carry its short name under it,
// outside the control (`GlassIconOption`).

import SwiftUI

/// The one height of a decision button at the current text size: the
/// token, its label's line scaled with Dynamic Type (`.subheadline`, the
/// capsules' title style), the glass's inset fixed.
struct DecisionHeight: DynamicProperty {
    @ScaledMetric(relativeTo: .subheadline) private var line = Theme.decisionLabelLine

    /// The control's height, and a round button's diameter.
    var control: CGFloat { Self.control(line: line) }
    /// The label's height inside the system glass.
    var label: CGFloat { Self.label(line: line) }
    /// A round button's symbol, grown as the line.
    var symbol: CGFloat { Theme.iconButtonSymbol * line / Theme.decisionLabelLine }

    /// The control's height for a label line `line` tall.
    static func control(line: CGFloat) -> CGFloat {
        Theme.decisionHeight - Theme.decisionLabelLine + line
    }

    /// The label inside the glass for a line `line` tall: the glass adds
    /// `Theme.glassButtonPadding` on each side to make it `control(line:)`.
    static func label(line: CGFloat) -> CGFloat {
        control(line: line) - 2 * Theme.glassButtonPadding
    }
}

/// A decision button's shape.
enum DecisionShape {
    /// A symbol in a glass circle.
    case circle
    /// A title in a glass capsule across the width it is given.
    case capsule
}

extension VerticalAlignment {
    private enum DecisionCentre: AlignmentID {
        static func defaultValue(in context: ViewDimensions) -> CGFloat { context[VerticalAlignment.center] }
    }

    /// A decision control's centre: a row of decisions lines up on it, so a
    /// capsule stands level with the circles, not with circle and caption.
    static let decisionCentre = VerticalAlignment(DecisionCentre.self)
}

/// A decision button's label at the one height: a circle's square side, a
/// capsule's height and width. A capsule's title stands on one line — it
/// shrinks a little, then truncates, rather than wrap and make its button
/// taller than the others; a row that can't hold its capsules side by side
/// stacks them instead (`NeedsCardButtonRow`), each still the one height.
private struct DecisionControl: ViewModifier {
    let shape: DecisionShape
    let height = DecisionHeight()

    func body(content: Content) -> some View {
        switch shape {
        case .circle:
            content
                .frame(width: height.label, height: height.label)
                .contentShape(Circle())
        case .capsule:
            content
                .lineLimit(1)
                .minimumScaleFactor(Theme.decisionTitleMinScale)
                .frame(maxWidth: .infinity)
                .frame(height: height.label)
                .contentShape(Capsule())
        }
    }
}

extension View {
    /// A decision button's label at the one control height
    /// (`Theme.decisionHeight`), for `decisionButtonStyle` to wrap.
    func decisionControl(_ shape: DecisionShape) -> some View {
        modifier(DecisionControl(shape: shape))
    }

    /// A decision button's style: system glass (prominent for the main
    /// decision), round or a capsule, the regular control size, the app's
    /// ink. Its label is a `decisionControl` of the same shape.
    func decisionButtonStyle(_ shape: DecisionShape, prominent: Bool) -> some View {
        Group {
            if prominent {
                self.buttonStyle(.glassProminent)
            } else {
                self.buttonStyle(.glass)
            }
        }
        .buttonBorderShape(shape == .circle ? .circle : .capsule)
        .controlSize(.regular)
        .tint(Theme.text)
    }
}

/// What an icon button's symbol is painted in.
enum GlassIconTone: Equatable {
    /// The app's ink.
    case ink
    /// A refusal: red.
    case destructive
    /// The main decision: prominent glass, the symbol in the page's paint.
    case prominent
}

/// A round system glass button holding one symbol, as tall as every other
/// decision button and growing with the text size as they do.
struct GlassIconButton: View {
    let symbol: String
    /// The option's full name: what VoiceOver says.
    let label: String
    var tone = GlassIconTone.ink
    let action: () -> Void

    private let height = DecisionHeight()

    var body: some View {
        Button(role: tone == .destructive ? .destructive : nil, action: action) {
            Image(systemName: symbol)
                .font(.system(size: height.symbol, weight: .semibold))
                .foregroundStyle(foreground)
                .decisionControl(.circle)
        }
        .decisionButtonStyle(.circle, prominent: tone == .prominent)
        .accessibilityLabel(label)
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

/// An icon button with its short name under it, in a column of one width
/// so a row of them reads as one control (as iOS sets the buttons of a
/// call or a share sheet). The name is for the eye: VoiceOver reads the
/// button's full label. Like a tab bar's, the name grows with the text
/// size only up to `Theme.iconCaptionLargestType` — past it the buttons
/// would stand one to a line — and a long press shows the option large.
/// The name stands outside the control: a row lines up on the circle's
/// centre (`.decisionCentre`).
struct GlassIconOption: View {
    let symbol: String
    let label: String
    let caption: String
    var tone = GlassIconTone.ink
    /// The button's own accessibility id: its frame is the control's,
    /// without the name under it.
    var identifier: String?
    let action: () -> Void

    @Environment(\.dynamicTypeSize) private var typeSize
    private let height = DecisionHeight()

    var body: some View {
        VStack(spacing: Theme.iconOptionCaptionGap) {
            GlassIconButton(symbol: symbol, label: label, tone: tone, action: action)
                .accessibilityIdentifier(identifier ?? "")
                .alignmentGuide(.decisionCentre) { $0[VerticalAlignment.center] }
            Text(caption)
                .font(Theme.sans(11, weight: .medium, relativeTo: .caption2))
                .foregroundStyle(tone == .destructive ? Theme.danger : Theme.textMuted)
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
                .dynamicTypeSize(...Theme.iconCaptionLargestType)
                .accessibilityHidden(true)
        }
        .frame(width: max(column, height.control))
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

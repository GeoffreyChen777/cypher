import SwiftUI
import XCTest

@testable import Cypher

/// Every decision button is one control height (`Theme.decisionHeight`):
/// a round icon button, a card's capsule and a page's decision bar, at the
/// default text size and as they grow with Dynamic Type.
@MainActor
final class DecisionButtonHeightTests: XCTestCase {
    private static let sizes: [DynamicTypeSize] = [.large, .xxxLarge, .accessibility3]

    func testTheOneHeightComesFromTheToken() {
        XCTAssertEqual(Theme.decisionHeight, 44, "iOS's standard control height")
        XCTAssertEqual(DecisionHeight.control(line: Theme.decisionLabelLine), Theme.decisionHeight)
        XCTAssertEqual(
            DecisionHeight.label(line: Theme.decisionLabelLine),
            Theme.decisionHeight - 2 * Theme.glassButtonPadding)
        // It grows by the label's line alone: the glass's inset stays.
        XCTAssertEqual(DecisionHeight.control(line: 48), Theme.decisionHeight + 28)
    }

    func testEveryKindOfDecisionButtonIsOneHeightAtEveryTextSize() {
        for size in Self.sizes {
            let expected = DecisionHeight.control(line: Self.scaledLine(size))
            let circle = Self.height(GlassIconButton(symbol: "xmark", label: "Deny", tone: .destructive) {}, size)
            let prominentCircle = Self.height(
                GlassIconButton(symbol: "checkmark", label: "Approve", tone: .prominent) {}, size)
            let capsule = Self.height(NeedsCardButton(title: "Reject", kind: .destructive) {}, size)
            let prominentCapsule = Self.height(NeedsCardButton(title: "Run once", kind: .primary) {}, size)
            XCTAssertEqual(circle, expected, accuracy: 1, "a circle at \(size)")
            XCTAssertEqual(prominentCircle, expected, accuracy: 1, "a prominent circle at \(size)")
            XCTAssertEqual(capsule, expected, accuracy: 1, "a capsule at \(size)")
            XCTAssertEqual(prominentCapsule, expected, accuracy: 1, "a prominent capsule at \(size)")
        }
        XCTAssertEqual(
            Self.height(GlassIconButton(symbol: "xmark", label: "Deny") {}, .large), 44, accuracy: 1,
            "44pt at the default text size")
    }

    func testADecisionBarsButtonsAreTheSameHeight() {
        for size in Self.sizes {
            let expected = DecisionHeight.control(line: Self.scaledLine(size))
            let bar = Self.height(
                BotDecisionBar(decisions: [
                    BotDecision(title: "Decline", role: .destructive) {},
                    BotDecision(title: "Approve", role: .primary) {},
                ]), size)
            // The bar's 12pt top and 8pt bottom margins around its buttons:
            // one line of them, or, where one line can't hold them, one
            // under another — each the one height either way, never a
            // button grown by a wrapped title.
            let gap = NeedsCardMetrics.buttonGap
            let lines = ((bar - 20 + gap) / (expected + gap)).rounded()
            XCTAssertEqual(
                bar, lines * expected + (lines - 1) * gap + 20, accuracy: 1,
                "the bar at \(size): \(bar)pt is no whole number of \(expected)pt buttons")
            if size == .large {
                XCTAssertEqual(lines, 1, "side by side at the default text size")
            }
        }
    }

    func testALongTitleKeepsItsCapsuleTheOneHeight() {
        let title = "Allow for this chat and every chat after it in this project"
        for size in Self.sizes {
            let expected = DecisionHeight.control(line: Self.scaledLine(size))
            for width: CGFloat in [120, 300] {
                XCTAssertEqual(
                    Self.height(NeedsCardButton(title: title) {}, size, width: width), expected, accuracy: 1,
                    "a long title at \(size), \(width)pt wide: one line, not a taller button")
            }
        }
    }

    /// The label's line at `size`, as `@ScaledMetric(relativeTo: .subheadline)`
    /// scales it.
    private static func scaledLine(_ size: DynamicTypeSize) -> CGFloat {
        let category = UIContentSizeCategory(size)
        return UIFontMetrics(forTextStyle: .subheadline).scaledValue(
            for: Theme.decisionLabelLine, compatibleWith: UITraitCollection(preferredContentSizeCategory: category))
    }

    /// The height `view` lays out at, `width` wide, at `size`.
    private static func height(_ view: some View, _ size: DynamicTypeSize, width: CGFloat = 300) -> CGFloat {
        let host = UIHostingController(
            rootView: view.frame(width: width).fixedSize(horizontal: false, vertical: true)
                .environment(\.dynamicTypeSize, size))
        return host.sizeThatFits(in: CGSize(width: width, height: CGFloat.greatestFiniteMagnitude)).height
    }
}

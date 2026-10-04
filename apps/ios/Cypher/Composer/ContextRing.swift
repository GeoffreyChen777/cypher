// Composer context gauge — context_ring.rs on the phone: the agent's
// context-window occupancy as a short arc round the send button, muted, then
// amber from 75% and red from 90%. It shows in every composer state, the
// collapsed pill included. The desktop compacts on click and explains in a
// tooltip; a 2pt arc can't take a tap without stealing the send button's, so
// a long press on the button opens a menu carrying the reading and an
// explicit Compact (a stray tap never rewrites the agent's memory). The `/`
// menu's /compact row carries the same reading.

import SwiftUI

extension ContextUsage {
    static let warnFraction = 0.75
    static let dangerFraction = 0.9

    /// context_ring.rs `format_tokens`: `950`, `9.5k`, `124k`, `1.2M`.
    static func formatTokens(_ tokens: Int64) -> String {
        func trimmed(_ value: Double, _ suffix: String) -> String {
            var text = String(format: "%.1f", value)
            if text.hasSuffix(".0") { text.removeLast(2) }
            return text + suffix
        }
        switch tokens {
        case ..<1_000: return "\(max(tokens, 0))"
        case ..<10_000: return trimmed(Double(tokens) / 1_000, "k")
        case ..<1_000_000: return "\(Int64((Double(tokens) / 1_000).rounded()))k"
        default: return trimmed(Double(tokens) / 1_000_000, "M")
        }
    }

    /// `62% context used · 124k / 200k`.
    var summary: String {
        "\(Int((fraction * 100).rounded()))% context used · \(Self.formatTokens(used)) / \(Self.formatTokens(size))"
    }

    var color: Color {
        if fraction >= Self.dangerFraction { return Theme.danger }
        if fraction >= Self.warnFraction { return Theme.warning }
        return Theme.textMuted
    }
}

/// Why Compact is or isn't offered (pickers.rs context_ring_chip hints).
enum CompactAvailability: Equatable {
    case ready
    /// A run is live or waiting on an answer: `/compact` mid-turn would
    /// reach the model as plain text, never as a steer.
    case busy
    /// Read-only here: the phone drives Pi sessions only.
    case unsupported
    case offline

    var note: String? {
        switch self {
        case .ready: return nil
        case .busy: return "Compact once the agent finishes"
        case .unsupported: return "Compact this agent from the desktop"
        case .offline: return "The session's device is offline"
        }
    }
}

/// What the send button's gauge needs: the reading and what Compact may do.
struct ContextGauge {
    var usage: ContextUsage
    var availability: CompactAvailability
    var onCompact: () -> Void
}

/// context_ring.rs `edge_arc`: a faint 60° track concentric with the send
/// button, the used share laid over it from the bottom up, 2pt with rounded
/// ends, 4.5pt clear of the button and about as far from the composer's edge
/// (12pt between the two).
/// On the one-line pill it sits on the button's far side; in the expanded
/// card it turns to the bottom-right corner, as on the desktop.
struct ContextArc: View {
    let usage: ContextUsage
    let expanded: Bool

    static let span: Double = 60
    static let stroke: CGFloat = 2
    /// The band's centerline: the 20pt button, 4.5pt clear, half the stroke.
    static let radius: CGFloat = 20 + 4.5 + stroke / 2

    var body: some View {
        let center: Double = expanded ? 45 : 0
        let side = 2 * (Self.radius + Self.stroke)
        ZStack {
            ArcBand(center: center, share: 1)
                .stroke(Theme.textMuted.opacity(0.25), style: Self.style)
            ArcBand(center: center, share: usage.fraction)
                .stroke(usage.color, style: Self.style)
        }
        .frame(width: side, height: side)
        .motionAnimation(Motion.resize, value: usage.fraction)
        .motionAnimation(Motion.collapse, value: expanded)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }

    private static let style = StrokeStyle(lineWidth: stroke, lineCap: .round, lineJoin: .round)
}

/// The first `share` of the arc, measured from its lower end. Angles are
/// degrees clockwise from 3 o'clock (screen coordinates, y down), traced
/// point by point so the direction never depends on a flip convention.
struct ArcBand: Shape {
    var center: Double
    var share: Double

    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(center, share) }
        set { center = newValue.first; share = newValue.second }
    }

    func path(in rect: CGRect) -> Path {
        var path = Path()
        let share = min(max(share, 0), 1)
        guard share > 0 else { return path }
        let end = center + ContextArc.span / 2
        let start = end - ContextArc.span * share
        let steps = max(2, Int((ContextArc.span * share / 3).rounded(.up)))
        for step in 0...steps {
            let angle = (start + (end - start) * Double(step) / Double(steps)) * .pi / 180
            let point = CGPoint(x: rect.midX + ContextArc.radius * cos(angle),
                                y: rect.midY + ContextArc.radius * sin(angle))
            if step == 0 { path.move(to: point) } else { path.addLine(to: point) }
        }
        return path
    }
}

/// The send button's long-press menu and VoiceOver reading, when the chat
/// has a reading.
struct ContextGaugeMenu: ViewModifier {
    let gauge: ContextGauge?

    func body(content: Content) -> some View {
        if let gauge {
            content
                .contextMenu {
                    Text(gauge.usage.summary)
                    if let note = gauge.availability.note {
                        Text(note)
                    }
                    Button("Compact context", systemImage: "arrow.down.right.and.arrow.up.left") {
                        UIImpactFeedbackGenerator(style: .light).impactOccurred()
                        gauge.onCompact()
                    }
                    .disabled(gauge.availability != .ready)
                }
                .accessibilityValue(gauge.usage.summary)
                .accessibilityAction(named: "Compact context") {
                    if gauge.availability == .ready { gauge.onCompact() }
                }
        } else {
            content
        }
    }
}

// Composer context ring — context_ring.rs on the phone: the agent's
// context-window occupancy as a small ring beside the model chip, amber from
// 75% and red from 90%. The desktop compacts on click and explains in a
// tooltip; a phone has no hover, so a tap opens a menu carrying the reading
// and an explicit Compact (a stray tap never rewrites the agent's memory).

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

struct ContextRingChip: View {
    let usage: ContextUsage
    let availability: CompactAvailability
    let onCompact: () -> Void

    var body: some View {
        Menu {
            Text(usage.summary)
            if let note = availability.note {
                Text(note)
            }
            Button("Compact context", systemImage: "arrow.down.right.and.arrow.up.left") {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onCompact()
            }
            .disabled(availability != .ready)
        } label: {
            ContextRing(fraction: usage.fraction, color: usage.color)
                .frame(width: 40, height: 40)
                .background(whiteAlpha(0.08), in: Circle())
                .overlay(Circle().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
                .contentShape(Circle())
        }
        .accessibilityLabel("Context")
        .accessibilityValue(usage.summary)
        .accessibilityIdentifier("context-ring")
    }
}

/// A 16pt ring: the track at 25% muted, filled clockwise from 12 o'clock.
struct ContextRing: View {
    let fraction: Double
    let color: Color

    var body: some View {
        ZStack {
            Circle()
                .stroke(Theme.textMuted.opacity(0.25), lineWidth: 2)
            Circle()
                .trim(from: 0, to: fraction)
                .stroke(color, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: 16, height: 16)
        .motionAnimation(Motion.resize, value: fraction)
    }
}

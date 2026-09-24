// Turn scrubber — a column of ticks on the transcript's trailing edge, one
// per round of conversation (a user prompt and everything answering it; a
// steer stays inside its round). The current round's tick is lit. Hidden at
// rest: a touch on the edge fades the column in, sliding the finger jumps
// round to round with a selection haptic per round and a preview of the
// prompt beside the finger, and the column fades out shortly after release.

import SwiftUI
import UIKit

/// One round of conversation: where it starts, and its prompt for preview.
struct TranscriptRound: Equatable {
    /// The prompt row's id, a ForEach id — `scrollTo(id:)` can target it.
    var rowId: String
    /// Index into the full (unwindowed) row list.
    var rowIndex: Int
    var preview: String

    static func rounds(in rows: [TranscriptRow]) -> [TranscriptRound] {
        rows.indices.compactMap { ix in
            guard case .user(let text, let isSteer) = rows[ix].kind, !isSteer else { return nil }
            return TranscriptRound(rowId: rows[ix].id, rowIndex: ix, preview: previewLine(text))
        }
    }

    /// The prompt's first non-blank line, whitespace collapsed.
    static func previewLine(_ text: String) -> String {
        let line = text.split(whereSeparator: \.isNewline)
            .first { !$0.allSatisfy(\.isWhitespace) } ?? ""
        return String(line.split(whereSeparator: \.isWhitespace).joined(separator: " ").prefix(140))
    }
}

/// Which round the reader is in. Prompt rows report their top edge while
/// realized; the current round is the last prompt above the reading line.
/// Only `current`/`atBottom` are observable, and they're written only when
/// they change — the transcript body never sees the per-frame reports.
@Observable
final class TurnTracker {
    var current: Int?
    /// Pinned or within the re-stick band: the last round, whatever the
    /// prompt positions say (a long final answer leaves its prompt far up).
    var atBottom = true
    /// The column is showing; the transcript hides its scroll indicator
    /// meanwhile, so the two never overlap.
    var revealed = false
    @ObservationIgnored private var tops: [Int: CGFloat] = [:]
    /// Prompt rows the lazy stack holds right now. A row's last geometry
    /// report can land after its disappearance (a jump tears rows down
    /// mid-flight); counting it left a phantom "Round 6" after landing on
    /// round 1. Reports are kept either way — appear can come second.
    @ObservationIgnored private var live: Set<Int> = []
    @ObservationIgnored private var line: CGFloat = 0

    /// `line` is the reading line in the same (scroll view) space as `top`.
    func report(round: Int, top: CGFloat, line: CGFloat) {
        tops[round] = top
        self.line = line
        recompute()
    }

    func appeared(round: Int) {
        live.insert(round)
        recompute()
    }

    func disappeared(round: Int) {
        live.remove(round)
        tops[round] = nil
    }

    private func recompute() {
        let known = tops.filter { live.contains($0.key) }
        let above = known.filter { $0.value <= line }.keys.max()
        // No prompt above the line: the reader is in the round before the
        // first prompt still below it.
        let next = above ?? known.keys.min().map { max(0, $0 - 1) }
        if let next, next != current { current = next }
    }

    func set(_ round: Int) {
        if current != round { current = round }
    }
}

/// Reports a prompt row's position to the tracker.
struct TurnAnchor: ViewModifier {
    let round: Int?
    let tracker: TurnTracker
    let scroll: ScrollState

    func body(content: Content) -> some View {
        if let round {
            content
                .onGeometryChange(for: CGFloat.self) { $0.frame(in: .scrollView).minY } action: { [tracker, scroll] top in
                    // A little above center: a prompt that has scrolled up
                    // past this is the one being read.
                    tracker.report(round: round, top: top, line: scroll.viewportHeight * 0.4)
                }
                .onAppear { tracker.appeared(round: round) }
                .onDisappear { tracker.disappeared(round: round) }
        } else {
            content
        }
    }
}

struct TurnScrubber: View {
    let rounds: [TranscriptRound]
    let tracker: TurnTracker
    /// Scrolls the transcript to a round's prompt.
    let jump: (Int) -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// The round under the finger while scrubbing.
    @State private var scrubbing: Int?
    /// The round under the finger at touch-down. A touch only reveals; the
    /// jumps start once the finger leaves this round, so a stray touch on
    /// the invisible edge never moves the transcript.
    @State private var armed: Int?
    @State private var hideTask: Task<Void, Never>?
    @State private var selection = UISelectionFeedbackGenerator()
    @State private var grab = UIImpactFeedbackGenerator(style: .light)

    /// Tick pitch (center to center): the reference's airy 12pt, packed down
    /// to 4pt on long sessions before ticks start standing for several rounds.
    static let pitchMax: CGFloat = 12
    static let pitchMin: CGFloat = 4
    static let tickWidth: CGFloat = 12
    /// Touch slop beyond the first and last tick.
    static let slop: CGFloat = 16
    /// How long the column lingers after the finger lifts.
    static let linger: Duration = .milliseconds(1200)

    var body: some View {
        GeometryReader { geo in
            let layout = TickLayout(rounds: rounds.count,
                                    maxHeight: min(geo.size.height * 0.55, 360))
            strip(layout)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .trailing)
        }
    }

    /// The lit round: the finger's while scrubbing, else the reader's.
    private var active: Int {
        let last = rounds.count - 1
        if let scrubbing { return scrubbing }
        if tracker.atBottom { return last }
        return min(tracker.current ?? last, last)
    }

    private func strip(_ layout: TickLayout) -> some View {
        let lit = layout.tick(forRound: active)
        return VStack(alignment: .trailing, spacing: 0) {
            ForEach(0..<layout.ticks, id: \.self) { tick in
                let distance = abs(tick - lit)
                Capsule()
                    .fill(distance == 0 ? Theme.text : Theme.textFaint.opacity(scrubbing == nil ? 0.55 : 0.8))
                    .frame(width: width(distance: distance), height: 2)
                    .frame(height: layout.pitch)
            }
        }
        // The ticks fade, not the column: it stays a touch target (and an
        // accessibility element) while nothing shows.
        .opacity(tracker.revealed ? 1 : 0)
        .padding(.vertical, Self.slop)
        // The whole column is the target, not just the 2pt ticks.
        .frame(width: 44, alignment: .trailing)
        .contentShape(Rectangle())
        .overlay(alignment: .topTrailing) {
            if let scrubbing {
                preview(round: scrubbing)
                    // Centered on the lit tick, left of the column. Its own
                    // height, or the zero-height frame squeezes it to a line.
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(height: 0)
                    .offset(x: -40, y: Self.slop + layout.pitch * (CGFloat(lit) + 0.5))
                    .transition(.opacity.combined(with: .scale(scale: 0.92, anchor: .trailing)))
            }
        }
        .padding(.trailing, 4)
        .animation(reduceMotion ? nil : .spring(response: 0.28, dampingFraction: 0.82), value: lit)
        .animation(reduceMotion ? nil : .spring(response: 0.3, dampingFraction: 0.85), value: scrubbing == nil)
        .animation(reduceMotion ? nil : .easeOut(duration: tracker.revealed ? 0.15 : 0.35), value: tracker.revealed)
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    let round = layout.round(atY: value.location.y - Self.slop)
                    if armed == nil {
                        armed = round
                        reveal()
                        grab.impactOccurred(intensity: 0.6)
                        selection.prepare()
                        return
                    }
                    if scrubbing == nil, round == armed { return }
                    guard round != scrubbing else { return }
                    selection.selectionChanged()
                    selection.prepare()
                    scrubbing = round
                    jump(round)
                }
                .onEnded { _ in
                    if let scrubbing { tracker.set(scrubbing) }
                    scrubbing = nil
                    armed = nil
                    hideLater()
                }
        )
        .accessibilityElement()
        .accessibilityIdentifier("turn-scrubber")
        .accessibilityLabel("Conversation rounds")
        .accessibilityValue("Round \(active + 1) of \(rounds.count)")
        .accessibilityAdjustableAction { direction in
            let target: Int
            switch direction {
            case .increment: target = min(active + 1, rounds.count - 1)
            case .decrement: target = max(active - 1, 0)
            @unknown default: return
            }
            tracker.set(target)
            jump(target)
            reveal()
            hideLater()
        }
    }

    private func reveal() {
        hideTask?.cancel()
        hideTask = nil
        if !tracker.revealed { tracker.revealed = true }
    }

    private func hideLater() {
        hideTask?.cancel()
        hideTask = Task { @MainActor in
            try? await Task.sleep(for: Self.linger)
            guard !Task.isCancelled else { return }
            tracker.revealed = false
        }
    }

    /// Ticks near the finger swell (the dock's magnification, in miniature);
    /// at rest only color marks the current round, as in the reference.
    private func width(distance: Int) -> CGFloat {
        guard scrubbing != nil else { return Self.tickWidth }
        switch distance {
        case 0: return 24
        case 1: return 18
        case 2: return 15
        default: return Self.tickWidth
        }
    }

    private func preview(round: Int) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Round \(round + 1) of \(rounds.count)")
                .font(Theme.sans(11, weight: .medium, relativeTo: .caption))
                .foregroundStyle(Theme.textMuted)
                .monospacedDigit()
            Text(rounds[round].preview.isEmpty ? "Message" : rounds[round].preview)
                .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                .foregroundStyle(Theme.text)
                .lineLimit(2)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frame(width: 230, alignment: .leading)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Where the ticks go. Past what fits at `pitchMin`, each tick stands for an
/// even share of the rounds; the finger still resolves every round.
struct TickLayout {
    let rounds: Int
    let ticks: Int
    let pitch: CGFloat

    init(rounds: Int, maxHeight: CGFloat) {
        self.rounds = rounds
        let fit = max(2, Int(maxHeight / TurnScrubber.pitchMin))
        ticks = min(rounds, fit)
        pitch = min(TurnScrubber.pitchMax, maxHeight / CGFloat(max(ticks, 1)))
    }

    func tick(forRound round: Int) -> Int {
        guard ticks < rounds, rounds > 1 else { return round }
        return Int((Double(round) * Double(ticks - 1) / Double(rounds - 1)).rounded())
    }

    /// `y` from the top of the first tick's cell; clamped to the ends.
    func round(atY y: CGFloat) -> Int {
        guard rounds > 1, ticks > 1 else { return 0 }
        let fraction = (y - pitch / 2) / (pitch * CGFloat(ticks - 1))
        let round = Int((min(max(fraction, 0), 1) * CGFloat(rounds - 1)).rounded())
        return round
    }
}

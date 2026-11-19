// The marks and the page head the workspace's pages share, in the desktop's
// rule (docs/design/agent-bots.md §13.1 "Marks at the left, a note at the
// right"): a row's state is its mark, at its left; its right end is one
// quiet note. The progress ring (crates/ui/src/kit/progress_ring.rs) is the
// mark of anything under way, and a page opens on how its item stands, then
// its title, then at most one line of what nothing else on the page says.

import SwiftUI

// MARK: - The progress ring

/// How a ring stands (items.rs `RingState`).
enum RingState: Equatable {
    /// Not started yet, or a wait that has gone on: an empty ring, still.
    case waiting
    /// Under way: the comet goes round.
    case running
    /// Parked on its owner: amber, still.
    case asking
    /// Being stopped: paler, the comet still going.
    case stopping
}

/// One small mark that says both how far a job has come and that it is
/// alive. The steps done are an arc from twelve o'clock over a faint track,
/// in the text's ink (no accent: colour is kept for what needs the owner).
/// Once per period a comet leaves the tip of that arc, stretches as it
/// speeds round what is left, crosses the finished arc as a glint and is
/// taken back in at the tip, where the ring rests for a beat. With nothing
/// done it circles alone, a plain spinner; under Reduce Motion it holds
/// still.
struct ProgressRing: View {
    var state: RingState = .running
    var done = 0
    var total = 0
    var size: CGFloat = 15
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Once round and a rest (motion.rs `RING_ORBIT`).
    static let period: Double = 1.6
    /// The share of a period the comet travels in.
    static let travel: Double = 0.86
    /// How long after the head the tail sets off, as a share of the travel.
    static let lag: Double = 0.18
    private static let slices = 5

    /// The comet at `phase` of a period: how far its tail and head have
    /// travelled from the tip of the arc, in turns (progress_ring.rs
    /// `comet`).
    static func comet(_ phase: Double) -> (tail: Double, head: Double) {
        let wrapped = phase - phase.rounded(.down)
        let travelled = min(wrapped / travel, 1)
        func ease(_ t: Double) -> Double {
            let t = min(max(t, 0), 1)
            return t * t * (3 - 2 * t)
        }
        return (ease((travelled - lag) / (1 - lag)), ease(travelled / (1 - lag)))
    }

    var fraction: Double {
        total > 0 ? min(max(Double(done) / Double(total), 0), 1) : 0
    }

    private var moving: Bool { state == .running || state == .stopping }

    var body: some View {
        TimelineView(.animation(paused: !moving || reduceMotion)) { timeline in
            let phase = moving && !reduceMotion ? timeline.date.timeIntervalSinceReferenceDate / Self.period : 0
            Canvas { context, canvas in
                draw(&context, canvas, phase: phase)
            }
        }
        .frame(width: size, height: size)
        .accessibilityElement()
        .accessibilityLabel(label)
    }

    private var label: String {
        let steps = total > 0 ? ", \(done) of \(total) steps done" : ""
        switch state {
        case .waiting: return "Waiting\(steps)"
        case .running: return "Running\(steps)"
        case .asking: return "Waiting on you\(steps)"
        case .stopping: return "Stopping\(steps)"
        }
    }

    private func draw(_ context: inout GraphicsContext, _ canvas: CGSize, phase: Double) {
        let stroke: CGFloat = 2
        let radius = min(canvas.width, canvas.height) / 2 - stroke / 2
        guard radius > 0 else { return }
        let center = CGPoint(x: canvas.width / 2, y: canvas.height / 2)
        let (ink, track): (Color, Color) =
            switch state {
            case .asking: (Theme.warning, Theme.warning.opacity(0.3))
            case .stopping: (Theme.textMuted.opacity(0.7), Theme.text.opacity(0.11))
            default: (Theme.text.opacity(0.82), Theme.text.opacity(0.11))
            }
        // From twelve o'clock, clockwise; `from` and `to` in turns.
        func arc(_ from: Double, _ to: Double, _ color: Color, round: Bool) {
            var path = Path()
            path.addArc(
                center: center, radius: radius, startAngle: .degrees(from * 360 - 90),
                endAngle: .degrees(to * 360 - 90), clockwise: false)
            context.stroke(
                path, with: .color(color), style: StrokeStyle(lineWidth: stroke, lineCap: round ? .round : .butt))
        }
        arc(0, 1, track, round: false)
        let done = fraction
        if done >= 1 {
            arc(0, 1, ink, round: false)
        } else if done > 0 {
            arc(0, done, ink, round: true)
        }
        guard moving else { return }
        let (tail, head) = Self.comet(phase)
        guard head - tail > 0.004 else { return }
        // From the tip of the arc: what is left of the turn is track, the
        // lap past twelve o'clock runs over the arc, as a glint.
        let (from, to) = (done + tail, done + head)
        for slice in 0..<Self.slices {
            let at = { (ix: Int) in from + (to - from) * Double(ix) / Double(Self.slices) }
            let (start, end) = (at(slice), at(slice + 1))
            let strength = pow(Double(slice + 1) / Double(Self.slices), 1.6)
            let leading = slice + 1 == Self.slices
            if start < 1 {
                arc(start, min(end, 1), ink.opacity(strength), round: leading && end < 1)
            }
            if end > 1 {
                arc(max(start, 1), end, Theme.bg.opacity(0.72 * strength), round: leading)
            }
        }
    }
}

// MARK: - Marks

extension BotWork {
    /// A running plan parked on a question.
    var asking: Bool {
        phase == .running && currentStep?.step.state == .asking
    }

    /// Its ring while it is under way; nil once it ended or while it
    /// awaits approval (items.rs `work_mark`).
    var ring: RingState? {
        switch phase {
        case .starting: .waiting
        case .running: asking ? .asking : .running
        case .stopping: .stopping
        default: nil
        }
    }
}

/// A session's state as a mark on a workspace page (items.rs
/// `session_mark`): the ring while it works, an amber question mark while
/// it asks, a check when it finished unseen, a red mark when it failed, a
/// quiet dot at rest.
struct WorkspaceSessionMark: View {
    let indicator: ChatIndicator

    var body: some View {
        switch indicator {
        case .working: ProgressRing(state: .running, size: 13)
        case .awaitingInput: BoardGlyph(symbol: "questionmark.circle", color: Theme.warning)
        case .completed: BoardGlyph(symbol: "checkmark", color: Theme.success)
        case .errored: BoardGlyph(symbol: "xmark.circle", color: Theme.danger)
        case .idle: Circle().fill(Theme.textFaint.opacity(0.6)).frame(width: 5, height: 5)
        }
    }
}

/// The mark of an answer drafted for a connected workspace that waits for
/// the owner to send it, in amber wherever it shows (items.rs
/// `REVIEW_GLYPH`). Its own glyph: the question mark is a session asking,
/// the checklist a plan to approve.
let reviewSymbol = "paperplane"

// MARK: - A page's head

/// The head of a page that is run and controlled (a work item, a routine, a
/// connection; items.rs `item_header`): how it stands (its mark and a word
/// in its tone), its title, and at most one quiet line under it. A tap on
/// the status shows `details` (its times), which the page then never
/// repeats.
struct BotItemHeader<Mark: View, Line: View>: View {
    let status: BotStatusLabel
    let title: String
    /// What a tap on the status shows; none makes it plain text.
    var details: [(label: String, value: String)] = []
    var statusIdentifier = "item-status"
    @ViewBuilder var mark: Mark
    @ViewBuilder var line: Line
    @State private var showDetails = false

    private var statusRow: some View {
        HStack(spacing: 7) {
            mark.frame(width: 18, height: 18)
            Text(status.text)
                .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                .foregroundStyle(status.tone.color)
            if !details.isEmpty {
                Image(systemName: "info.circle")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.textFaint)
                    .accessibilityHidden(true)
            }
        }
        .contentShape(Rectangle())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if details.isEmpty {
                statusRow
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(status.text)
                    .accessibilityIdentifier(statusIdentifier)
            } else {
                Button {
                    showDetails = true
                } label: {
                    statusRow
                }
                .buttonStyle(.borderless)
                .accessibilityLabel(status.text)
                .accessibilityHint("Shows its times")
                .accessibilityIdentifier(statusIdentifier)
                .popover(isPresented: $showDetails) {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(details, id: \.label) { row in
                            HStack(alignment: .firstTextBaseline, spacing: 14) {
                                Text(row.label)
                                    .foregroundStyle(Theme.textMuted)
                                    .frame(minWidth: 70, alignment: .leading)
                                Text(row.value).foregroundStyle(Theme.text)
                            }
                            .font(Theme.sans(14, relativeTo: .subheadline))
                        }
                    }
                    .padding(16)
                    .presentationCompactAdaptation(.popover)
                }
            }
            Text(title)
                .font(Theme.sans(22, weight: .semibold, relativeTo: .title2))
                .foregroundStyle(Theme.text)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isHeader)
            line
                .font(Theme.sans(14, relativeTo: .subheadline))
                .foregroundStyle(Theme.textMuted)
        }
        .padding(.vertical, 4)
    }
}

extension BotItemHeader where Line == EmptyView {
    init(
        status: BotStatusLabel, title: String, details: [(label: String, value: String)] = [],
        statusIdentifier: String = "item-status", @ViewBuilder mark: () -> Mark
    ) {
        self.init(
            status: status, title: title, details: details, statusIdentifier: statusIdentifier, mark: mark,
            line: { EmptyView() })
    }
}

extension AppModel {
    /// A project as a workspace's page names it: its name, and its device
    /// only when that is not the workspace's own (items.rs
    /// `ProjectLabel::short`).
    func placeName(_ space: Space, for bot: Bot?) -> String {
        guard let bot, space.deviceId != bot.hostDeviceId else { return space.displayName }
        return "\(space.displayName) · \(deviceName(space.deviceId))"
    }
}

/// Text the owner or an agent wrote that a page shows as a quote: a plan's
/// request, a routine's prompt, a question.
struct BotQuote: View {
    let text: String
    var cacheKey = "bot-quote"

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            RoundedRectangle(cornerRadius: 1).fill(Theme.text.opacity(0.2)).frame(width: 2)
            BotMarkdown(text: text, cacheKey: cacheKey)
                .foregroundStyle(Theme.textMuted)
        }
        .padding(.vertical, 4)
    }
}

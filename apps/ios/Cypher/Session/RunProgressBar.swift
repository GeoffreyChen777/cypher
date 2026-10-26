// The working strip's progress bar — a port of crates/ui/src/kit/progress_bar.rs
// and transcript/progress.rs: a hairline track whose filled part carries a
// travelling glint, a solid dot where each passed stage began, a hollow ring
// breathing at the fill's end for the current stage, a faint dot for each
// step still to come, and a faint pinpoint at the finish. Every tone is a
// shade of the theme's text colour.
//
// A phone has no hover: the stage tips become one card, opened by tapping
// the strip (`RunProgressCard`).
//
// Motion: when a reading lands, the fill and the current ring glide to the
// new percentage, a new stage's ring grows in, refreshed upcoming dots glide
// to their places (new ones in from the finish), and passed stages a grown
// plan made room before glide back with the fill. Reduce Motion jumps,
// hides the glint and rests the ring small.

import SwiftUI

/// progress_bar.rs geometry and tones. Pure, so it is unit-tested.
enum ProgressBarGeometry {
    /// Height of the painted band; the track runs through its middle.
    static let height: CGFloat = 22
    /// How far the first and last dots sit inside the bar's ends.
    static let inset: Double = 7
    /// Glint and breath period.
    static let flowPeriod: Double = 2.4
    /// Most stage dots a bar carries (the engine keeps 16 stages).
    static let maxDots = 16

    static let track: Double = 2
    static let gap: Double = 2.5
    static let passedDot: Double = 6
    /// The current ring breathes between these diameters.
    static let currentSmall: Double = 8
    static let currentLarge: Double = 11
    static let currentBorder: Double = 1.5
    static let finishDot: Double = 4
    static let upcomingDot: Double = 4.5
    static let glint: Double = 72

    static let toneTrack = 0.06
    static let toneFill = 0.26
    static let toneGlint = 0.6
    static let toneFinish = 0.14
    static let toneUpcoming = 0.22
    static let tonePassed = 0.36
    static let toneCurrent = 0.58

    /// `dot_x`: the x of a stage at `at` (0–1), inset from both ends.
    static func dotX(_ at: Double, width: Double) -> Double {
        inset + min(max(at, 0), 1) * max(width - 2 * inset, 0)
    }

    /// `segments`: `from..<to` with every `(centre, radius)` hole cut out.
    static func segments(from: Double, to: Double, holes: [(Double, Double)]) -> [Range<Double>] {
        let cuts = holes.map { ($0.0 - $0.1, $0.0 + $0.1) }.sorted { $0.0 < $1.0 }
        var out: [Range<Double>] = []
        var start = from
        for (cutFrom, cutTo) in cuts {
            if cutTo <= start { continue }
            if cutFrom >= to { break }
            if cutFrom > start { out.append(start..<cutFrom) }
            start = max(start, cutTo)
        }
        if start < to { out.append(start..<to) }
        return out
    }

    /// `shown_stages`: the passed stages' x and the current one's. A passed
    /// dot that would overlap the dot after it is left out — the tones are
    /// translucent, so stacked dots would paint darker than the rest.
    static func shownStages(_ stages: [Double], width: Double) -> (passed: [Double], current: Double?) {
        guard let last = stages.last else { return ([], nil) }
        let current = dotX(last, width: width)
        var shown: [Double] = []
        var next = (x: current, room: (currentLarge + passedDot) / 2)
        for at in stages.dropLast().reversed() {
            let x = dotX(at, width: width)
            if next.x - x >= next.room {
                shown.append(x)
                next = (x, passedDot)
            }
        }
        return (shown.reversed(), current)
    }

    /// `shown_upcoming`: the upcoming steps' x, each left out when it would
    /// overlap the dot before it.
    static func shownUpcoming(_ upcoming: [Double], current: Double?, width: Double) -> [Double] {
        var shown: [Double] = []
        var before = current.map { (x: $0, room: (currentLarge + upcomingDot) / 2) }
        for at in upcoming {
            let x = dotX(at, width: width)
            if before.map({ x - $0.x >= $0.room }) ?? true {
                shown.append(x)
                before = (x, upcomingDot)
            }
        }
        return shown
    }
}

/// One frame's worth of bar (progress_bar.rs `ProgressBar`).
struct ProgressBarFrame: Equatable {
    /// Filled fraction, 0–1 (mid-tween while the fill moves).
    var fill: Double
    /// Stage positions, oldest first; the last (current) follows `fill`.
    var dots: [Double]
    var upcoming: [Double]
    /// How far the newest dot has grown in, 0–1.
    var appear: Double

    init(fill: Double, stages: [Double], upcoming: [Double], appear: Double) {
        let fill = min(max(fill, 0), 1)
        var dots = Array(stages.suffix(ProgressBarGeometry.maxDots))
        if !dots.isEmpty { dots[dots.count - 1] = fill }
        self.fill = fill
        self.dots = dots
        self.upcoming = Array(upcoming.prefix(RunProgress.maxUpcoming))
        self.appear = min(max(appear, 0), 1)
    }

    /// Paint at glint/breath phase `phase` (0–1; 0 under Reduce Motion, where
    /// the glint hides and the ring rests small).
    func paint(in context: inout GraphicsContext, size: CGSize, phase: Double, ink: Color) {
        typealias G = ProgressBarGeometry
        let width = Double(size.width)
        let mid = Double(size.height) / 2
        let (passed, current) = G.shownStages(dots, width: width)
        let ahead = G.shownUpcoming(upcoming, current: current, width: width)
        // Every dot with the room it keeps clear on the track. The current
        // ring keeps room for its largest breath, so the gap holds still.
        var holes = passed.map { ($0, G.passedDot / 2 + G.gap) }
        if let current { holes.append((current, G.currentLarge / 2 + G.gap)) }
        holes += ahead.map { ($0, G.upcomingDot / 2 + G.gap) }
        // The finish shows unless the last dot before it crowds it.
        let finish = G.dotX(1, width: width)
        let beforeFinish = ahead.last.map { ($0, G.upcomingDot) } ?? current.map { ($0, G.currentLarge) }
        let finishShown = beforeFinish.map { finish - $0.0 > $0.1 } ?? true
        if finishShown { holes.append((finish, G.finishDot / 2 + G.gap)) }
        let fillX = G.dotX(fill, width: width)
        func bar(_ span: Range<Double>) -> CGRect {
            CGRect(x: span.lowerBound, y: mid - G.track / 2, width: span.upperBound - span.lowerBound, height: G.track)
        }
        func capsule(_ span: Range<Double>) -> Path {
            Path(roundedRect: bar(span), cornerRadius: G.track / 2)
        }

        // Track, then fill over it, both broken around the dots.
        for span in G.segments(from: fillX, to: width, holes: holes) {
            context.fill(capsule(span), with: .color(ink.opacity(G.toneTrack)))
        }
        let fillSpans = G.segments(from: 0, to: fillX, holes: holes)
        for span in fillSpans {
            context.fill(capsule(span), with: .color(ink.opacity(G.toneFill)))
        }

        // The glint: a soft band sweeping the filled part once per period,
        // entering from off the left edge and leaving past the fill's end.
        if phase > 0, fillX > 0 {
            let eased = UnitCurve.easeInOut.value(at: phase)
            let centre = -G.glint / 2 + eased * (fillX + G.glint)
            let band = CGRect(x: centre - G.glint / 2, y: mid - G.track / 2, width: G.glint, height: G.track)
            let shading = GraphicsContext.Shading.linearGradient(
                Gradient(colors: [ink.opacity(0), ink.opacity(G.toneGlint), ink.opacity(0)]),
                startPoint: CGPoint(x: band.minX, y: mid), endPoint: CGPoint(x: band.maxX, y: mid))
            for span in fillSpans {
                var clipped = context
                clipped.clip(to: capsule(span))
                clipped.fill(Path(band), with: shading)
            }
        }

        func disc(_ x: Double, _ diameter: Double) -> Path {
            Path(ellipseIn: CGRect(x: x - diameter / 2, y: mid - diameter / 2, width: diameter, height: diameter))
        }
        if finishShown {
            context.fill(disc(finish, G.finishDot), with: .color(ink.opacity(G.toneFinish)))
        }
        for x in ahead {
            context.fill(disc(x, G.upcomingDot), with: .color(ink.opacity(G.toneUpcoming)))
        }
        for x in passed {
            context.fill(disc(x, G.passedDot), with: .color(ink.opacity(G.tonePassed)))
        }
        if let current {
            // Current: a hollow ring breathing in and out once per period,
            // grown in from nothing when its stage is new.
            let breath = 0.5 - 0.5 * cos(phase * 2 * .pi)
            let diameter = (G.currentSmall + (G.currentLarge - G.currentSmall) * breath) * appear
            if diameter > 0 {
                var ring = disc(current, diameter)
                let inner = diameter - 2 * G.currentBorder
                if inner > 0 { ring.addPath(disc(current, inner)) }
                context.fill(
                    ring, with: .color(ink.opacity(G.toneCurrent * appear)), style: FillStyle(eoFill: true))
            }
        }
    }
}

/// What a reading puts on the bar, once it lands.
struct ProgressReading: Equatable {
    /// Each stage's label, oldest first: how the tween tells a stage that
    /// moved from a new one.
    var labels: [String]
    /// Where each stage began, 0–1.
    var stages: [Double]
    var fill: Double
    var upcoming: [Double]

    init(_ progress: RunProgress) {
        let percent = progress.shownPercent
        labels = progress.shownStages.map(\.label)
        stages = progress.shownStages.map { Double(min($0.percent, 100)) / 100 }
        fill = Double(percent) / 100
        upcoming = RunProgress.upcomingPositions(percent: percent, count: progress.stepsLeftCount)
    }
}

/// progress.rs `ProgressMotion`: what changed between the readings shown, so
/// the bar glides rather than jumps. A plan that grew moves passed stages
/// back to make room: they glide there with the fill.
struct ProgressTween: Equatable {
    /// progress.rs `PROGRESS_TWEEN`: 900ms ease-out-expo.
    static let duration: Double = 0.9
    static let curve = UnitCurve.bezier(
        startControlPoint: UnitPoint(x: 0.16, y: 1), endControlPoint: UnitPoint(x: 0.3, y: 1))

    private(set) var reading: ProgressReading?
    private(set) var fromFill: Double = 0
    /// Where each stage glides from to its place in `reading`.
    private(set) var fromStages: [Double] = []
    private(set) var fromUpcoming: [Double] = []
    /// The last change added a stage (its ring grows in).
    private(set) var grew = false
    /// Seconds since the reference date; `-infinity` when it doesn't glide.
    private(set) var start: Double = -.infinity

    /// Eased progress through the glide at `now`.
    func t(at now: Double) -> Double {
        let raw = min(max((now - start) / Self.duration, 0), 1)
        return Self.curve.value(at: raw)
    }

    /// The stages' places `t` of the way through the glide.
    private func stages(at t: Double) -> [Double] {
        guard let reading else { return [] }
        return reading.stages.enumerated().map { ix, to in
            lerp(ix < fromStages.count ? fromStages[ix] : to, to, t)
        }
    }

    func frame(at now: Double) -> ProgressBarFrame? {
        guard let reading else { return nil }
        let t = t(at: now)
        let upcoming = reading.upcoming.enumerated().map { ix, to in
            lerp(ix < fromUpcoming.count ? fromUpcoming[ix] : 1, to, t)
        }
        return ProgressBarFrame(
            fill: lerp(fromFill, reading.fill, t), stages: stages(at: t), upcoming: upcoming,
            appear: grew ? min(t * 1.4, 1) : 1)
    }

    /// Note the reading on screen. The first one fills in from empty, its
    /// upcoming dots spreading out from the finish; later ones glide from
    /// wherever the bar is.
    mutating func observe(_ next: ProgressReading, now: Double, animated: Bool) {
        guard next != reading else { return }
        if let reading, let shown = frame(at: now) {
            // The stages on screen, matched to the new ones by label: the
            // oldest may have dropped off the front (the host keeps 16).
            let old = reading.labels
            let dropped =
                (0...old.count).first { k in
                    zip(old[k...], next.labels).allSatisfy { $0 == $1 }
                } ?? old.count
            let seen = Array(stages(at: t(at: now)).dropFirst(dropped))
            // A stage already on screen glides from where it is; a new one
            // grows in at its place.
            fromStages = next.stages.enumerated().map { ix, to in ix < seen.count ? seen[ix] : to }
            fromFill = shown.fill
            fromUpcoming = shown.upcoming
            grew = next.stages.count > seen.count
        } else {
            fromStages = next.stages
            fromFill = 0
            fromUpcoming = []
            grew = false
        }
        reading = next
        start = animated ? now : -.infinity
    }

    private func lerp(_ a: Double, _ b: Double, _ t: Double) -> Double { a + (b - a) * t }
}

/// The bar for `progress`, as wide as it is offered.
struct RunProgressBar: View {
    let progress: RunProgress
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var tween: ProgressTween

    /// The tween starts out holding the reading, so the very first frame has
    /// a bar to paint: it doesn't wait on a change handler to hand it over.
    init(progress: RunProgress) {
        self.progress = progress
        var tween = ProgressTween()
        tween.observe(ProgressReading(progress), now: Date().timeIntervalSinceReferenceDate, animated: true)
        _tween = State(initialValue: tween)
    }

    var body: some View {
        // Read here, not only in the Canvas closure, so a reading that lands
        // redraws the bar.
        let shown = tween
        TimelineView(.animation(paused: reduceMotion)) { timeline in
            let now = timeline.date.timeIntervalSinceReferenceDate
            let period = ProgressBarGeometry.flowPeriod
            let phase = reduceMotion ? 0 : now.truncatingRemainder(dividingBy: period) / period
            // Reduce Motion paints where the glide ends (its clock is paused).
            let frame = shown.frame(at: reduceMotion ? .infinity : now)
            Canvas { context, size in
                frame?.paint(in: &context, size: size, phase: phase, ink: Theme.text)
            }
        }
        .frame(height: ProgressBarGeometry.height)
        .onChange(of: ProgressReading(progress)) { _, reading in
            tween.observe(reading, now: Date().timeIntervalSinceReferenceDate, animated: !reduceMotion)
        }
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue(spoken)
    }

    /// "42%, Port the bar, 3 steps left".
    private var spoken: String {
        let label = progress.shownStages.last?.label ?? progress.current
        return "\(progress.shownPercent)%, \(label), \(RunProgress.leftNote(progress.stepsLeftCount))"
    }
}

/// The desktop's stage tips in one card, opened by tapping the strip: the
/// stages passed (where each began), the current one with how far the turn
/// has got and how many steps are left, then the steps still to come. Each
/// row leads with its state: a green check when done, a pulsing ring in the
/// text colour for the step under way, a dashed circle for those not started.
struct RunProgressCard: View {
    let progress: RunProgress

    /// A row's place in the turn.
    enum StepState {
        case done, current, upcoming

        var symbol: String {
            switch self {
            case .done: "checkmark.circle.fill"
            case .current: "smallcircle.filled.circle"
            case .upcoming: "circle.dashed"
            }
        }

        var iconTone: Color {
            switch self {
            case .done: Theme.success
            // The bar's current ring is its darkest grey; so is this one.
            case .current: Theme.text
            case .upcoming: Theme.textFaint
            }
        }

        var labelTone: Color { self == .current ? Theme.text : Theme.textMuted }

        /// What VoiceOver says before the step.
        var spoken: String {
            switch self {
            case .done: "Done"
            case .current: "In progress"
            case .upcoming: "Not started"
            }
        }
    }

    var body: some View {
        let stages = progress.shownStages
        let left = progress.stepsLeftCount
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(stages.dropLast().enumerated()), id: \.offset) { _, stage in
                StepRow(state: .done, label: stage.label, note: "\(stage.percent)%")
            }
            StepRow(state: .current, label: stages.last?.label ?? progress.current, note: progress.note)
            ForEach(Array(progress.next.enumerated()), id: \.offset) { _, step in
                StepRow(state: .upcoming, label: step, note: nil)
            }
            if left > progress.next.count {
                StepRow(state: .upcoming, label: "and \(left - progress.next.count) more", note: nil, faint: true)
            }
        }
        .font(Theme.sans(13, relativeTo: .footnote))
        .fixedSize(horizontal: false, vertical: true)
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(maxWidth: 300, alignment: .leading)
        .presentationCompactAdaptation(.popover)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("run-progress-card")
    }
}

/// One row of the card: its state's icon, the step, and a faint note.
private struct StepRow: View {
    let state: RunProgressCard.StepState
    let label: String
    let note: String?
    /// The "and 3 more" row: a count, not a step.
    var faint = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 7) {
            Image(systemName: state.symbol)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(state.iconTone.opacity(faint ? 0.6 : 1))
                .symbolEffect(.pulse, options: .repeating, isActive: state == .current && !reduceMotion)
                .frame(width: 14)
            Text(label)
                .font(Theme.sans(13, weight: state == .current ? .medium : .regular, relativeTo: .footnote))
                .foregroundStyle(faint ? Theme.textFaint : state.labelTone)
            if let note {
                Text(note)
                    .font(Theme.sans(11, relativeTo: .caption))
                    .foregroundStyle(Theme.textFaint)
                    .monospacedDigit()
                    .fixedSize()
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([faint ? nil : state.spoken, label, note].compactMap { $0 }.joined(separator: ", "))
    }
}

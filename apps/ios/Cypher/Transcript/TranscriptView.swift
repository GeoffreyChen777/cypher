// Transcript — virtualized block-granularity rows with stick-to-bottom.
//
// Desktop parity (transcript.rs): GAP_TURN 14 / GAP_BLOCK 8 / MD_BLOCK_GAP 12,
// content column max 736, re-engage band 70, jump-button threshold 320,
// bottom pad 24. Rows are identified by stable ids and versioned by content
// fingerprints, so a streamed token re-renders exactly one row. SwiftUI's lazy
// stack + scroll APIs stand in for gpui's list(): the pin breaks only on
// user scroll-up and re-engages when approaching the bottom.

import SwiftUI

struct TranscriptView: View {
    let store: SessionStore
    let chatId: String
    /// Owned by SessionView so IT can report the composer inset's global
    /// frame into `insetTopGlobalY` — the measured truth `correctPin`
    /// re-pins against.
    let scroll: ScrollState

    init(store: SessionStore, chatId: String, scroll: ScrollState) {
        self.store = store
        self.chatId = chatId
        self.scroll = scroll
        // NOT seeded from store.hasRevealed anymore. That seed (meant to stop
        // a blink on mid-typing view re-creation) un-gated every warm RE-OPEN:
        // a new view lays the whole LazyVStack out from scratch — estimates,
        // churn, mis-anchor — and the user watched it, or worse, was left in
        // the blank over-estimate region ("cached session opens blank every
        // time"). Every new view identity now earns its reveal through
        // settleToBottom, which converges on the MEASURED pin error and is
        // ~1 frame for content that lays out where the anchor put it.
        _hydrated = State(initialValue: !store.entries.isEmpty || !store.pendingSends.isEmpty)
    }

    nonisolated static let gapTurn: CGFloat = 14
    nonisolated static let gapExchange: CGFloat = 36
    nonisolated static let gapBlock: CGFloat = 8
    static let maxContentWidth: CGFloat = 736
    static let stickThreshold: CGFloat = 70
    static let jumpThreshold: CGFloat = 320

    @State private var veils = VeilStore()
    @State private var folds: [String: Bool] = [:]
    /// Toggles the reader tapped (a translation's original, a thought, a
    /// work run) and the state they left them in; every other one keeps its
    /// default (transcript.rs `toggle_pins`).
    @State private var togglePins: [String: Bool] = [:]
    /// Per tool group row: the part ids of chips whose detail is open.
    @State private var openChips: [String: Set<String>] = [:]
    @State private var turns = TurnTracker()
    /// One-shot guard for the first non-empty projection.
    @State private var hydrated = false
    /// Gates the reveal: false until the transcript has landed at the bottom.
    @State private var settled = false
    @State private var scrollPosition = ScrollPosition(edge: .bottom)
    @State private var hydrationTask: Task<Void, Never>?
    /// Tags this view's pad reports: the ScrollState outlives view identities,
    /// and another identity's pad frame must never count as ours.
    @State private var padOwner = UUID()
    /// Rows hidden above the rendered window; nil until the first rows fix it.
    /// Only ever lowered ("Show earlier messages"), so streamed appends never
    /// drop rows off the top under a reader.
    @State private var windowFloor: Int?
    /// Rendered tail on open, and each page "Show earlier" adds. Bounds what
    /// the lazy stack can realize: jumping to the bottom of an estimated
    /// transcript made iOS 26 lay out every row on the way (3,000 rows:
    /// ~1,800 text views, seconds of main thread), which no scroll API avoids.
    static let windowRows = 200
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        // The parse cache lives on the store (one per session, prewarmed
        // off-main), so opening a chat assembles rows from settled parses
        // instead of re-parsing the whole transcript on the main thread.
        let allRows = transcriptRows()
        let floor = windowStart(of: allRows)
        let rows = floor > 0 ? Array(allRows[floor...]) : allRows
        let rounds = store.transcriptCache.rounds
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if floor > 0 {
                    // Explicit, not load-on-reach: reaching the top is always
                    // mid-gesture, and SwiftUI drops a programmatic scroll
                    // under the finger — the reader's place couldn't be held.
                    Button {
                        loadEarlierRows()
                    } label: {
                        Text("Show earlier messages")
                            .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                            .foregroundStyle(Theme.textMuted)
                            .padding(.horizontal, 14)
                            .frame(height: 32)
                            .background(whiteAlpha(0.06), in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("transcript-show-earlier")
                    .frame(maxWidth: .infinity)
                    .padding(.top, 8)
                    .padding(.bottom, 4)
                }
                ForEach(rows) { row in
                    rowView(row).id(row.id)
                }
                Color.clear.frame(height: 44)  // bottom pad clears the fade + floating status strip
                    // The pad's on-screen frame is the one bottom-position
                    // reading the keyboard can't distort (see correctPin).
                    .onGeometryChange(for: CGFloat.self) {
                        $0.frame(in: .global).maxY
                    } action: { [scroll, padOwner] new in
                        scroll.padGlobalMaxY = new
                        scroll.padOwner = padOwner
                        correctPin()
                    }
            }
            .frame(maxWidth: Self.maxContentWidth)
            .frame(maxWidth: .infinity)
        }
        .accessibilityIdentifier("chat-transcript")
        // The indicator would run through the turn scrubber's ticks; it
        // steps aside only while they show.
        .scrollIndicators(turns.revealed ? .hidden : .automatic)
        .scrollPosition($scrollPosition)
        .scrollEdgeEffectStyle(.soft, for: .bottom)
        .defaultScrollAnchor(.bottom)
        // Drag past the composer and the keyboard follows the finger down —
        // the Messages-style interactive dismissal.
        .scrollDismissesKeyboard(.interactively)
        // Tap anywhere in the transcript to put the keyboard away (t3's
        // tap-to-blur; TapGesture already cancels on drag-sized movement).
        // Simultaneous so fold toggles and row buttons still receive theirs.
        .simultaneousGesture(
            SpatialTapGesture(coordinateSpace: .global).onEnded { event in
                TranscriptKeyboardDismissal.dismiss(at: event.location)
            }
        )
        // Held invisible until it has settled at the bottom, then faded in.
        // The settling itself is unavoidable (see settleToBottom) — what is
        // avoidable is WATCHING it: painting mid-settle is what read as the
        // transcript sliding on load.
        //
        // Both fades are SCOPED to their own paint. A value-keyed animation on
        // this whole view swept the scroll view's re-anchoring into the fade
        // when rows landed while hidden — a programmatic scroll whose phase
        // never returns to idle, which blocked the settle and every later
        // correctPin for the life of the view.
        .animation(reduceMotion ? nil : Motion.fadeQuick) { $0.opacity(settled ? 1 : 0) }
        // While a big transcript is finding its bottom, show the skeleton — a
        // black void here read as "the session is broken" (and on a slow
        // settle it WAS multiple seconds of void).
        .overlay {
            ZStack {
                if !settled {
                    TranscriptSkeleton()
                        .background(Theme.bg)
                }
            }
            .motionAnimation(Motion.fadeQuick, value: settled)
        }
        .background(Theme.bg)
        .task {
            // SessionView calls this on keyboard didShow/didHide — the one
            // forced correction that ends each keyboard transition.
            scroll.requestCorrection = { correctPin(force: true) }
            // Warm sessions already have rows at first layout, and `onChange`
            // never fires for an initial value — this is the only hook for them.
            fixWindow()
            await settleToBottom()
        }
        .onChange(of: allRows.isEmpty) { _, isEmpty in
            // Projection is off-main, so a cached transcript usually lands after
            // the pass above ran on an empty list. Only ever hides a transcript
            // that has never been shown — re-hiding a visible one is what made
            // it blink out mid-typing. `hydrated` is the one-shot: it seeds
            // true for stores that have already revealed content, so this
            // fires exactly once, on a fresh store's first non-empty rows.
            if !isEmpty { fixWindow() }
            guard !isEmpty, !hydrated else { return }
            hydrated = true
            settled = false
            // The empty list's pad frame is stale for the landed rows — and on
            // an empty list aligned to the bottom it sits right on the
            // boundary, faking convergence. Cleared HERE, before the new
            // layout reports; clearing inside the async settle raced that
            // report, and a transcript that landed already in place never
            // moved to report again (a full-budget loader).
            scroll.padGlobalMaxY = 0
            hydrationTask?.cancel()
            hydrationTask = Task { await settleToBottom() }
        }
        .onScrollGeometryChange(for: CGFloat.self) {
            $0.contentSize.height
        } action: { [scroll] old, new in
            scroll.contentHeight = new
            if let id = scroll.restoreTopRowId, new > old {
                // The previous page just landed above the reader.
                scroll.restoreTopRowId = nil
                scrollPosition.scrollTo(id: id, anchor: .top)
            }
            // Estimated row heights keep resolving after the settle, and a
            // SHRINK leaves the held offset past the content — a black
            // viewport until the user's scroll clamps it (t3 re-pins on
            // itemLayout for exactly this). Keep a pinned feed glued through
            // reflows; never touch a user's in-flight drag.
            if scroll.pinned, !scroll.userScrolling, new < old - 1 {
                correctPin()
            }
        }
        .onScrollGeometryChange(for: CGFloat.self) { geo in
            // In ScrollPosition's space, not UIKit's: `contentOffset` counts
            // the top inset (the nav bar — it reads -116 at the top), while
            // `scrollTo(y:)` takes the content y shown at the inset-adjusted
            // top. Mixing them landed every measured nudge one top inset
            // short: clamped to 0 on short transcripts, parked a nav bar
            // above the tail on long ones — so the reveal never converged
            // (the ~3s loader on open) and the pin left the tail under the
            // composer.
            geo.contentOffset.y + geo.contentInsets.top
        } action: { [scroll] _, new in
            scroll.scrollY = new
            scroll.motion.geometryChanged(now: Date().timeIntervalSinceReferenceDate)
        }
        .onScrollGeometryChange(for: CGFloat.self) {
            $0.containerSize.height + $0.contentInsets.bottom
        } action: { [scroll] _, new in
            scroll.viewportHeight = new
            // The viewport resized under the content — keyboard up/down, the
            // composer's capsule↔card morph, the question-panel swap. t3's
            // rule for exactly this ("keyboardLiftBehavior=whenAtEnd" +
            // inset re-report on remount): a feed pinned at the end follows
            // the new bottom IMMEDIATELY; an unpinned reader stays put —
            // unless the resize stranded the offset past the content (blank
            // viewport), which is clamped back to the bottom. Without this,
            // focusing the composer could leave the transcript scrolled out
            // of range (blank until touched) or resting a viewport short
            // (content "appears" only when the next resize brought it back).
            guard !scroll.userScrolling else { return }  // their drag wins
            if scroll.pinned || scroll.distanceFromBottom < -1 {
                // t3's keyboardLiftBehavior=whenAtEnd: no per-frame chasing
                // while the boundary animates (that fight was the stutter,
                // and the edge math lies by the keyboard inset anyway) —
                // correctPin trails the transition and lands one measured,
                // animated lift when the boundary goes quiet.
                correctPin()
            }
        }
        .onScrollPhaseChange { [scroll] _, newPhase in
            // Desktop rule: the pin breaks only on USER input (wheel-up/drag),
            // never on streaming growth. Phases track the gesture.
            scroll.motion.changed(to: newPhase, now: Date().timeIntervalSinceReferenceDate)
            // Invalidate callbacks queued by a previous gesture/transition.
            // In particular, a second touch starts at tracking, not interacting.
            scroll.correctionGeneration &+= 1
            scroll.correctionScheduled = false
            // A gesture can END stranded past the content (a shrink landed
            // mid-drag; the in-flight clamps all yield to the user). Once the
            // scroll view goes quiet there is no later geometry event to
            // catch it — clamp here or the viewport stays blank.
            if newPhase == .idle {
                correctPin()  // self-gates: pinned re-glue or stranded clamp
            }
        }
        .onScrollGeometryChange(for: CGFloat.self) { geo in
            // Inset-proof distance: visibleRect is already contentInsets-
            // adjusted, so this is 0 at the VISUAL bottom whether or not the
            // keyboard is up. The old offset+insets formula double-counted
            // the keyboard inset (~keyboard-height at the bottom), which
            // both pinned the jump button on and put the 70pt re-stick band
            // permanently out of reach while typing. Unclamped on purpose:
            // negative can be native rubber-banding; only treat it as a
            // stranded layout after the motion/quiet gate releases.
            geo.contentSize.height - geo.visibleRect.maxY
        } action: { [scroll] old, new in
            scroll.distanceFromBottom = new
            if scroll.userScrolling, new > old + 1, new > 2 {
                scroll.pinned = false
            } else if !scroll.pinned, new <= Self.stickThreshold, new < old {
                // Re-stick only when moving TOWARD the bottom inside the 70pt
                // band, else the pin would be unbreakable.
                scroll.pinned = true
            } else if !scroll.userScrolling {
                if new < -1 {
                    // Stranded past the content end (blank viewport). The
                    // measured corrector resolves it — and self-defers while
                    // the composer boundary is mid-transition, so it never
                    // fights a keyboard animation the way raw edge-scrolls
                    // here used to.
                    correctPin()
                } else if scroll.pinned, new > old + 1, new > Self.stickThreshold {
                    // The bottom moved out from under a pinned feed with no
                    // user input — estimated heights resolving AFTER the
                    // settle loop exited. Growth-only (`new > old`), so the
                    // streamed-append spring, which closes the distance frame
                    // by frame, is never fought.
                    correctPin()
                }
            }
            // The only observable write, and only at the threshold crossing —
            // it re-renders the tiny jump button, never this body. Gated on
            // the PIN, not just distance: with the keyboard up, UIKit
            // double-counts the bottom inset (t3's "nativeInsetOvercount"),
            // so distance reads ~keyboard-height at the visual bottom and the
            // raw threshold kept the button up for a pinned feed.
            let show = !scroll.pinned && new > Self.jumpThreshold
            if scroll.showJump != show { scroll.showJump = show }
            let atBottom = scroll.pinned || new <= Self.stickThreshold
            if turns.atBottom != atBottom { turns.atBottom = atBottom }
        }
        .onChange(of: contentSignature(rows)) {
            // Until the reveal, settleToBottom owns positioning (it follows
            // growth by measurement). An animated scroll started while the
            // transcript is hidden never reports its phase back to idle —
            // `.animating` stuck for the view's life, which blocked the settle
            // (a ~4s loader when a cold transcript landed late) and every
            // later correctPin. `hydrated` covers the first rows arriving
            // before the rows-arrived handler has flipped `settled`.
            guard settled, hydrated else { return }
            guard scroll.pinned else { return }
            guard !scroll.keyboardTransitioning,
                !scroll.motion.blocksContentFollowing(now: Date().timeIntervalSinceReferenceDate)
            else {
                // The idle correction uses the latest geometry, coalescing
                // all tokens that arrived while the user owned the scroll.
                correctPin()
                return
            }
            if reduceMotion {
                scrollPosition.scrollTo(edge: .bottom)
            } else {
                // correctPin must not snap-cancel this spring mid-flight.
                scroll.animatingUntil = Date().timeIntervalSinceReferenceDate + 0.35
                withAnimation(.spring(duration: 0.3)) {
                    scrollPosition.scrollTo(edge: .bottom)
                }
            }
        }
        .overlay(alignment: .top) {
            // Soft fade under the nav bar — content dissolves instead of
            // hard-clipping against the header.
            LinearGradient(
                stops: [
                    .init(color: Theme.bg, location: 0),
                    .init(color: Theme.bg.opacity(0.85), location: 0.45),
                    .init(color: Theme.bg.opacity(0), location: 1),
                ],
                startPoint: .top, endPoint: .bottom
            )
            .frame(height: 130)
            .ignoresSafeArea(edges: .top)
            .allowsHitTesting(false)
        }
        .overlay(alignment: .trailing) {
            // Revealed with the transcript: it indexes rows being settled.
            if rounds.count > 1, settled {
                TurnScrubber(rounds: rounds, tracker: turns) { jumpToRound($0) }
                    .transition(.opacity)
            }
        }
        // The bottom dissolve lives on SessionView's composer inset (one
        // continuous gradient from above the status strip to the physical
        // bottom edge) — a second ramp here would double-darken the rows
        // right where they slide under the glass.
        .overlay(alignment: .bottomTrailing) {
            // Jump-to-bottom floats ABOVE the fades. A child view so only IT
            // observes the show flag — the transcript body stays out of the
            // per-frame scroll path.
            JumpToBottomButton(scroll: scroll) {
                scroll.pinned = true
                scroll.animatingUntil = Date().timeIntervalSinceReferenceDate + 0.4
                withAnimation(.spring(duration: 0.35)) {
                    scrollPosition.scrollTo(edge: .bottom)
                }
            }
            .padding(.trailing, 16)
            // 12pt above the COMPOSER, not the safe-area edge: the edge
            // now sits atop the 24pt status-strip band (SessionView's
            // inset), so dip into it — the strip never hit-tests.
            .padding(.bottom, -12)
        }
        .onDisappear {
            hydrationTask?.cancel()
            hydrationTask = nil
            scroll.correctionGeneration &+= 1
            scroll.correctionScheduled = false
            scroll.requestCorrection = {}
        }
    }

    /// One trailing correction, invalidated by the next touch or view exit.
    /// Re-checking the gate on wake also handles geometry that arrives after
    /// the idle phase callback, without clamping an unfinished rubber-band.
    private func scheduleCorrection(after delay: TimeInterval) {
        guard !scroll.correctionScheduled else { return }
        scroll.correctionScheduled = true
        let generation = scroll.correctionGeneration
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(max(0.01, delay)))
            guard !Task.isCancelled, generation == scroll.correctionGeneration else { return }
            scroll.correctionScheduled = false
            correctPin()
        }
    }

    /// Measured re-pin. The scroll-geometry numbers LIE while the keyboard is
    /// up: UIKit's interactive-dismiss avoidance and the SwiftUI safe area
    /// each count the keyboard inset once (the jump-button comment's
    /// "nativeInsetOvercount"), so `scrollTo(edge: .bottom)` overshoots by
    /// ~keyboard height — the transcript parks with a blank band under it, or
    /// wholly off-viewport on a tall keyboard (the "blank screen when I open
    /// the composer"). And because `distanceFromBottom` is derived from the
    /// same lying insets, it reads ≈0 there, which is why clamps built on it
    /// never caught this. On-screen GLOBAL frames don't lie: when pinned,
    /// nudge the offset by the measured gap between the bottom pad and the
    /// composer boundary. Falls back to the edge scroll until both frames
    /// have reported (first layout), and stays out of the way of user drags
    /// and in-flight programmatic springs.
    private func correctPin(force: Bool = false) {
        guard !scroll.motion.isMoving else { return }
        let now = Date().timeIntervalSinceReferenceDate
        if scroll.motion.blocksPositioning(now: now) {
            scheduleCorrection(after: scroll.motion.quietUntil - now)
            return
        }
        // The keyboard's whole transition is one no-correct window (UIKit
        // will/did notifications, flipped by SessionView) — the focus
        // sequence runs TWO boundary animations (composer morph, then the
        // keyboard) with a gap between them, and a quiet-gap glide firing
        // between the two was the double-adjust stutter. SessionView requests
        // exactly one forced correction on didShow/didHide.
        guard force || !scroll.keyboardTransitioning else { return }
        guard now >= scroll.animatingUntil else {
            scheduleCorrection(after: scroll.animatingUntil - now)
            return
        }
        // While the composer boundary is mid-flight (card morph, panel swap)
        // nothing corrects — chasing a moving target was the stutter.
        // Trail instead: skip now, re-check after the boundary goes quiet.
        if !force, now - scroll.insetTopChangedAt < 0.12 {
            scheduleCorrection(after: 0.15)
            return
        }
        scroll.motion.didSettle()
        guard let pad = padFrame, scroll.insetTopGlobalY > 0 else {
            if scroll.pinned { jumpToBottomRow() }
            return
        }
        // < 0 after native motion settles: overshot past the end due to a
        //      reflow — not the legitimate rubber-band we yield to above.
        // > 0: tail parked short of the boundary — only wrong for a PINNED
        //      feed (an unpinned reader mid-history always measures > 0).
        let error = pad - scroll.insetTopGlobalY
        guard error < -2 || (scroll.pinned && error > 2) else { return }
        // A keyboard-sized lift reads as motion — glide it. Tiny nudges (late
        // row measurements) stay instant and invisible. So does anything
        // while the transcript is hidden for its reveal, or longer than a
        // viewport: an animated scroll renders every row it passes (a long
        // transcript's estimate error glided through thousands of rows), and
        // one started while hidden never reports its phase back to idle.
        if abs(error) > scroll.viewportHeight {
            // See jumpToBottomRow: a long correction goes by row, not offset.
            jumpToBottomRow()
        } else if settled, abs(error) > 48 {
            scroll.animatingUntil = now + 0.35
            withAnimation(.spring(duration: 0.3)) {
                scrollPosition.scrollTo(y: scroll.scrollY + error)
            }
        } else {
            scrollPosition.scrollTo(y: scroll.scrollY + error)
        }
    }

    /// The rendered rows: the store's cached build with closed translation
    /// originals folded away. Every index into the transcript (window floor,
    /// rounds, jumps) is into this array.
    private func transcriptRows() -> [TranscriptRow] {
        store.transcriptCache.rows(
            revision: store.revision, entries: store.entries,
            pendingSends: store.pendingSends, togglePins: togglePins)
    }

    private func windowStart(of rows: [TranscriptRow]) -> Int {
        min(windowFloor ?? max(0, rows.count - Self.windowRows), rows.count)
    }

    /// Pins the window's start at the first rows this view renders.
    private func fixWindow() {
        guard windowFloor == nil else { return }
        let rows = transcriptRows()
        guard !rows.isEmpty else { return }
        windowFloor = max(0, rows.count - Self.windowRows)
    }

    /// Prepends the previous page, keeping the row that was first (just
    /// under the button) at the top — by id, once the page has laid out (it's
    /// realized, so no walk). The bottom scroll anchor does NOT hold a
    /// reader's place here: it kept the top offset, showing the new page.
    private func loadEarlierRows() {
        let rows = transcriptRows()
        let floor = windowStart(of: rows)
        guard floor > 0, floor < rows.count else { return }
        windowFloor = max(0, floor - Self.windowRows)
        // Restored once the prepended rows have laid out (content height).
        scroll.restoreTopRowId = rows[floor].id
    }

    /// Puts a round's prompt at the top. Takes the scroll from the pin (a
    /// pinned feed would be pulled straight back to the tail), and glides
    /// only short hops: an animated scroll renders every row it passes.
    private func jumpToRound(_ index: Int) {
        let rows = transcriptRows()
        let rounds = store.transcriptCache.rounds
        guard rounds.indices.contains(index) else { return }
        let target = rounds[index]
        let fromRow =
            turns.current.flatMap { rounds.indices.contains($0) ? rounds[$0].rowIndex : nil }
            ?? rows.count
        scroll.pinned = false
        turns.set(index)
        let floor = windowStart(of: rows)
        if target.rowIndex < floor {
            // Above the rendered window: open it down to the target (same
            // update as the scroll, so the id resolves against the new rows).
            windowFloor = max(0, min(floor - Self.windowRows, target.rowIndex))
        }
        let glide = !reduceMotion && target.rowIndex >= floor && abs(target.rowIndex - fromRow) <= 40
        scroll.animatingUntil = Date().timeIntervalSinceReferenceDate + (glide ? 0.4 : 0)
        if glide {
            withAnimation(.smooth(duration: 0.3)) {
                scrollPosition.scrollTo(id: target.rowId, anchor: .top)
            }
        } else {
            scrollPosition.scrollTo(id: target.rowId, anchor: .top)
        }
        // A glide aims at the lazy stack's ESTIMATED offset for rows it hasn't
        // measured, and a fast scrub retargets mid-flight — either can leave
        // it rounds short. Once motion ends, land the latest target by id.
        scroll.turnJump &+= 1
        let jump = scroll.turnJump
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(glide ? 380 : 120))
            guard jump == scroll.turnJump, !scroll.userScrolling else { return }
            scrollPosition.scrollTo(id: target.rowId, anchor: .top)
        }
    }

    /// The bottom pad's measured global maxY, if THIS view reported it since
    /// the last invalidation.
    private var padFrame: CGFloat? {
        scroll.padOwner == padOwner && scroll.padGlobalMaxY > 0 ? scroll.padGlobalMaxY : nil
    }

    /// Places the LAST ROW at the viewport's bottom by id: the lazy stack
    /// realizes it and its neighbours directly, and the measured nudge then
    /// adds the pad. `scrollTo(edge:)`/`(y:)` go by the estimated content
    /// height instead — on a long transcript that lands in the over-estimated
    /// blank, and correcting from there laid out every row in between (3,000
    /// rows: ~2,300 text views, seconds on the main thread). The id must be a
    /// ForEach element's: the pad's own `.id` is unresolvable while unrealized.
    private func jumpToBottomRow() {
        let rows = transcriptRows()
        if let last = rows.last {
            scrollPosition.scrollTo(id: last.id, anchor: .bottom)
        } else {
            scrollPosition.scrollTo(edge: .bottom)
        }
    }

    /// Hold the bottom until the MEASURED pin is right, then reveal.
    ///
    /// A lazy stack only measures the rows near the viewport; the rest carry
    /// ESTIMATED heights that resolve over the next frames, moving the real
    /// bottom. The old loop compared content heights across 30ms polls (16
    /// max) — on a big transcript the height was still churning when it gave
    /// up, revealing wherever the churn was: sometimes mid-transcript,
    /// sometimes in the blank over-estimate region past the content.
    ///
    /// Convergence is now the same truth correctPin uses: the bottom pad's
    /// on-screen frame meeting the composer boundary. A jump to the last row
    /// (by id, never by estimated offset) brings the pad into range; measured
    /// nudges close the remainder; an in-tolerance read reveals.
    /// Bounded (~2s worst case), and it yields the moment the user takes the
    /// scroll view. `settled` flips either way — never left invisible.
    private func settleToBottom() async {
        for _ in 0..<60 {
            guard !Task.isCancelled, scroll.pinned, !scroll.userScrolling else { break }
            // Don't chase targets through a keyboard transition — the edge
            // math lies there and the budget burns on garbage jumps. The
            // didShow correction handles that endpoint; just wait it out.
            if scroll.keyboardTransitioning
                || scroll.motion.blocksPositioning(now: Date().timeIntervalSinceReferenceDate)
            {
                try? await Task.sleep(nanoseconds: 60_000_000)
                continue
            }
            if let pad = padFrame, scroll.insetTopGlobalY > 0 {
                let error = pad - scroll.insetTopGlobalY
                // Near-pinned is good enough to reveal — correctPin's trailing
                // nudges close the last few points invisibly, while every
                // 50ms spent here is the user staring at the loader.
                if abs(error) < 24 { break }
                if abs(error) > scroll.viewportHeight {
                    // Off by more than a screen: the estimated heights are
                    // wrong, and an offset jump would make the lazy stack lay
                    // out every row in between (thousands on a long session).
                    jumpToBottomRow()
                } else {
                    scrollPosition.scrollTo(y: scroll.scrollY + error)
                }
            } else {
                jumpToBottomRow()
            }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        guard !Task.isCancelled else { return }
        settled = true
        // Revealed-with-CONTENT only: a settle that ran against a still-empty
        // projection must not latch, or the rows landing a beat later find
        // `hasRevealed` true, seed `hydrated`/`settled` from it on the next
        // view identity, and skip their own settle — the transcript then
        // rests wherever the empty pass left it (out of view until a resize
        // — focusing the composer — happened to drag it back).
        if !store.entries.isEmpty || !store.pendingSends.isEmpty {
            store.hasRevealed = true
        }
    }

    // Streamed growth signature: last row id + version + count. Any append or
    // reflow of the tail bumps it; scroll-back through history doesn't.
    private func contentSignature(_ rows: [TranscriptRow]) -> String {
        guard let last = rows.last else { return "" }
        return "\(rows.count)|\(last.id)|\(last.version)"
    }

    // MARK: Row rendering

    @ViewBuilder
    private func rowView(_ row: TranscriptRow) -> some View {
        Group {
            switch row.kind {
            case .user(let text, let isSteer):
                UserBubble(
                    text: text, pending: row.timestamp == nil,
                    isSteer: isSteer,
                    deviceId: store.hostDeviceId ?? "")

            case .prose(let blocks, let streaming):
                ProseRowView(row: row, blocks: blocks, streaming: streaming, veils: veils)

            case .markdown(let block, _):
                // A thought's code block or table: its own colors, quieted.
                MarkdownBlockView(block: block, cacheKey: row.id)
                    .opacity(row.muted ? 0.75 : 1)

            case .toolGroup(let tools, let autoOpen):
                ToolGroupView(
                    tools: tools,
                    open: row.nested || (folds[row.id] ?? autoOpen),
                    nested: row.nested,
                    userToggled: folds[row.id] != nil,
                    openChips: openChips[row.id] ?? [],
                    toggleChip: { partId in
                        withAnimation(reduceMotion ? nil : Motion.resize) {
                            openChips[row.id, default: []].formSymmetricDifference([partId])
                        }
                    }
                ) {
                    withAnimation(reduceMotion ? nil : Motion.resize) {
                        folds[row.id] = !(folds[row.id] ?? autoOpen)
                    }
                }

            case .inputChip(let header, let resolved):
                InputChipView(header: header, resolved: resolved)

            case .errorChip(let message):
                ErrorChipView(message: message)

            case .translationOriginal:
                let open = TranscriptRowBuilder.isOpen(row, pins: togglePins)
                FoldToggle(
                    open: open, closedLabel: "Show original",
                    openLabel: "Hide original", identifier: "translation-original-toggle"
                ) {
                    togglePins[row.id] = !open
                }

            case .activity(_, let summary, _):
                let open = TranscriptRowBuilder.isOpen(row, pins: togglePins)
                FoldToggle(
                    open: open, closedLabel: summary, openLabel: summary,
                    identifier: "activity-toggle"
                ) {
                    togglePins[row.id] = !open
                }

            case .thought(_, let live, let preview):
                let open = TranscriptRowBuilder.isOpen(row, pins: togglePins)
                if row.nested {
                    ThoughtChipRow(live: live, preview: preview, open: open) {
                        togglePins[row.id] = !open
                    }
                } else {
                    let label = live ? "Thinking…" : "Thought"
                    FoldToggle(
                        open: open, closedLabel: label,
                        openLabel: label, identifier: "thought-toggle"
                    ) {
                        togglePins[row.id] = !open
                    }
                }
            }
        }
        // A work run's thought text sits under its chip, lined up with the
        // chips' icons.
        .padding(.leading, row.nested && row.partKey != nil ? ThoughtChipRow.textInset : 0)
        .padding(.top, row.topGap)
        .padding(.horizontal, 16)
        .environment(
            \.transcriptEntry,
            TranscriptEntryContext(
                entryId: row.entryId, role: row.role,
                settled: row.timestamp != nil)
        )
        .modifier(TurnAnchor(round: store.transcriptCache.roundIndex[row.id], tracker: turns, scroll: scroll))
    }
}

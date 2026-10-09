// Transcript scroll tracking and the jump-to-bottom button.

import SwiftUI

/// Per-frame scroll tracking for the transcript. Only `showJump` is
/// observable — everything else is written every scroll frame and read only
/// inside closures/the settle loop, so observation (and with it, whole-body
/// re-evaluation per frame) must not see those writes.
@Observable
final class ScrollState {
    /// Flips at the jump threshold; observed by JumpToBottomButton alone.
    var showJump = false
    @ObservationIgnored var distanceFromBottom: CGFloat = 0
    @ObservationIgnored var contentHeight: CGFloat = 0
    /// Scroll position in `ScrollPosition.scrollTo(y:)` coordinates (the
    /// content offset plus the top inset). Measured nudges add to this.
    @ObservationIgnored var scrollY: CGFloat = 0
    /// Container plus bottom inset: the longest correction still glided.
    @ObservationIgnored var viewportHeight: CGFloat = 0
    @ObservationIgnored var pinned = true
    @ObservationIgnored var motion = TranscriptScrollMotion()
    var userScrolling: Bool { motion.userScrolling }
    /// Programmatic spring deadline — correctPin stays quiet until it passes.
    @ObservationIgnored var animatingUntil: TimeInterval = 0
    /// Measured on-screen frames for correctPin: the transcript's bottom pad
    /// (written here) and the composer inset's top edge (written by
    /// SessionView, which owns the inset).
    @ObservationIgnored var padGlobalMaxY: CGFloat = 0
    @ObservationIgnored var padOwner: UUID?
    /// Latest turn-scrubber jump; an older jump's trailing landing yields.
    @ObservationIgnored var turnJump: UInt64 = 0
    /// Window paging: the row to hold at the top once a prepended page lands.
    @ObservationIgnored var restoreTopRowId: String?
    @ObservationIgnored var insetTopGlobalY: CGFloat = 0
    /// When the boundary last moved — correctPin trails transitions.
    @ObservationIgnored var insetTopChangedAt: TimeInterval = 0
    /// True between UIKit's keyboardWillShow/Hide and didShow/Hide — the
    /// no-correct window (flipped by SessionView, which owns the notifications).
    @ObservationIgnored var keyboardTransitioning = false
    /// Trailing re-check dedupe: one scheduled correction at a time.
    @ObservationIgnored var correctionScheduled = false
    @ObservationIgnored var correctionGeneration: UInt64 = 0
    /// Set by TranscriptView; SessionView invokes it on didShow/didHide.
    @ObservationIgnored var requestCorrection: () -> Void = {}
}

struct JumpToBottomButton: View {
    let scroll: ScrollState
    let action: () -> Void

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            if scroll.showJump {
                Button(action: action) {
                    Image(systemName: "arrow.down")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(Theme.text)
                        .frame(width: 36, height: 36)
                }
                .glassEffect(.regular.interactive(), in: Circle())
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            }
        }
        .motionAnimation(Motion.fadeQuick, value: scroll.showJump)
    }
}

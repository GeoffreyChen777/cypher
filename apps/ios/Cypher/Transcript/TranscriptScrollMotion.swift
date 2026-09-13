import SwiftUI

/// Automatic positioning must yield from the first touch (tracking), through
/// dragging/deceleration, until the last bounce geometry has settled. Negative
/// bottom distance during this interval is rubber-banding, not a broken layout.
struct TranscriptScrollMotion {
    static let quietInterval: TimeInterval = 0.15
    private(set) var phase: ScrollPhase = .idle
    private(set) var waitingForQuiet = false
    private(set) var quietUntil: TimeInterval = 0

    var userScrolling: Bool {
        phase == .tracking || phase == .interacting || phase == .decelerating
    }

    var isMoving: Bool { phase != .idle }

    mutating func changed(to phase: ScrollPhase, now: TimeInterval) {
        self.phase = phase
        if phase != .idle {
            waitingForQuiet = true
        } else if waitingForQuiet {
            quietUntil = now + Self.quietInterval
        }
    }

    mutating func geometryChanged(now: TimeInterval) {
        if phase == .idle, waitingForQuiet {
            quietUntil = now + Self.quietInterval
        }
    }

    func blocksPositioning(now: TimeInterval) -> Bool {
        isMoving || (waitingForQuiet && now < quietUntil)
    }

    /// Streaming may smoothly retarget its own spring, but must never take
    /// ownership from a touch, native momentum or the following bounce tail.
    func blocksContentFollowing(now: TimeInterval) -> Bool {
        phase == .animating ? false : blocksPositioning(now: now)
    }

    mutating func didSettle() { waitingForQuiet = false }
}

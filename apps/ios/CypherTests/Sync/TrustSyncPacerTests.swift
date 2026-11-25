// TrustSyncPacer: when the phone reads the trust chain, given what the
// registry session reports (T1, docs/design/do-cost.md). The invariant: the
// phone is never more than a minute without either a joined session that
// carries chain heads or a chain read.

import XCTest
@testable import Cypher

final class TrustSyncPacerTests: XCTestCase {
    private let t0 = ContinuousClock.now

    private func at(_ seconds: Int) -> ContinuousClock.Instant { t0 + .seconds(seconds) }

    /// A pacer that just synced at t0 with a chain of length 1.
    private func synced() -> TrustSyncPacer {
        var pacer = TrustSyncPacer(due: t0)
        pacer.synced(at: t0)
        return pacer
    }

    func testWithoutHeadsItReadsEveryMinute() {
        var pacer = synced()
        XCTAssertEqual(pacer.due, at(60))
        pacer.joined(at: at(5))  // an older Worker: joined, never a head
        XCTAssertEqual(pacer.due, at(60))
        pacer.synced(at: at(60))
        XCTAssertEqual(pacer.due, at(120))
    }

    func testAMatchingHeadDefersToTheSafetyPoll() {
        var pacer = synced()
        pacer.joined(at: at(1))
        pacer.head(1, local: 1, at: at(1))
        XCTAssertEqual(pacer.due, at(15 * 60))
        pacer.synced(at: at(15 * 60))
        XCTAssertEqual(pacer.due, at(30 * 60), "safety cadence while heads are carried")
    }

    func testADifferentHeadReadsAtOnce() {
        var pacer = synced()
        pacer.head(1, local: 1, at: at(1))
        pacer.head(2, local: 1, at: at(300))
        XCTAssertEqual(pacer.due, at(300))
    }

    func testADisconnectReadsWithinAMinuteUnlessTheRejoinMatches() {
        var pacer = synced()
        pacer.head(1, local: 1, at: at(1))
        pacer.left(at: at(300))
        XCTAssertEqual(pacer.due, at(360))
        XCTAssertFalse(pacer.carried)

        var rejoined = pacer
        rejoined.joined(at: at(310))
        rejoined.head(1, local: 1, at: at(310))
        XCTAssertEqual(rejoined.due, at(15 * 60), "the rejoin's head matched")

        var oldWorker = pacer
        oldWorker.joined(at: at(310))
        XCTAssertEqual(oldWorker.due, at(360), "a rejoin without a head keeps the minute")
    }

    func testAFailedReadRetriesWithinAMinuteEvenWhileCarried() {
        var pacer = synced()
        pacer.head(2, local: 1, at: at(10))
        pacer.synced(at: at(10), ok: false)
        XCTAssertEqual(pacer.due, at(70))
        // A matching head does not stretch past the last good read's safety.
        pacer.head(2, local: 1, at: at(20))
        XCTAssertEqual(pacer.due, at(20))
    }

    func testAHeadBeforeTheFirstReadDoesNotDelayIt() {
        var pacer = TrustSyncPacer(due: t0)
        pacer.head(0, local: 0, at: at(0))
        XCTAssertEqual(pacer.due, t0)
    }
}

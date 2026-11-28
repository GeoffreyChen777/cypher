// What a chat2 room provably holds, as Loro counter ranges, so the device can
// send the room whatever only it still has.
//
// The doc and the push queue live apart: the doc is saved with every local
// commit in it, the queue only in memory. A commit whose push never got its
// ack before the app died survived in the saved doc and nowhere else, and
// every later commit built on it, so every reader parked them all for good:
// sends sat grey forever. A row a host parks can also leave the room when a
// checkpoint covers its seq (the checkpoint holds only what the host could
// apply). The ledger answers both: after each catch-up, whatever the doc
// holds beyond what the room holds goes up again (`RoomHeal`).
//
// The room holds its checkpoint (the state frame's frontier, the version
// vector the host exported) and every row after the checkpoint's seq. The
// ledger keeps the frontier and, per Loro peer, runs of counters seen in rows
// after it. It may claim less than the room holds (that costs a duplicate
// push, which Loro ignores), never more: a claim the room can't back would
// hide a lost op again.

import Foundation
import Loro

struct RoomLedger: Codable, Equatable {
    /// Counters `[start, end)` of one peer, seen in rows `firstSeq...lastSeq`.
    struct Run: Codable, Equatable {
        var peer: UInt64
        var start: Int32
        var end: Int32
        var firstSeq: UInt64
        var lastSeq: UInt64
        var rows: Int
    }

    /// A run grows over at most this many rows. A checkpoint that falls
    /// inside a run drops the whole run (which part it covers is unknown), so
    /// this bounds what such a checkpoint costs in duplicate pushes.
    static let rowsPerRun = 16

    /// The checkpoint seq the ledger stands on; nil until a hello answered.
    private(set) var basis: UInt64?
    /// That checkpoint's version vector (peer → end counter).
    private(set) var frontier: [UInt64: Int32] = [:]
    /// Counters seen in rows after `basis`.
    private(set) var runs: [Run] = []
    /// Every row in `(basis, through]` has been noted.
    private(set) var through: UInt64 = 0
    /// Noted rows past a gap in `(through, …)`, until the gap fills.
    private(set) var ahead: Set<UInt64> = []

    /// The hello answer (`checkpointSeq` 0 when the room has no checkpoint).
    /// Afterwards rows must be read from `through` on for the ledger to know
    /// the room: the caller asks for `min(its own plan, through)`.
    mutating func rebase(checkpointSeq: UInt64, frontier: [UInt64: Int32], headSeq: UInt64) {
        // No ledger yet, or a room that went back (wiped and reseeded).
        let reset = (basis.map { checkpointSeq < $0 } ?? true) || headSeq < through
        basis = checkpointSeq
        self.frontier = frontier
        if reset {
            runs = []
            ahead = []
            through = checkpointSeq
            return
        }
        // Rows at or before the checkpoint left the room. What the host
        // applied of them is in `frontier`; the rest is gone, so claims
        // resting on them go too, a run the checkpoint cut through included.
        runs.removeAll { $0.firstSeq <= checkpointSeq }
        ahead = ahead.filter { $0 > checkpointSeq }
        through = max(through, checkpointSeq)
        advance()
    }

    /// The room holds the row `seq` with these counter ranges (a backfilled
    /// or live row, or an ack of this device's own push).
    mutating func note(seq: UInt64, spans: [UInt64: CounterSpan]) {
        guard let basis, seq > basis else { return }
        if seq <= through || ahead.contains(seq) { return }
        for (peer, span) in spans.sorted(by: { $0.key < $1.key }) where span.end > span.start {
            add(peer: peer, span: span, seq: seq)
        }
        if seq == through + 1 {
            through = seq
            advance()
        } else {
            ahead.insert(seq)
        }
    }

    /// Per peer, the end of the counter prefix from 0 the room holds.
    func held() -> [UInt64: Int32] {
        var out = frontier
        let byPeer = Dictionary(grouping: runs, by: \.peer)
        for (peer, peerRuns) in byPeer {
            var end = out[peer] ?? 0
            for run in peerRuns.sorted(by: { $0.start < $1.start }) {
                guard run.start <= end else { break }
                end = max(end, run.end)
            }
            if end > 0 { out[peer] = end }
        }
        return out
    }

    /// The ranges a doc holding `doc` (peer → end counter) has that the room
    /// doesn't, leaving out the batches already queued for push.
    func missing(doc: [UInt64: Int32], queued: [[UInt64: CounterSpan]]) -> [IdSpan] {
        var covered = held()
        // Queued batches extend the cover only where they continue it: a
        // queued batch past a gap still leaves the gap to send.
        var extended = true
        while extended {
            extended = false
            for batch in queued {
                for (peer, span) in batch
                where span.start <= (covered[peer] ?? 0) && span.end > (covered[peer] ?? 0) {
                    covered[peer] = span.end
                    extended = true
                }
            }
        }
        return doc.sorted(by: { $0.key < $1.key }).compactMap { peer, end in
            let start = covered[peer] ?? 0
            return end > start ? IdSpan(peer: peer, counter: CounterSpan(start: start, end: end)) : nil
        }
    }

    private mutating func add(peer: UInt64, span: CounterSpan, seq: UInt64) {
        if let ix = runs.lastIndex(where: { $0.peer == peer && $0.end == span.start }),
            runs[ix].rows < Self.rowsPerRun
        {
            runs[ix].end = span.end
            runs[ix].lastSeq = seq
            runs[ix].rows += 1
            return
        }
        // Already known (a duplicate row, or a push the room acked twice).
        if runs.contains(where: { $0.peer == peer && $0.start <= span.start && span.end <= $0.end }) { return }
        runs.append(Run(peer: peer, start: span.start, end: span.end, firstSeq: seq, lastSeq: seq, rows: 1))
    }

    private mutating func advance() {
        while ahead.remove(through + 1) != nil { through += 1 }
    }
}

extension RoomLedger {
    /// Counter ranges an update carries, per peer; nil when the bytes don't
    /// decode (the row then counts for its seq only).
    static func spans(of update: Data) -> [UInt64: CounterSpan]? {
        guard let meta = try? decodeImportBlobMeta(bytes: update, checkChecksum: false) else { return nil }
        let start = meta.partialStartVv.toHashmap()
        var out: [UInt64: CounterSpan] = [:]
        for (peer, end) in meta.partialEndVv.toHashmap() {
            let from = start[peer] ?? 0
            if end > from { out[peer] = CounterSpan(start: from, end: end) }
        }
        return out
    }

    /// A checkpoint frontier (an encoded version vector); empty when absent.
    static func vector(_ frontier: Data) -> [UInt64: Int32] {
        guard !frontier.isEmpty, let vv = try? VersionVector.decode(bytes: frontier) else { return [:] }
        return vv.toHashmap()
    }
}

/// The batches that send a room what `doc` holds and the ledger says the
/// room lacks, each under the push cap, in causal order.
enum RoomHeal {
    static func batches(
        doc: LoroDoc, ledger: RoomLedger, queued: [Data], maxBytes: Int = ChatRoomClient.maxPushBytes
    ) -> [Data] {
        let spans = ledger.missing(
            doc: doc.oplogVv().toHashmap(), queued: queued.compactMap { RoomLedger.spans(of: $0) })
        guard !spans.isEmpty else { return [] }
        return export(spans, from: doc, maxBytes: maxBytes)
    }

    /// One export when it fits; otherwise halves (spans, then a span's
    /// counters) until each part does. A single op over the cap can't be
    /// sent at all: it is logged and left out.
    private static func export(_ spans: [IdSpan], from doc: LoroDoc, maxBytes: Int) -> [Data] {
        guard let bytes = try? doc.export(mode: .updatesInRange(spans: spans)), !bytes.isEmpty else { return [] }
        if bytes.count <= maxBytes { return [bytes] }
        if spans.count > 1 {
            let half = spans.count / 2
            return export(Array(spans[..<half]), from: doc, maxBytes: maxBytes)
                + export(Array(spans[half...]), from: doc, maxBytes: maxBytes)
        }
        let span = spans[0]
        let length = span.counter.end - span.counter.start
        guard length > 1 else {
            roomLog.error("chat2: one op of \(bytes.count)B exceeds the row cap; it can't be re-sent")
            return []
        }
        let mid = span.counter.start + length / 2
        return export(
            [IdSpan(peer: span.peer, counter: CounterSpan(start: span.counter.start, end: mid))],
            from: doc, maxBytes: maxBytes)
            + export(
                [IdSpan(peer: span.peer, counter: CounterSpan(start: mid, end: span.counter.end))],
                from: doc, maxBytes: maxBytes)
    }
}

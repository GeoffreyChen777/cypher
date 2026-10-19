// Transcript row-build cache and the per-row veil registry, both owned by the
// SessionStore so parses and veils outlive view instances.

import SwiftUI

/// Row-build cache: one incremental parser per streaming part plus a memo of
/// settled parses. Owned by the SessionStore (NOT view @State) so the parses
/// survive across view instances — re-opening a chat re-parses nothing.
@MainActor
final class TranscriptBuilderCache {
    private var parsers: [String: IncrementalMarkdownParser] = [:]
    private var completed: [String: CompletedParse] = [:]
    private var cachedRevision: UInt64?
    private var builtRows: [TranscriptRow] = []
    /// The toggle pins `cachedRows` was folded for; nil forces a refold.
    private var foldedFor: [String: Bool]?
    private var cachedRows: [TranscriptRow] = []
    private var prewarming = false
    /// The conversation's rounds, for the turn scrubber — rebuilt with the
    /// rows, so read them after `rows(...)`.
    private(set) var rounds: [TranscriptRound] = []
    /// Round index by the id of the row that starts it.
    private(set) var roundIndex: [String: Int] = [:]

    /// Rows for the store's current `revision`, with the closed toggles
    /// (`togglePins` over each toggle's default) folded away. Rows only
    /// change when the doc or the pins do — gate on both and hand back the
    /// same array.
    func rows(
        revision: UInt64,
        entries: [MessageEntry],
        pendingSends: [PendingSend],
        togglePins: [String: Bool]
    ) -> [TranscriptRow] {
        if cachedRevision != revision {
            builtRows = TranscriptRowBuilder.rows(
                entries: entries, pendingSends: pendingSends,
                parsers: &parsers, completed: &completed)
            cachedRevision = revision
            foldedFor = nil
        }
        if foldedFor == togglePins { return cachedRows }
        cachedRows = TranscriptRowBuilder.foldClosedToggles(builtRows, pins: togglePins)
        foldedFor = togglePins
        let rounds = TranscriptRound.rounds(in: cachedRows)
        if rounds != self.rounds {
            self.rounds = rounds
            roundIndex = Dictionary(uniqueKeysWithValues: rounds.enumerated().map { ($1.rowId, $0) })
        }
        return cachedRows
    }

    /// The settled parses so far: a projection's off-main parse skips them.
    var settledParses: [String: CompletedParse] { completed }

    /// Parses the settled parts of `entries` that `memo` lacks, for `adopt`.
    /// The doc projection runs this in its background task, so the rows of a
    /// page it publishes assemble from memo hits, never parsing on the main
    /// thread.
    nonisolated static func parseSettled(
        _ entries: [MessageEntry], memo: [String: CompletedParse]
    ) -> [String: CompletedParse] {
        var parsed: [String: CompletedParse] = [:]
        for job in settledJobs(entries, memo: memo) {
            parsed[job.key] = CompletedParse(source: job.text, blocks: MarkdownParser.parse(job.text))
        }
        return parsed
    }

    /// Takes in parses made off the main thread.
    func adopt(_ parsed: [String: CompletedParse]) {
        for (key, parse) in parsed where completed[key]?.source != parse.source {
            completed[key] = parse
        }
    }

    /// The settled text parts of `entries` without a parse in `memo`. A
    /// streaming tail is the incremental parser's.
    nonisolated private static func settledJobs(
        _ entries: [MessageEntry], memo: [String: CompletedParse]
    ) -> [(key: String, text: String)] {
        var jobs: [(key: String, text: String)] = []
        for entry in entries where entry.role != .user {
            let streaming = entry.status == .streaming
            let lastIx = entry.parts.indices.last
            for (ix, part) in entry.parts.enumerated() {
                let partId: String
                let text: String
                switch part {
                case .text(let id, let body, _), .reasoning(let id, let body):
                    (partId, text) = (id, body)
                default:
                    continue
                }
                guard !text.isEmpty else { continue }
                if streaming && ix == lastIx { continue }
                let key = "\(entry.id)#\(partId)"
                if memo[key]?.source != text {
                    jobs.append((key, text))
                }
            }
        }
        return jobs
    }

    /// Parse every settled part OFF the main thread and merge into the memo,
    /// so the first `rows()` of fed entries (`SessionStore.setEntries`)
    /// assembles from memo hits instead of parsing inside body.
    func prewarm(entries: [MessageEntry]) {
        guard !prewarming else { return }
        let jobs = Self.settledJobs(entries, memo: completed)
        guard !jobs.isEmpty else { return }
        prewarming = true
        Task { @MainActor [weak self] in
            let parsed = await Task.detached(priority: .userInitiated) { [jobs] in
                jobs.map { (key: $0.key, text: $0.text, blocks: MarkdownParser.parse($0.text)) }
            }.value
            guard let self else { return }
            self.prewarming = false
            for job in parsed where self.completed[job.key]?.source != job.text {
                self.completed[job.key] = CompletedParse(source: job.text, blocks: job.blocks)
            }
        }
    }
}

/// Veil registry — one RowVeil per live row, dropped on the live→complete flip.
@Observable
final class VeilStore {
    @ObservationIgnored private var veils: [String: RowVeil] = [:]

    func veil(for rowId: String, seeded: Bool) -> RowVeil {
        if let existing = veils[rowId] { return existing }
        let veil = RowVeil()
        veils[rowId] = veil
        return veil
    }

    func drop(_ rowId: String) {
        veils.removeValue(forKey: rowId)
    }
}

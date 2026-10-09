// Transcript row model — a port of crates/ui/src/shell/transcript.rs
// rows_for_entry. One row = one markdown top-level block / tool group / chip,
// never one message: streamed tokens re-render one row, and SwiftUI's lazy
// stack only re-measures what changed.
//
// Stable ids: markdown rows are "{entryId}#{partId}.{blockIx}", tool groups
// "{entryId}#g{groupIx}", chips "{entryId}#{partId}". Live and completed parts
// split identically, so the live→complete handoff never changes row identity.

import Foundation

enum RowKind {
    case user(text: String, isSteer: Bool = false)
    /// Consecutive prose blocks of one part (paragraphs, headings, plain
    /// lists) as one selectable text, so a selection can cross them.
    case prose(blocks: [MDBlock], streaming: Bool)
    /// A block with a view of its own: a code block, table, quote or rule.
    case markdown(block: MDBlock, streaming: Bool)
    case toolGroup(tools: [ToolItem], autoOpen: Bool)
    /// The toggle over a run of work that mixed tool calls with thinking —
    /// one row instead of alternating "Thought" and "Ran N commands" rows
    /// (transcript.rs `Activity`). The `rows` that follow are the run in
    /// order (its tool groups, its thoughts and their blocks, all `nested`);
    /// they are dropped while it is closed. Open by default only while the
    /// run is the streaming tail, like a tool group.
    case activity(rows: Int, summary: String, autoOpen: Bool)
    case inputChip(header: String, resolved: Bool)
    case errorChip(message: String)
    /// The toggle over an append-mode translation's original answer. The
    /// `rows` that follow it — the original's blocks and the separator rule —
    /// are dropped while it is closed, the default (transcript.rs
    /// `TranslationOriginal`; see `foldClosedToggles`).
    case translationOriginal(rows: Int)
    /// The toggle over a reasoning part: "Thinking…" while it streams,
    /// "Thought" once settled. The `rows` that follow — the thought's blocks,
    /// `muted` — are dropped while it is closed, the default (transcript.rs
    /// `Thought`). In a work run it is a chip labelled with `preview`, the
    /// thought's first line.
    case thought(rows: Int, live: Bool, preview: String)
}

/// What an append-mode translation puts between the agent's answer and its
/// translation (quote_origin.rs `APPEND_SEPARATOR`).
let translationAppendSeparator = "\n\n---\n\n"

struct ToolItem: Hashable {
    /// The tool part's id — Pi's call id, which carries the nesting.
    var id: String
    var call: RenderToolCall
    var isError: Bool
    var resolved: Bool
    /// Nesting under the call that made this one: 0 for a call the model
    /// made, 1 for a call a Pi codemode script made from inside its run
    /// (deeper if that call made calls of its own). See `nestToolCalls`.
    var depth = 0

    var status: ToolStatus {
        if isError { return .failed }
        return resolved ? .completed : .running
    }
}

/// transcript.rs `ToolStatus`: where a tool call is in its lifecycle, shown
/// as the chip's status icon. A failed call reads as failed even if it never
/// resolved.
enum ToolStatus: Equatable {
    case running, completed, failed
}

struct TranscriptRow: Identifiable {
    var id: String
    /// Content fingerprint — SwiftUI diff key; a changed version re-renders
    /// exactly one row.
    var version: UInt64
    var turnStart: Bool
    var kind: RowKind
    var entryId: String
    var timestamp: Int64?
    /// "{entryId}#{partId}" for markdown rows, nil otherwise — two adjacent
    /// rows sharing it are blocks of the same part (the tighter gap).
    var partKey: String?
    /// Leading gap, resolved at build time. It depends on the PREVIOUS row, so
    /// deriving it in the view body forced an `enumerated()` copy of the whole
    /// row array on every frame; now the body just reads it.
    var topGap: CGFloat = 0
    /// The owning entry's role — what a selection's actions may do with it
    /// (fork before a prompt / after a reply; never a system row).
    var role: MessageRole = .assistant
    /// A block of the model's thinking: painted in the muted tone.
    var muted = false
    /// Part of a work run (`activity`): a tool group shows only its chips, a
    /// thought is a chip, and a thought's blocks sit indented under it.
    var nested = false
}

/// A settled part's parse, keyed by content so a completed block is parsed
/// once rather than on every rebuild.
struct CompletedParse {
    var source: String
    var blocks: [TopBlock]
}

enum TranscriptRowBuilder {
    /// Split entries into rows. `parsers` caches one incremental parser per
    /// "{entryId}#{partId}" so the streaming tail re-parses O(delta + tail);
    /// `completed` memoizes settled parts so they parse exactly once.
    static func rows(entries: [MessageEntry],
                     pendingSends: [PendingSend],
                     parsers: inout [String: IncrementalMarkdownParser],
                     completed: inout [String: CompletedParse]) -> [TranscriptRow] {
        var rows: [TranscriptRow] = []
        var live = Set<String>()
        for entry in entries {
            let first = rows.count
            rowsForEntry(entry, into: &rows, parsers: &parsers,
                         completed: &completed, live: &live)
            for ix in first..<rows.count { rows[ix].role = entry.role }
        }
        // Optimistic echo: pending sends share their client-minted id, so the
        // host's real entry replaces them without a flicker.
        let ids = Set(entries.map(\.id))
        for pending in pendingSends where !ids.contains(pending.messageId) {
            rows.append(TranscriptRow(id: pending.messageId,
                                      version: userVersion(pending.text, isSteer: pending.isSteer) | 1,
                                      turnStart: true,
                                      kind: .user(text: pending.text, isSteer: pending.isSteer),
                                      entryId: pending.messageId,
                                      timestamp: nil,
                                      partKey: nil,
                                      role: .user))
        }
        // Drop memos for parts that no longer exist. The count guard keeps the
        // common (append-only) rebuild from copying the dict every token.
        if completed.count > live.count {
            completed = completed.filter { live.contains($0.key) }
        }
        for ix in rows.indices {
            rows[ix].topGap = gap(for: rows[ix],
                                  previous: ix > 0 ? rows[ix - 1] : nil,
                                  isFirst: ix == 0)
        }
        return rows
    }

    private static func gap(for row: TranscriptRow,
                            previous: TranscriptRow?,
                            isFirst: Bool) -> CGFloat {
        if isFirst { return TranscriptView.gapTurn + 10 }
        // Separate exchanges without pulling a user's prompt away from its
        // reply. Pending sends follow this same path as confirmed messages.
        if case .user(_, let isSteer) = row.kind {
            return isSteer ? TranscriptView.gapTurn : TranscriptView.gapExchange
        }
        if row.turnStart { return TranscriptView.gapTurn }
        if row.nested { return nestedGap(for: row, previous: previous) }
        // Same part ⇒ these are sibling markdown blocks, not a new turn.
        if let key = row.partKey, key == previous?.partKey { return MD.blockGap }
        return TranscriptView.gapBlock
    }

    /// transcript.rs `nested_gap`: chips in a work run stack like a tool
    /// group's (their cards carry their own margins), a thought's text sits
    /// just under its chip, and the chip after it gets some air.
    private static func nestedGap(for row: TranscriptRow, previous: TranscriptRow?) -> CGFloat {
        guard let previous else { return 0 }
        if case .activity = previous.kind { return 2 }
        let isBlock = { (r: TranscriptRow) in r.partKey != nil }
        if isBlock(row) {
            return row.partKey == previous.partKey ? MD.blockGap : 2
        }
        return isBlock(previous) ? 6 : 0
    }

    private static func rowsForEntry(_ entry: MessageEntry,
                                     into rows: inout [TranscriptRow],
                                     parsers: inout [String: IncrementalMarkdownParser],
                                     completed: inout [String: CompletedParse],
                                     live: inout Set<String>) {
        let streaming = entry.status == .streaming
        let settled = entry.status != nil && !streaming

        if entry.role == .user {
            // One bubble row per user message.
            let text = entry.parts.compactMap { part -> String? in
                if case .text(_, let t, _) = part { return t }
                return nil
            }.joined(separator: "\n")
            guard !text.isEmpty else { return }
            rows.append(TranscriptRow(id: entry.id, version: userVersion(text, isSteer: entry.isSteer),
                                      turnStart: true, kind: .user(text: text, isSteer: entry.isSteer),
                                      entryId: entry.id, timestamp: entry.createdAt,
                                      partKey: nil))
            return
        }

        var first = true
        var pendingTools: [ToolItem] = []
        var groupIx = 0
        let lastPartIx = entry.parts.indices.last
        // The open work run: where its rows start and the part that opened
        // it (the activity row's id). Tool calls and thoughts extend it;
        // anything the reader sees between them closes it.
        var run: (start: Int, firstPart: String)?

        func openRun(_ partId: String) {
            if run == nil { run = (rows.count, partId) }
        }

        /// transcript.rs `fold_work_run`: a run that mixed tool calls with
        /// thinking folds behind one activity row. A run of only tools is one
        /// group already and a lone thought its own toggle.
        func closeRun(autoOpen: Bool) {
            guard let open = run else { return }
            run = nil
            let start = open.start
            var thoughts = 0
            var tools: [ToolItem] = []
            for row in rows[start...] {
                switch row.kind {
                case .thought: thoughts += 1
                case .toolGroup(let group, _): tools += group
                default: break
                }
            }
            guard thoughts > 0, !tools.isEmpty else { return }
            for ix in start..<rows.count {
                rows[ix].nested = true
                // A thought that gains its first tool call redraws as a chip.
                rows[ix].version ^= 1 << 61
            }
            let summary = toolGroupSummary(tools, thoughts: thoughts)
            let count = rows.count - start
            let activity = TranscriptRow(id: "\(entry.id)#\(open.firstPart).activity",
                                         version: (fnv1a(summary) ^ UInt64(count)) << 1 | (autoOpen ? 1 : 0),
                                         turnStart: rows[start].turnStart,
                                         kind: .activity(rows: count, summary: summary, autoOpen: autoOpen),
                                         entryId: entry.id, timestamp: nil, partKey: nil)
            rows[start].turnStart = false
            rows.insert(activity, at: start)
        }

        func flushTools(lastIx: Int?) {
            guard !pendingTools.isEmpty else { return }
            let autoOpen = streaming && lastIx == lastPartIx
            let id = "\(entry.id)#g\(groupIx)"
            let tools = nestToolCalls(pendingTools)
            var version = toolFingerprint(tools)
            if autoOpen { version ^= 1 }
            rows.append(TranscriptRow(id: id, version: version, turnStart: first,
                                      kind: .toolGroup(tools: tools, autoOpen: autoOpen),
                                      entryId: entry.id, timestamp: nil, partKey: nil))
            first = false
            pendingTools = []
            groupIx += 1
        }

        /// One row per prose run / other block of a part, named by its first
        /// block so a run keeps its id as blocks join it.
        func appendBlockRows(key: String, blocks: [TopBlock], runs: [Range<Int>], partIx: Int,
                             liveTail: Bool, muted: Bool) {
            for run in runs {
                let lastOfPart = run.upperBound == blocks.count
                let live = liveTail && lastOfPart
                let stamped = settled && partIx == lastPartIx && lastOfPart
                let kind: RowKind
                var version: UInt64
                if TranscriptTextStyle.isProse(blocks[run.lowerBound].block) {
                    var hash: UInt64 = 0xcbf29ce484222325
                    for top in blocks[run] { hash = (hash ^ top.fingerprint) &* 0x100000001b3 }
                    version = (hash << 1) | (live ? 1 : 0)
                    kind = .prose(blocks: blocks[run].map(\.block), streaming: live)
                } else {
                    version = (blocks[run.lowerBound].fingerprint << 1) | (live ? 1 : 0)
                    kind = .markdown(block: blocks[run.lowerBound].block, streaming: live)
                }
                if stamped {
                    version ^= 1 << 62  // timestamp attach keeps the diff key honest
                }
                rows.append(TranscriptRow(
                    id: "\(key).\(run.lowerBound)", version: version, turnStart: first,
                    kind: kind,
                    entryId: entry.id,
                    timestamp: stamped ? entry.createdAt : nil,
                    partKey: key,
                    muted: muted))
                first = false
            }
        }

        for (ix, part) in entry.parts.enumerated() {
            switch part {
            case .tool(let partId, let call, let isError, let resolved):
                // Nothing is appended until the group flushes, so the run's
                // rows start here.
                openRun(partId)
                pendingTools.append(ToolItem(id: partId, call: call, isError: isError, resolved: resolved))
                if ix == lastPartIx { flushTools(lastIx: ix) }

            case .text(let partId, let text, let agentText):
                // An empty text part renders nothing, so it splits nothing.
                guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { continue }
                flushTools(lastIx: ix - 1)
                closeRun(autoOpen: false)
                let key = "\(entry.id)#\(partId)"
                live.insert(key)
                let isLiveTail = streaming && ix == lastPartIx
                let blocks = parse(text: text, key: key, streaming: isLiveTail,
                                   parsers: &parsers, completed: &completed)
                let runs = proseRuns(blocks, liveTail: isLiveTail)
                // Block rows keep their ids either way, so the toggle only
                // ever hides or shows rows. The rule closing the original is
                // never prose, so no run straddles the fold.
                if let agentText,
                   let folded = appendedOriginalBlocks(text: text, agent: agentText, blocks: blocks) {
                    let hidden = runs.filter { $0.lowerBound < folded }.count
                    rows.append(TranscriptRow(id: "\(key).original", version: UInt64(hidden) << 1,
                                              turnStart: first,
                                              kind: .translationOriginal(rows: hidden),
                                              entryId: entry.id, timestamp: nil, partKey: nil))
                    first = false
                }
                appendBlockRows(key: key, blocks: blocks, runs: runs, partIx: ix,
                                liveTail: isLiveTail, muted: false)

            case .reasoning(let partId, let text):
                guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { continue }
                flushTools(lastIx: ix - 1)
                openRun(partId)
                let key = "\(entry.id)#\(partId)"
                live.insert(key)
                // Still thinking: the reasoning is the live tail.
                let isLiveTail = streaming && ix == lastPartIx
                let blocks = parse(text: text, key: key, streaming: isLiveTail,
                                   parsers: &parsers, completed: &completed)
                let runs = proseRuns(blocks, liveTail: isLiveTail)
                let preview = thoughtPreview(text)
                rows.append(TranscriptRow(id: "\(key).thought",
                                          version: (UInt64(runs.count) << 1 | (isLiveTail ? 1 : 0))
                                              ^ fnv1a(preview) << 8,
                                          turnStart: first,
                                          kind: .thought(rows: runs.count, live: isLiveTail, preview: preview),
                                          entryId: entry.id, timestamp: nil, partKey: nil))
                first = false
                appendBlockRows(key: key, blocks: blocks, runs: runs, partIx: ix,
                                liveTail: isLiveTail, muted: true)

            case .input(let partId, _, let questions, let resolved):
                flushTools(lastIx: ix - 1)
                closeRun(autoOpen: false)
                let header = questions.first.map { QuestionPresentation($0).header } ?? "Question"
                rows.append(TranscriptRow(id: "\(entry.id)#\(partId)",
                                          version: (fnv1a(header) << 1) | (resolved ? 1 : 0),
                                          turnStart: first,
                                          kind: .inputChip(header: header, resolved: resolved),
                                          entryId: entry.id, timestamp: nil, partKey: nil))
                first = false

            case .error(let partId, let message):
                flushTools(lastIx: ix - 1)
                closeRun(autoOpen: false)
                rows.append(TranscriptRow(id: "\(entry.id)#\(partId)", version: fnv1a(message),
                                          turnStart: first,
                                          kind: .errorChip(message: message),
                                          entryId: entry.id, timestamp: nil, partKey: nil))
                first = false
            }
        }
        flushTools(lastIx: lastPartIx)
        // Still the tail of a streaming reply: open, like a live tool group.
        closeRun(autoOpen: streaming)
    }

    /// transcript.rs `thought_preview`: a thought's label in a work run — its
    /// first line, without the heading or emphasis markers models often
    /// title a thought with ("**Planning**").
    static func thoughtPreview(_ text: String) -> String {
        let line = text.split(whereSeparator: \.isNewline)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty } ?? ""
        var title = Substring(line.drop { $0 == "#" }.drop { $0 == " " })
        for marker in ["**", "__", "*", "_"]
        where title.count > marker.count * 2 && title.hasPrefix(marker) && title.hasSuffix(marker) {
            title = title.dropFirst(marker.count).dropLast(marker.count)
            break
        }
        return title.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// transcript.rs `appended_original_blocks`: how many top-level blocks of
    /// an append-mode translation belong to the folded original — its own
    /// blocks plus the separator rule. nil unless `text` is `agent`, the
    /// separator and a translation that has begun; until then (and whenever
    /// the rendering doesn't parse as expected, e.g. an original ending
    /// inside an open code fence) the text shows whole.
    static func appendedOriginalBlocks(text: String, agent: String, blocks: [TopBlock]) -> Int? {
        let rest = text.utf8.dropFirst(agent.utf8.count)
        guard text.utf8.starts(with: agent.utf8),
              rest.starts(with: translationAppendSeparator.utf8) else { return nil }
        // Blocks carry source lines, not offsets: the original ends on line
        // `agentLines`, so the first block past it must be the rule.
        let agentLines = agent.utf8.reduce(1) { $1 == UInt8(ascii: "\n") ? $0 + 1 : $0 }
        guard let rule = blocks.firstIndex(where: { $0.startLine > agentLines }),
              rule > 0, rule + 1 < blocks.count,
              case .rule = blocks[rule].block else { return nil }
        return rule + 1
    }

    /// transcript.rs `toggle_span`: the rows a toggle covers and whether it
    /// starts open, if `kind` is one. A translation's original and a thought
    /// start closed, a work run only while it streams.
    static func toggleSpan(_ kind: RowKind) -> (rows: Int, openByDefault: Bool)? {
        switch kind {
        case .translationOriginal(let rows), .thought(let rows, _, _): return (rows, false)
        case .activity(let rows, _, let autoOpen): return (rows, autoOpen)
        default: return nil
        }
    }

    /// Whether the toggle `row` is open: the reader's tap (`pins`, by row
    /// id) wins, else the toggle's default.
    static func isOpen(_ row: TranscriptRow, pins: [String: Bool]) -> Bool {
        guard let span = toggleSpan(row.kind) else { return false }
        return pins[row.id] ?? span.openByDefault
    }

    /// transcript.rs `fold_closed_toggles`: drop the rows each CLOSED toggle
    /// covers. `pins` holds the toggles the reader tapped; every other
    /// toggle keeps its default. The row after a fold re-takes its gap from
    /// the toggle it now follows, and a folded row's timestamp (a reply that
    /// ended while thinking) moves onto its toggle.
    static func foldClosedToggles(_ rows: [TranscriptRow], pins: [String: Bool]) -> [TranscriptRow] {
        guard rows.contains(where: { toggleSpan($0.kind) != nil }) else { return rows }
        var folded: [TranscriptRow] = []
        folded.reserveCapacity(rows.count)
        var hide = 0
        var regap = false
        for var row in rows {
            if hide > 0 {
                hide -= 1
                if let stamp = row.timestamp, let toggle = folded.indices.last {
                    folded[toggle].timestamp = stamp
                    folded[toggle].version ^= 1 << 62
                }
                continue
            }
            if regap {
                row.topGap = gap(for: row, previous: folded.last, isFirst: folded.isEmpty)
                regap = false
            }
            folded.append(row)
            if let span = toggleSpan(row.kind), !isOpen(row, pins: pins) {
                hide = span.rows
                regap = span.rows > 0
            }
        }
        return folded
    }

    /// A part's blocks in rows: each run of consecutive prose blocks
    /// (`TranscriptTextStyle.isProse`) together, every other block alone.
    /// While the part streams, its last block keeps a row of its own: only
    /// that block re-lays out per frame as text arrives, and it joins the
    /// run above it once the reply settles.
    static func proseRuns(_ blocks: [TopBlock], liveTail: Bool) -> [Range<Int>] {
        var runs: [Range<Int>] = []
        let mergeable = liveTail ? blocks.count - 1 : blocks.count
        var start = 0
        while start < blocks.count {
            var end = start + 1
            if start < mergeable, TranscriptTextStyle.isProse(blocks[start].block) {
                while end < mergeable, TranscriptTextStyle.isProse(blocks[end].block) { end += 1 }
            }
            runs.append(start..<end)
            start = end
        }
        return runs
    }

    private static func userVersion(_ text: String, isSteer: Bool) -> UInt64 {
        fnv1a(text) ^ (isSteer ? UInt64(1) << 63 : 0)
    }

    private static func parse(text: String, key: String, streaming: Bool,
                              parsers: inout [String: IncrementalMarkdownParser],
                              completed: inout [String: CompletedParse]) -> [TopBlock] {
        if streaming {
            let parser = parsers[key] ?? IncrementalMarkdownParser()
            parser.setText(text)
            parsers[key] = parser
            return parser.blocks
        }
        // Completed. Drop the live parser either way, then serve from the memo:
        // rows are rebuilt on every doc update, and re-parsing every settled
        // part each time made a rebuild O(whole transcript) — the dominant cost
        // of opening a long cached session, and paid again per streamed token.
        let handoff = parsers.removeValue(forKey: key)
        if let hit = completed[key], hit.source == text {
            return hit.blocks
        }
        // Adopt the live parser's tree on the live→complete flip, else parse.
        let blocks = handoff?.source == text
            ? (handoff?.blocks ?? MarkdownParser.parse(text))
            : MarkdownParser.parse(text)
        completed[key] = CompletedParse(source: text, blocks: blocks)
        return blocks
    }

    private static func toolFingerprint(_ tools: [ToolItem]) -> UInt64 {
        var hash: UInt64 = 0xcbf29ce484222325
        for tool in tools {
            for byte in tool.call.tag.utf8 {
                hash ^= UInt64(byte)
                hash = hash &* 0x100000001b3
            }
            hash ^= UInt64(tool.call.fields.count) &+ (tool.isError ? 2 : 0) &+ (tool.resolved ? 4 : 0)
            hash = hash &* 0x100000001b3
            // A call nesting under its caller once the caller's part arrives
            // re-indents the chip.
            hash ^= UInt64(tool.depth)
            hash = hash &* 0x100000001b3
            for (k, v) in tool.call.fields.sorted(by: { $0.key < $1.key }) {
                for byte in "\(k)=\(v)".utf8 {
                    hash ^= UInt64(byte)
                    hash = hash &* 0x100000001b3
                }
            }
        }
        return hash << 3
    }

    /// transcript.rs `nest_tool_calls`: order a tool group so each call made
    /// from inside another call's run follows that call, one level deeper.
    /// Pi runs the calls a codemode script makes (`await tools.read(…)`) as
    /// tool calls of their own, with the id `{caller id}/{n}`; the doc keeps
    /// those ids, so the nesting needs no field of its own.
    ///
    /// A call whose caller is not in the group (a different group, or an id
    /// that merely contains `/`) stays where it is at depth 0. Siblings keep
    /// their arrival order.
    static func nestToolCalls(_ items: [ToolItem]) -> [ToolItem] {
        var index: [String: Int] = [:]
        for (ix, item) in items.enumerated() {
            index[item.id] = ix
        }
        var children = Array(repeating: [Int](), count: items.count)
        var roots: [Int] = []
        for (ix, item) in items.enumerated() {
            // A caller's id is a strict prefix of its calls' ids, so this
            // never cycles.
            if let slash = item.id.lastIndex(of: "/"),
               let caller = index[String(item.id[..<slash])] {
                children[caller].append(ix)
            } else {
                roots.append(ix)
            }
        }
        guard roots.count < items.count else { return items }
        var nested: [ToolItem] = []
        nested.reserveCapacity(items.count)
        var stack: [(ix: Int, depth: Int)] = roots.reversed().map { ($0, 0) }
        while let (ix, depth) = stack.popLast() {
            var item = items[ix]
            item.depth = depth
            nested.append(item)
            for child in children[ix].reversed() {
                stack.append((child, depth + 1))
            }
        }
        return nested
    }

    static func fnv1a(_ text: String) -> UInt64 {
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in text.utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 0x100000001b3
        }
        return hash << 1
    }
}

/// "Ran 3 commands · edited 2 files · 1 failed" (transcript.rs
/// tool_group_summary). A work run counts its thoughts before any failures
/// (view.rs `work_summary`).
func toolGroupSummary(_ tools: [ToolItem], thoughts: Int = 0) -> String {
    var segments: [String] = []
    let runs = tools.filter { $0.call.tag == "exec" }.count
    let scripts = tools.filter(\.call.isScript).count
    // Commands and scripts share one verb: "ran 2 commands and 1 script".
    var ran: [String] = []
    if runs > 0 { ran.append(runs == 1 ? "1 command" : "\(runs) commands") }
    if scripts > 0 { ran.append(scripts == 1 ? "1 script" : "\(scripts) scripts") }
    if !ran.isEmpty { segments.append("ran " + ran.joined(separator: " and ")) }
    let edits = tools.filter { ["editFile", "writeFile", "applyPatch"].contains($0.call.tag) }.count
    if edits > 0 { segments.append(edits == 1 ? "edited 1 file" : "edited \(edits) files") }
    let reads = tools.filter { $0.call.tag == "readFile" }.count
    if reads > 0 { segments.append(reads == 1 ? "read 1 file" : "read \(reads) files") }
    let searches = tools.filter {
        ["search", "glob", "webSearch", "webFetch"].contains($0.call.tag) || $0.call.isToolSearch
    }.count
    if searches > 0 { segments.append(searches == 1 ? "1 search" : "\(searches) searches") }
    let other = tools.count - runs - scripts - edits - reads - searches
    if other > 0 { segments.append(other == 1 ? "1 tool" : "\(other) tools") }
    if thoughts > 0 { segments.append(thoughts == 1 ? "1 thought" : "\(thoughts) thoughts") }
    let failed = tools.filter(\.isError).count
    if failed > 0 { segments.append("\(failed) failed") }
    guard var summary = segments.first else { return "\(tools.count) tools" }
    summary = summary.prefix(1).uppercased() + summary.dropFirst()
    return ([summary] + segments.dropFirst()).joined(separator: " · ")
}

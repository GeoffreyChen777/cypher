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
    case inputChip(header: String, resolved: Bool)
    case errorChip(message: String)
    /// The toggle over an append-mode translation's original answer. The
    /// `rows` that follow it — the original's blocks and the separator rule —
    /// are dropped while it is closed, the default (transcript.rs
    /// `TranslationOriginal`; see `foldTranslationOriginals`).
    case translationOriginal(rows: Int)
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
        // Same part ⇒ these are sibling markdown blocks, not a new turn.
        if let key = row.partKey, key == previous?.partKey { return MD.blockGap }
        return TranscriptView.gapBlock
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

        for (ix, part) in entry.parts.enumerated() {
            switch part {
            case .tool(let partId, let call, let isError, let resolved):
                pendingTools.append(ToolItem(id: partId, call: call, isError: isError, resolved: resolved))
                if ix == lastPartIx { flushTools(lastIx: ix) }

            case .text(let partId, let text, let agentText):
                flushTools(lastIx: ix - 1)
                guard !text.isEmpty else { continue }
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
                for run in runs {
                    let lastOfPart = run.upperBound == blocks.count
                    let live = isLiveTail && lastOfPart
                    let stamped = settled && ix == lastPartIx && lastOfPart
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
                    // Named by its first block, so a run keeps its id as blocks
                    // join it.
                    rows.append(TranscriptRow(
                        id: "\(key).\(run.lowerBound)", version: version, turnStart: first,
                        kind: kind,
                        entryId: entry.id,
                        timestamp: stamped ? entry.createdAt : nil,
                        partKey: key))
                    first = false
                }

            case .input(let partId, _, let questions, let resolved):
                flushTools(lastIx: ix - 1)
                let header = questions.first.map { QuestionPresentation($0).header } ?? "Question"
                rows.append(TranscriptRow(id: "\(entry.id)#\(partId)",
                                          version: (fnv1a(header) << 1) | (resolved ? 1 : 0),
                                          turnStart: first,
                                          kind: .inputChip(header: header, resolved: resolved),
                                          entryId: entry.id, timestamp: nil, partKey: nil))
                first = false

            case .error(let partId, let message):
                flushTools(lastIx: ix - 1)
                rows.append(TranscriptRow(id: "\(entry.id)#\(partId)", version: fnv1a(message),
                                          turnStart: first,
                                          kind: .errorChip(message: message),
                                          entryId: entry.id, timestamp: nil, partKey: nil))
                first = false
            }
        }
        flushTools(lastIx: lastPartIx)
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

    /// transcript.rs `fold_translation_originals`: drop the rows each CLOSED
    /// toggle covers. `open` holds the ids of toggles the user expanded;
    /// every other toggle stays collapsed. The row after a fold re-takes its
    /// gap from the toggle it now follows.
    static func foldTranslationOriginals(_ rows: [TranscriptRow], open: Set<String>) -> [TranscriptRow] {
        let hasToggle = rows.contains {
            if case .translationOriginal = $0.kind { return true }
            return false
        }
        guard hasToggle else { return rows }
        var folded: [TranscriptRow] = []
        folded.reserveCapacity(rows.count)
        var hide = 0
        var regap = false
        for var row in rows {
            if hide > 0 {
                hide -= 1
                continue
            }
            if regap {
                row.topGap = gap(for: row, previous: folded.last, isFirst: folded.isEmpty)
                regap = false
            }
            folded.append(row)
            if case .translationOriginal(let hidden) = row.kind, !open.contains(row.id) {
                hide = hidden
                regap = hidden > 0
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

// MARK: - Tool chip content (transcript.rs tool_chip_content_raw)

extension RenderToolCall {
    /// Pi's `codemode` tool: instead of calling tools one at a time, the
    /// model writes a JavaScript script that calls them (`await
    /// tools.read({…})`), and Pi reports each call the script makes as a tool
    /// call of its own (view.rs CODEMODE_TOOL).
    static let codemodeTool = "codemode"
    /// Pi's `tool_search`: finds and loads tools the model wasn't shown up
    /// front (view.rs TOOL_SEARCH_TOOL).
    static let toolSearchTool = "tool_search"

    /// The input field the doc keeps for an extension tool, if any: a
    /// codemode call's script and a tool_search's query.
    static func keptInputField(_ name: String) -> String? {
        switch name {
        case codemodeTool: return "code"
        case toolSearchTool: return "query"
        default: return nil
        }
    }

    var isScript: Bool { tag == "unknown" && string("name") == Self.codemodeTool }
    var isToolSearch: Bool { tag == "unknown" && string("name") == Self.toolSearchTool }

    /// The script of a codemode call, when the transcript kept it.
    var script: String? { isScript ? string("code") : nil }

    var chipLabel: String {
        if isScript { return "Script" }
        if isToolSearch { return "Find tools" }
        switch tag {
        case "exec": return "Run"
        case "readFile": return "Read"
        case "writeFile": return "Write"
        case "editFile": return "Edit"
        case "applyPatch": return "Patch"
        case "search": return "Search"
        case "glob": return "Glob"
        case "webFetch": return "Fetch"
        case "webSearch": return "Web"
        case "todo": return "Todo"
        case "mcp": return "MCP"
        default: return "Tool"
        }
    }

    var chipDetail: String {
        if isScript { return script.map(Self.scriptSummary) ?? "" }
        if isToolSearch { return string("query") ?? "" }
        switch tag {
        case "exec": return string("command") ?? ""
        case "readFile", "writeFile", "editFile": return shortPath(string("path") ?? "")
        case "applyPatch":
            let changes = (fields["changes"] as? [String])?.count ?? 0
            return changes == 1 ? "1 file" : "\(changes) files"
        case "search": return string("pattern") ?? ""
        case "glob": return string("pattern") ?? ""
        case "webFetch": return string("url") ?? ""
        case "webSearch": return string("query") ?? ""
        case "todo":
            return string("summary") ?? "task list"
        case "mcp":
            let server = string("server").map { "\($0) · " } ?? ""
            return server + (string("tool") ?? "")
        default: return string("name") ?? ""
        }
    }

    var chipSymbol: String {
        if isScript { return "chevron.left.forwardslash.chevron.right" }
        if isToolSearch { return "magnifyingglass" }
        switch tag {
        case "exec": return "terminal"
        case "readFile", "applyPatch": return "doc.text"
        case "writeFile": return "doc.badge.plus"
        case "editFile": return "pencil"
        case "search": return "magnifyingglass"
        case "glob": return "folder"
        case "webFetch", "webSearch": return "globe"
        case "todo": return "checklist"
        default: return "square.grid.2x2"
        }
    }

    private func shortPath(_ path: String) -> String {
        let comps = path.split(separator: "/")
        guard comps.count > 2 else { return path }
        return comps.suffix(2).joined(separator: "/")
    }

    /// view.rs `script_tools`: the tools a script calls, in order of first
    /// use — `tools.read(…)` and `tools["read"](…)`, an MCP tool by its tool
    /// name. Read off the source, so this names what the script is about;
    /// the nested chips under it say what actually ran.
    static func scriptTools(_ code: String) -> [String] {
        func ident(_ b: UInt8) -> Bool {
            (b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5A) || (b >= 0x61 && b <= 0x7A)
                || b == UInt8(ascii: "_") || b == UInt8(ascii: "$")
        }
        let bytes = Array(code.utf8)
        let global = Array("tools".utf8)
        let quotes: [UInt8] = [UInt8(ascii: "\""), UInt8(ascii: "'"), UInt8(ascii: "`")]
        var names: [String] = []
        var at = 0
        while at + global.count <= bytes.count {
            guard bytes[at..<at + global.count].elementsEqual(global) else {
                at += 1
                continue
            }
            let start = at
            let end = at + global.count
            at = end
            // `ALL_TOOLS`, `myTools.x` and `x.tools.y` are not the global.
            if start > 0, ident(bytes[start - 1]) || bytes[start - 1] == UInt8(ascii: ".") { continue }
            guard end < bytes.count else { continue }
            var name = ""
            if bytes[end] == UInt8(ascii: ".") {
                var stop = end + 1
                while stop < bytes.count, ident(bytes[stop]) { stop += 1 }
                name = String(decoding: bytes[(end + 1)..<stop], as: UTF8.self)
            } else if bytes[end] == UInt8(ascii: "["), end + 1 < bytes.count, quotes.contains(bytes[end + 1]),
                      let close = bytes[(end + 2)...].firstIndex(of: bytes[end + 1]) {
                name = String(decoding: bytes[(end + 2)..<close], as: UTF8.self)
            }
            guard !name.isEmpty else { continue }
            let shown = mcpToolLabel(name) ?? name
            if !names.contains(shown) { names.append(shown) }
        }
        return names
    }

    /// The tool part of a Pi MCP tool name (`mcp__{server}__{tool}`). A
    /// script only carries Pi's sanitized server name; the nested MCP chips
    /// name the server as configured, so the summary leaves it to them.
    private static func mcpToolLabel(_ name: String) -> String? {
        guard name.hasPrefix("mcp__") else { return nil }
        let rest = name.dropFirst("mcp__".count)
        guard let split = rest.range(of: "__") else { return nil }
        let server = rest[..<split.lowerBound]
        let tool = rest[split.upperBound...]
        return server.isEmpty || tool.isEmpty ? nil : String(tool)
    }

    /// view.rs `script_summary`: a script chip's one-line detail — the tools
    /// it calls, else its first line of code (the `// @options:` header is
    /// configuration, not content).
    static func scriptSummary(_ code: String) -> String {
        let tools = scriptTools(code)
        if !tools.isEmpty { return tools.joined(separator: ", ") }
        return code.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty && !$0.hasPrefix("// @options:") } ?? ""
    }

    /// transcript.rs `SCRIPT_DETAIL_MAX_LINES`: a script gets more room than a
    /// command's echo before the counted tail.
    static let scriptBodyMaxLines = 80

    /// transcript.rs `call_block` for a script: the code itself, without the
    /// blank lines around it (a model's script routinely opens with a
    /// newline), capped at `scriptBodyMaxLines` with the rest counted.
    static func scriptBody(_ code: String) -> (code: String, truncatedBy: Int)? {
        var lines = code.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline)
            .drop(while: { $0.trimmingCharacters(in: .whitespaces).isEmpty })
            .map(String.init)
        while lines.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { lines.removeLast() }
        guard !lines.isEmpty else { return nil }
        let truncatedBy = max(lines.count - scriptBodyMaxLines, 0)
        return (lines.prefix(scriptBodyMaxLines).joined(separator: "\n"), truncatedBy)
    }
}

/// "Ran 3 commands · edited 2 files · 1 failed" (transcript.rs
/// tool_group_summary).
func toolGroupSummary(_ tools: [ToolItem]) -> String {
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
    let failed = tools.filter(\.isError).count
    if failed > 0 { segments.append("\(failed) failed") }
    guard var summary = segments.first else { return "\(tools.count) tools" }
    summary = summary.prefix(1).uppercased() + summary.dropFirst()
    return ([summary] + segments.dropFirst()).joined(separator: " · ")
}

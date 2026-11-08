// ```diff fences drawn as diff rows — accent bars, row washes, markers, hunk
// header strips and line numbers when the hunks carry them — so a diff the
// model writes reads like the Changes sheet (desktop markdown/diff_fence.rs,
// parser changes/patch.rs `parse_fence_diff`).

import SwiftUI

enum DiffFence {
    /// Fence tags that render as a diff.
    static func isDiff(_ language: String?) -> Bool {
        guard let tag = language?.lowercased() else { return false }
        return ["diff", "patch", "udiff"].contains(tag)
    }

    enum LineKind: Hashable {
        case context, add, del
        /// `\ No newline at end of file` and friends.
        case meta
    }

    struct Line: Hashable {
        var kind: LineKind
        var oldNo: Int?
        var newNo: Int?
        var text: String
    }

    struct Hunk: Hashable {
        /// Empty for the implicit hunk before any `@@`.
        var header: String
        var lines: [Line] = []
    }

    enum Status: Hashable {
        case added, deleted, modified
    }

    struct File: Hashable {
        /// The post-change side; empty when the fence names no file.
        var path = ""
        var oldPath: String?
        var status = Status.modified
        var binary = false
        var hunks: [Hunk] = []
        var additions = 0
        var deletions = 0
        /// Largest line number on either side: 0 when no hunk is numbered.
        var maxLine = 0

        var notices: [String] {
            var out: [String] = []
            switch status {
            case .added: out.append("New file")
            case .deleted: out.append("Deleted file")
            case .modified:
                if let oldPath { out.append("Renamed from \(oldPath)") }
            }
            if binary { out.append("Binary file — contents not shown") }
            return out
        }
    }

    /// The body of a diff fence. Chat diffs are rarely clean git patches —
    /// hunk headers lack ranges (`@@ fn name`), file headers are missing,
    /// context lines lose their leading space — so this is lenient: lines
    /// before any header open an implicit file and hunk, unprefixed lines are
    /// context, and lines carry numbers only under a numbered `@@ -a +b @@`.
    static func parse(_ source: String) -> [File] {
        var files: [File] = []
        // Next (old, new) line numbers under a numbered hunk header.
        var numbers: (old: Int, new: Int)?
        let lines = source.split(separator: "\n", omittingEmptySubsequences: false).map {
            $0.hasSuffix("\r") ? String($0.dropLast()) : String($0)
        }
        // A trailing newline is no extra blank context line.
        let count = lines.last == "" ? lines.count - 1 : lines.count
        func current() -> Int {
            if files.isEmpty { files.append(File()) }
            return files.count - 1
        }
        var ix = 0
        while ix < count {
            let raw = lines[ix]
            ix += 1
            if raw.hasPrefix("diff --git ") {
                let (old, new) = gitPaths(String(raw.dropFirst("diff --git ".count)))
                files.append(File(path: new, oldPath: old != new ? old : nil))
                numbers = nil
                continue
            }
            // A file header is the `---`/`+++` PAIR; either alone is a body line.
            if raw.hasPrefix("--- "), ix < count, lines[ix].hasPrefix("+++ ") {
                let old = headerPath(String(raw.dropFirst(4)))
                let new = headerPath(String(lines[ix].dropFirst(4)))
                ix += 1
                // `diff --git` already opened this file.
                if !(files.last.map { $0.hunks.isEmpty } ?? false) { files.append(File()) }
                let at = current()
                switch (old, new) {
                case (nil, let new?):
                    files[at].status = .added
                    files[at].path = new
                case (let old?, nil):
                    files[at].status = .deleted
                    files[at].path = old
                case (let old?, let new?):
                    files[at].oldPath = old != new ? old : nil
                    files[at].path = new
                case (nil, nil):
                    break
                }
                numbers = nil
                continue
            }
            if raw.hasPrefix("@@") {
                numbers = hunkNumbers(raw)
                files[current()].hunks.append(Hunk(header: raw))
                continue
            }
            // Git's extended header lines, only between a file header and its
            // first hunk — anywhere else they are ordinary context.
            if let last = files.indices.last, files[last].hunks.isEmpty {
                var header = true
                if raw.hasPrefix("new file mode") {
                    files[last].status = .added
                } else if raw.hasPrefix("deleted file mode") {
                    files[last].status = .deleted
                } else if raw.hasPrefix("Binary files") {
                    files[last].binary = true
                } else {
                    header = ["index ", "similarity index", "rename from", "rename to", "old mode", "new mode"]
                        .contains { raw.hasPrefix($0) }
                }
                if header { continue }
            }
            let at = current()
            let kind: LineKind
            let text: String
            switch raw.first {
            case "+": (kind, text) = (.add, String(raw.dropFirst()))
            case "-": (kind, text) = (.del, String(raw.dropFirst()))
            case " ": (kind, text) = (.context, String(raw.dropFirst()))
            case "\\": (kind, text) = (.meta, raw.drop { $0 == "\\" }.trimmingCharacters(in: .whitespaces))
            default: (kind, text) = (.context, raw)
            }
            var oldNo: Int?
            var newNo: Int?
            if var next = numbers {
                switch kind {
                case .add:
                    newNo = next.new
                    next.new += 1
                case .del:
                    oldNo = next.old
                    next.old += 1
                case .context:
                    oldNo = next.old
                    newNo = next.new
                    next.old += 1
                    next.new += 1
                case .meta:
                    break
                }
                numbers = next
            }
            if kind == .add { files[at].additions += 1 }
            if kind == .del { files[at].deletions += 1 }
            files[at].maxLine = max(files[at].maxLine, oldNo ?? 0, newNo ?? 0)
            if files[at].hunks.isEmpty { files[at].hunks.append(Hunk(header: "")) }
            files[at].hunks[files[at].hunks.count - 1].lines.append(
                Line(kind: kind, oldNo: oldNo, newNo: newNo, text: text))
        }
        return files
    }

    private static func stripGitPrefix(_ path: String) -> String {
        if path.hasPrefix("a/") || path.hasPrefix("b/") { return String(path.dropFirst(2)) }
        return path
    }

    /// One `---`/`+++` header path: nil for `/dev/null`, otherwise without
    /// the `a/`/`b/` prefix or a trailing tab-separated timestamp.
    private static func headerPath(_ raw: String) -> String? {
        let path = (raw.split(separator: "\t", omittingEmptySubsequences: false).first.map(String.init) ?? raw)
            .trimmingCharacters(in: .whitespaces)
        return path == "/dev/null" ? nil : stripGitPrefix(path)
    }

    /// The tail of `diff --git a/… b/…` as (old, new); git's own convention
    /// splits an unquoted path with spaces at the last ` b/`.
    private static func gitPaths(_ rest: String) -> (String, String) {
        func unquote(_ text: Substring) -> String {
            let trimmed = text.trimmingCharacters(in: .whitespaces)
            guard trimmed.count >= 2, trimmed.hasPrefix("\""), trimmed.hasSuffix("\"") else { return trimmed }
            return String(trimmed.dropFirst().dropLast())
                .replacingOccurrences(of: "\\\"", with: "\"")
                .replacingOccurrences(of: "\\\\", with: "\\")
        }
        guard let split = rest.range(of: " b/", options: .backwards) ?? rest.range(of: " \"b/", options: .backwards)
        else {
            let path = stripGitPrefix(unquote(rest[...]))
            return (path, path)
        }
        return (
            stripGitPrefix(unquote(rest[..<split.lowerBound])),
            stripGitPrefix(unquote(rest[rest.index(after: split.lowerBound)...]))
        )
    }

    /// `@@ -10,3 +12,4 @@` → (10, 12); nil for a header without ranges.
    static func hunkNumbers(_ line: String) -> (old: Int, new: Int)? {
        let rest = line.dropFirst(2)
        func number(after sign: Character) -> Int? {
            guard let at = rest.firstIndex(of: sign) else { return nil }
            let digits = rest[rest.index(after: at)...].prefix { $0.isASCII && $0.isNumber }
            return Int(digits)
        }
        guard let old = number(after: "-"), let new = number(after: "+") else { return nil }
        return (old, new)
    }

    /// The language a file's lines are colored as, from its path.
    static func language(of file: File) -> HighlightLanguage? {
        guard !file.path.isEmpty else { return nil }
        return HighlightLanguage.forTag((file.path as NSString).pathExtension)
    }
}

// MARK: - Rows

/// One rendered row of a diff fence: every row is one code line tall, so the
/// code column is a single selectable text and the washes line up beneath it.
struct DiffFenceRow: Hashable {
    enum Kind: Hashable {
        /// A file's strip in a fence of several files.
        case file(additions: Int, deletions: Int)
        case notice
        case hunk
        case line(DiffFence.LineKind)
    }

    var kind: Kind
    var text: String
    var oldNo: Int?
    var newNo: Int?
    /// Whether the file this row belongs to shows line numbers.
    var numbered: Bool
    /// The file's language when its rows take syntax colors.
    var language: HighlightLanguage?

    static func rows(_ files: [DiffFence.File]) -> [DiffFenceRow] {
        let single = files.count == 1
        // Mixed-language fences color only the fence language's files.
        let fenceLanguage = files.lazy.compactMap(DiffFence.language).first
        var rows: [DiffFenceRow] = []
        for file in files {
            let numbered = file.maxLine > 0
            let language = DiffFence.language(of: file)
            let colored = language == nil || language == fenceLanguage ? fenceLanguage : nil
            if !single {
                rows.append(
                    DiffFenceRow(
                        kind: .file(additions: file.additions, deletions: file.deletions),
                        text: file.path.isEmpty ? "diff" : file.path, numbered: false))
            }
            for notice in file.notices {
                rows.append(DiffFenceRow(kind: .notice, text: notice, numbered: false))
            }
            for hunk in file.hunks {
                if !hunk.header.isEmpty {
                    rows.append(DiffFenceRow(kind: .hunk, text: hunk.header, numbered: numbered))
                }
                for line in hunk.lines {
                    rows.append(
                        DiffFenceRow(
                            kind: .line(line.kind), text: line.text, oldNo: line.oldNo, newNo: line.newNo,
                            numbered: numbered, language: line.kind == .meta ? nil : colored))
                }
            }
        }
        return rows
    }
}

// MARK: - View

struct DiffFenceView: View {
    let code: String

    private let files: [DiffFence.File]
    private let rows: [DiffFenceRow]
    @State private var spans: [Int: [TokenSpan]] = [:]
    @State private var width: CGFloat = 0
    @State private var copied = false

    init(code: String) {
        self.code = code
        files = DiffFence.parse(code)
        rows = DiffFenceRow.rows(files)
    }

    private var headerLabel: String {
        if files.count == 1, let path = files.first?.path, !path.isEmpty { return path }
        return "diff"
    }

    private var totals: (Int, Int) {
        files.reduce((0, 0)) { ($0.0 + $1.additions, $0.1 + $1.deletions) }
    }

    /// Digits the widest line number takes.
    private var digits: Int {
        String(files.map(\.maxLine).max() ?? 0).count
    }

    var body: some View {
        FencePanel {
            HStack(spacing: 8) {
                Text(headerLabel).lineLimit(1).truncationMode(.middle)
                DiffStats(additions: totals.0, deletions: totals.1)
                Spacer(minLength: 0)
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .font(.system(size: 11))
                        .frame(width: 22, height: 18)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Copy diff")
                .accessibilityIdentifier("diff-fence-copy")
            }
        } content: {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(alignment: .top, spacing: 0) {
                    if rows.contains(where: \.numbered) {
                        gutter
                    }
                    markers
                    SelectableTranscriptText(attributed: codeText, wraps: false)
                        .padding(.trailing, MD.codePaddingX)
                }
                .frame(minWidth: width, alignment: .leading)
                .background(alignment: .topLeading) { washes }
                .padding(.vertical, 4)
            }
            .onGeometryChange(for: CGFloat.self) {
                $0.size.width
            } action: {
                width = $0
            }
        }
        .accessibilityIdentifier("diff-fence")
        .task(id: code) {
            let rows = rows
            let result = await Task.detached(priority: .utility) { Self.highlight(rows) }.value
            guard !Task.isCancelled else { return }
            spans = result
        }
    }

    /// Old and new line numbers, each in its own column.
    private var gutter: some View {
        let columnWidth = CGFloat(max(digits, 2)) * 6.6 + 8
        return VStack(alignment: .trailing, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                HStack(spacing: 0) {
                    Text(row.oldNo.map(String.init) ?? "").frame(width: columnWidth, alignment: .trailing)
                    Text(row.newNo.map(String.init) ?? "").frame(width: columnWidth, alignment: .trailing)
                }
                .frame(height: MD.codeLineHeight)
            }
        }
        .font(Theme.mono(11))
        .foregroundStyle(Theme.textFaint)
        .padding(.trailing, 4)
        .accessibilityHidden(true)
    }

    private var markers: some View {
        VStack(spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                Text(Self.marker(row))
                    .foregroundStyle(Self.markerColor(row))
                    .frame(width: 18, height: MD.codeLineHeight)
            }
        }
        .font(Theme.mono(MD.codeTextSize))
        .padding(.leading, 4)
        .accessibilityHidden(true)
    }

    private var washes: some View {
        VStack(spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                Self.wash(row)
                    .frame(maxWidth: .infinity)
                    .frame(height: MD.codeLineHeight)
                    .overlay(alignment: .leading) {
                        if let bar = Self.bar(row) { Rectangle().fill(bar).frame(width: 2) }
                    }
            }
        }
    }

    private static func marker(_ row: DiffFenceRow) -> String {
        switch row.kind {
        case .line(.add): "+"
        case .line(.del): "−"
        default: ""
        }
    }

    private static func markerColor(_ row: DiffFenceRow) -> Color {
        switch row.kind {
        case .line(.add): Theme.success
        case .line(.del): Theme.danger
        default: Theme.textFaint
        }
    }

    private static func wash(_ row: DiffFenceRow) -> Color {
        switch row.kind {
        case .line(.add): Theme.success.opacity(0.10)
        case .line(.del): Theme.danger.opacity(0.10)
        case .hunk: Theme.accent.opacity(0.06)
        case .file, .notice: whiteAlpha(0.03)
        case .line: .clear
        }
    }

    private static func bar(_ row: DiffFenceRow) -> Color? {
        switch row.kind {
        case .line(.add): Theme.success.opacity(0.8)
        case .line(.del): Theme.danger.opacity(0.8)
        default: nil
        }
    }

    /// Syntax colors per row index, from one pass per language over the rows
    /// it colors (old and new lines side by side, as the desktop does).
    nonisolated private static func highlight(_ rows: [DiffFenceRow]) -> [Int: [TokenSpan]] {
        var byLanguage: [HighlightLanguage: [Int]] = [:]
        for (ix, row) in rows.enumerated() {
            if let language = row.language { byLanguage[language, default: []].append(ix) }
        }
        var out: [Int: [TokenSpan]] = [:]
        for (language, indices) in byLanguage {
            let source = indices.map { rows[$0].text }.joined(separator: "\n")
            let lines = Highlighter.highlight(code: source, language: language)
            for (n, ix) in indices.enumerated() where n < lines.count {
                out[ix] = lines[n]
            }
        }
        return out
    }

    /// The code column: every row's text on its own line, muted where it is
    /// a strip or a hunk header, syntax-colored where highlighting reached.
    private var codeText: NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.minimumLineHeight = MD.codeLineHeight
        paragraph.maximumLineHeight = MD.codeLineHeight
        let result = NSMutableAttributedString()
        for (ix, row) in rows.enumerated() {
            if ix > 0 { result.append(NSAttributedString(string: "\n")) }
            let muted: Bool
            switch row.kind {
            case .file, .notice, .hunk, .line(.meta): muted = true
            case .line: muted = false
            }
            let start = result.length
            result.append(
                NSAttributedString(
                    string: row.text,
                    attributes: [
                        .font: muted ? Theme.monoUI(11) : Theme.monoUI(MD.codeTextSize),
                        .foregroundColor: UIColor(muted ? Theme.textMuted : Theme.text.opacity(0.9)),
                    ]))
            let characters = Array(row.text)
            for span in spans[ix] ?? [] {
                let lower = span.range.lowerBound
                let upper = min(span.range.upperBound, characters.count)
                guard lower >= 0, lower < upper else { continue }
                let range = NSRange(
                    location: start + String(characters[..<lower]).utf16.count,
                    length: String(characters[lower..<upper]).utf16.count)
                result.addAttribute(.foregroundColor, value: UIColor(Self.tokenColor(span.cls)), range: range)
            }
        }
        result.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: result.length))
        return result
    }

    private static func tokenColor(_ cls: TokenClass) -> Color {
        switch cls {
        case .keyword: Theme.tokenKeyword
        case .stringLit: Theme.tokenString
        case .number: Theme.tokenNumber
        case .comment: Theme.textFaint
        }
    }
}

/// `+N −M` in the diff hues; a side with nothing stays out.
struct DiffStats: View {
    let additions: Int
    let deletions: Int

    var body: some View {
        HStack(spacing: 6) {
            if additions > 0 { Text("+\(additions)").foregroundStyle(Theme.success) }
            if deletions > 0 { Text("−\(deletions)").foregroundStyle(Theme.danger) }
        }
        .font(Theme.mono(11))
        .monospacedDigit()
    }
}

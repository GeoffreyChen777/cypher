// Which files a chat reply previews beyond images, and how a csv/tsv head
// becomes a small table — a port of the desktop's
// crates/ui/src/markdown/file_preview.rs. Both apps run the same table,
// protocol/vectors/file-previews-v1.json (FilePreviewTests); its
// `description` is the spec.
//
// - A path (cleaned and resolved like a local image path, see
//   `ImageSource.localPath`) previews by its extension, any case: csv,
//   tsv/tab, pdf, mp4/mov/m4v/webm, html/htm and svg. URLs never do.
// - Only the head of a csv/tsv file is read (`tableReadBytes`); the table
//   shows `tableRows` data rows with cells clipped to `columnChars`, and the
//   row count reads "N+" when the head was all that was read.
// - PDFs, videos and html/svg pages above their caps show a placeholder.

import Foundation

enum FilePreview {
    /// Only this much of the head of a csv/tsv file is read.
    static let tableReadBytes: UInt64 = 256 * 1024
    /// Data rows a table preview shows.
    static let tableRows = 20
    /// The most characters (Unicode scalars) a shown cell has.
    static let columnChars = 40
    /// Larger PDFs show a "too large" placeholder.
    static let pdfMaxBytes: UInt64 = 50 * 1024 * 1024
    /// Larger videos show a "too large" placeholder.
    static let videoMaxBytes: UInt64 = 200 * 1024 * 1024
    /// Larger html and svg files show a "too large" placeholder.
    static let htmlMaxBytes: UInt64 = 5 * 1024 * 1024

    /// What kind of preview a file gets.
    enum Kind: String, Hashable, Sendable {
        case csv, tsv, pdf, video, html, svg

        /// By file extension, any case.
        init?(extension ext: some StringProtocol) {
            switch ext.lowercased() {
            case "csv": self = .csv
            case "tsv", "tab": self = .tsv
            case "pdf": self = .pdf
            case "mp4", "mov", "m4v", "webm": self = .video
            case "html", "htm": self = .html
            case "svg": self = .svg
            default: return nil
            }
        }

        /// The table delimiter, for csv and tsv.
        var delimiter: Unicode.Scalar? {
            switch self {
            case .csv: ","
            case .tsv: "\t"
            case .pdf, .video, .html, .svg: nil
            }
        }

        /// The most bytes a file of this kind may be (for a table: how much
        /// of its head is read).
        var maxBytes: UInt64 {
            switch self {
            case .csv, .tsv: FilePreview.tableReadBytes
            case .pdf: FilePreview.pdfMaxBytes
            case .video: FilePreview.videoMaxBytes
            case .html, .svg: FilePreview.htmlMaxBytes
            }
        }
    }

    /// The preview kind of a source (a path as written in a reply), if any.
    static func kind(_ src: String) -> Kind? {
        guard let path = ImageSource.localPath(src), let ext = ImageSource.fileExtension(path) else { return nil }
        return Kind(extension: ext)
    }

    /// The file a whole reply paragraph names, given as its Markdown source:
    /// one path, bare, in backticks or in `<…>` (the image-line token rules),
    /// that previews here. An svg is left to the image rules (it shows as a
    /// picture), so nil for it as for anything else.
    static func fileLine(_ line: String) -> String? {
        guard let token = ImageSource.lineToken(line), let kind = kind(token), kind != .svg else { return nil }
        return token
    }

    // MARK: Tables

    /// A csv/tsv head as a preview table.
    struct Table: Hashable, Sendable {
        /// The header's cells, when the first record is a header.
        var columns: [String]?
        /// The first `tableRows` data rows, padded to one width.
        var rows: [[String]] = []
        /// Data rows in the text read (the header excluded).
        var totalRows = 0
        /// Only the head of the file was read: the count is a lower bound.
        var truncated = false

        /// "1 row", "12 rows", "20+ rows".
        var countLabel: String {
            let plus = truncated ? "+" : ""
            let noun = totalRows == 1 && !truncated ? "row" : "rows"
            return "\(totalRows)\(plus) \(noun)"
        }

        /// Columns in every shown row.
        var width: Int { rows.first?.count ?? columns?.count ?? 0 }
    }

    /// Parse `text` (what was read of a file) as a table; `complete` is
    /// false when it is only the head of the file.
    static func parseTable(_ text: String, delimiter: Unicode.Scalar, complete: Bool) -> Table {
        var scalars = Substring(text).unicodeScalars
        if scalars.first == "\u{feff}" { scalars.removeFirst() }
        let records = Self.records(Array(scalars), delimiter: delimiter, complete: complete)
            .filter { !($0.count == 1 && $0[0].isEmpty) }
            .map { $0.map(display) }
        let header = records.count >= 2 && isHeader(records[0])
        let columns = header ? records[0] : nil
        let data = header ? records.dropFirst() : records[...]
        var rows = Array(data.prefix(tableRows))
        let width = (rows + (columns.map { [$0] } ?? [])).map(\.count).max() ?? 0
        func pad(_ row: [String]) -> [String] {
            (row + Array(repeating: "", count: max(0, width - row.count))).map(clip)
        }
        rows = rows.map(pad)
        return Table(columns: columns.map(pad), rows: rows, totalRows: data.count, truncated: !complete)
    }

    /// RFC 4180 records, raw cells (quotes resolved, line breaks kept).
    private static func records(
        _ scalars: [Unicode.Scalar], delimiter: Unicode.Scalar, complete: Bool
    ) -> [[String]] {
        var records: [[String]] = []
        var record: [String] = []
        var field = String.UnicodeScalarView()
        var atFieldStart = true
        var inQuotes = false
        // Whether the text ended right after a record break outside quotes.
        var endedOnBreak = true
        var at = 0
        func endField() {
            record.append(String(field))
            field = String.UnicodeScalarView()
        }
        while at < scalars.count {
            let c = scalars[at]
            at += 1
            endedOnBreak = false
            if inQuotes {
                if c == "\"" {
                    if at < scalars.count, scalars[at] == "\"" {
                        at += 1
                        field.append("\"")
                    } else {
                        inQuotes = false
                    }
                } else {
                    field.append(c)
                }
                continue
            }
            if c == "\"", atFieldStart {
                inQuotes = true
                atFieldStart = false
            } else if c == delimiter {
                endField()
                atFieldStart = true
            } else if c == "\n" || c == "\r" {
                if c == "\r", at < scalars.count, scalars[at] == "\n" { at += 1 }
                endField()
                records.append(record)
                record = []
                atFieldStart = true
                endedOnBreak = true
            } else {
                field.append(c)
                atFieldStart = false
            }
        }
        if !endedOnBreak, complete {
            endField()
            records.append(record)
        }
        return records
    }

    /// A cell as shown: line breaks as spaces, trimmed.
    private static func display(_ cell: String) -> String {
        var out = String.UnicodeScalarView()
        var scalars = cell.unicodeScalars.makeIterator()
        var pending = scalars.next()
        while let c = pending {
            pending = scalars.next()
            if c == "\r" {
                if pending == "\n" { pending = scalars.next() }
                out.append(" ")
            } else if c == "\n" {
                out.append(" ")
            } else {
                out.append(c)
            }
        }
        return String(out).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func clip(_ cell: String) -> String {
        guard cell.unicodeScalars.count > columnChars else { return cell }
        var out = String.UnicodeScalarView(cell.unicodeScalars.prefix(columnChars - 1))
        out.append("…")
        return String(out)
    }

    private static func isHeader(_ first: [String]) -> Bool {
        first.enumerated().allSatisfy { at, cell in
            !cell.isEmpty && !isNumeric(cell) && !first[..<at].contains(cell)
        }
    }

    /// `-1.5`, `.5`, `+2e10`, `12%`; not `1,000`.
    static func isNumeric(_ cell: String) -> Bool {
        var s = Substring(cell)
        if s.hasSuffix("%") { s = s.dropLast() }
        if s.hasPrefix("+") || s.hasPrefix("-") { s = s.dropFirst() }
        let mantissa: Substring
        var exponent: Substring?
        if let at = s.firstIndex(where: { $0 == "e" || $0 == "E" }) {
            mantissa = s[..<at]
            exponent = s[s.index(after: at)...]
        } else {
            mantissa = s
        }
        let whole: Substring
        let fraction: Substring
        if let dot = mantissa.firstIndex(of: ".") {
            whole = mantissa[..<dot]
            fraction = mantissa[mantissa.index(after: dot)...]
        } else {
            whole = mantissa
            fraction = ""
        }
        func digits(_ part: Substring) -> Bool {
            part.unicodeScalars.allSatisfy { $0.value >= 0x30 && $0.value <= 0x39 }
        }
        let mantissaOK = digits(whole) && digits(fraction) && !(whole.isEmpty && fraction.isEmpty)
        var exponentOK = true
        if var exp = exponent {
            if exp.hasPrefix("+") || exp.hasPrefix("-") { exp = exp.dropFirst() }
            exponentOK = !exp.isEmpty && digits(exp)
        }
        return mantissaOK && exponentOK
    }

    // MARK: Naming

    /// The file name a source shows (`results.csv` for `out/results.csv:3`).
    static func fileName(_ src: String) -> String {
        let path = ImageSource.localPath(src) ?? src
        var trimmed = Substring(path)
        while trimmed.hasSuffix("/") { trimmed = trimmed.dropLast() }
        let name = trimmed.split(separator: "/").last.map(String.init) ?? ""
        return name.isEmpty ? src : name
    }

    /// `512 B`, `12 KB`, `3.4 MB` (desktop file_card.rs `human_size`).
    static func humanSize(_ bytes: UInt64) -> String {
        let kb = 1024.0
        let b = Double(bytes)
        if b < kb { return "\(bytes) B" }
        if b < kb * kb { return String(format: "%.0f KB", b / kb) }
        if b < kb * kb * kb { return String(format: "%.1f MB", b / kb / kb) }
        return String(format: "%.1f GB", b / kb / kb / kb)
    }
}

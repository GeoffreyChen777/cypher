// File references and file links in transcript text (desktop
// transcript/code_ref.rs, files/links.rs): a reference in inline code shows
// as a chip — a file glyph, the file name and muted lines, no code wash — and
// a tap on a chip or a file link opens the file in the session's Files sheet
// at its lines. A long press shows the cited lines from the checkout as it is
// now, with Open and Copy.

import SwiftUI
import UIKit

extension NSAttributedString.Key {
    /// Over a file reference chip: the inline code it was written as — what
    /// a copy of it holds.
    static let cypherFileRef = NSAttributedString.Key("cypher.fileRef")
}

extension EnvironmentValues {
    /// The paths a transcript entry cites, by entry id: a bare name in a
    /// reply is read as the one longer path of that name the reply cites.
    @Entry var fileReferenceContext: ((String) -> [String])? = nil
    /// Reads a reference's cited lines for its long-press preview.
    @Entry var fileReferenceSnippet: ((WorkspaceReference) async throws -> FileReferenceSnippet)? = nil
}

/// The URLs chips and file links carry in transcript text, and what they open.
enum TranscriptFileLinks {
    /// A chip's link: the inline code as written.
    static let referenceScheme = "cypher-ref"
    /// A file link's: its destination as written (a bare path is no URL a
    /// text view keeps).
    static let linkScheme = "cypher-link"

    static func url(scheme: String, raw: String) -> URL? {
        URL(string: "\(scheme):" + (raw.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""))
    }

    /// The URL a Markdown link's destination gets in transcript text.
    static func url(forLink destination: String) -> URL? {
        if FileLink.parse(destination) != nil { return url(scheme: linkScheme, raw: destination) }
        return URL(string: destination)
    }

    /// What a chip's or a file link's URL opens; nil for any other link.
    static func reference(_ url: URL, context: [String]) -> WorkspaceReference? {
        guard let scheme = url.scheme,
            let raw = url.absoluteString.dropFirst(scheme.count + 1).removingPercentEncoding
        else { return nil }
        switch scheme {
        case referenceScheme:
            guard let ref = FileReference.detect(raw) else { return nil }
            return WorkspaceReference(path: ref.path, lines: ref.lines, context: context)
        case linkScheme:
            guard let link = FileLink.parse(raw) else { return nil }
            return WorkspaceReference(path: link.path, lines: link.lines, dir: link.dir)
        default:
            return nil
        }
    }

    /// What Copy puts on the clipboard for a chip or a file link: the
    /// reference as written.
    static func written(_ url: URL) -> String? {
        guard let scheme = url.scheme, [referenceScheme, linkScheme].contains(scheme) else { return nil }
        return url.absoluteString.dropFirst(scheme.count + 1).removingPercentEncoding
    }

    /// Every path the reply `entry` cites in inline code.
    static func citedPaths(in entry: MessageEntry?) -> [String] {
        guard let entry else { return [] }
        var paths: [String] = []
        for part in entry.parts {
            guard case .text(_, let text, _) = part else { continue }
            for match in text.matches(of: /`([^`\n]+)`/) {
                if let ref = FileReference.detect(String(match.1)), !paths.contains(ref.path) {
                    paths.append(ref.path)
                }
            }
        }
        return paths
    }
}

extension NSAttributedString.Key {
    /// Over a chip's leading glue: the color the file glyph is painted in
    /// over it (`TranscriptTextLayoutManager`).
    static let cypherFileGlyph = NSAttributedString.Key("cypher.fileGlyph")
}

@MainActor
enum FileReferenceChip {
    /// Glue before a chip's label, painted over with a file glyph: two
    /// no-break spaces in the code font (about one em), so the chip's width —
    /// and the line breaking around it — is settled by the text alone
    /// (code_ref.rs `ICON_GLUE`).
    static let glue = "\u{a0}\u{a0}"
    /// Between a chip and the prose beside it (render.rs
    /// `INLINE_CODE_MARGIN`). Copies as nothing.
    static let margin = "\u{2009}"

    /// The chip for `raw` (the inline code as written), showing `label` —
    /// the file name (or `folder/name` where names clash) and its lines.
    static func string(raw: String, reference: FileReference, label: String, size: CGFloat) -> NSAttributedString {
        let font = Theme.monoUI(size - 1.5)
        let suffix = reference.lineSuffix
        let name = String(label.dropLast(suffix.count))
        let chip = NSMutableAttributedString(
            string: glue,
            attributes: [.font: font, .cypherFileGlyph: UIColor(Theme.textMuted)])
        chip.append(NSAttributedString(string: name, attributes: [.font: font, .foregroundColor: UIColor(Theme.text)]))
        if !suffix.isEmpty {
            chip.append(
                NSAttributedString(
                    string: suffix, attributes: [.font: font, .foregroundColor: UIColor(Theme.textMuted)])
            )
        }
        let full = NSRange(location: 0, length: chip.length)
        chip.addAttribute(.cypherFileRef, value: raw, range: full)
        if let url = TranscriptFileLinks.url(scheme: TranscriptFileLinks.referenceScheme, raw: raw) {
            chip.addAttribute(.link, value: url, range: full)
        }
        return chip
    }

    /// The thin space between a chip and prose, in the prose's font.
    static func margin(_ attributes: [NSAttributedString.Key: Any]) -> NSAttributedString {
        var attributes = attributes
        attributes[.cypherFileRef] = ""
        return NSAttributedString(string: margin, attributes: attributes)
    }

    /// Paint each chip's file glyph over its glue: the desktop's document
    /// icon, stroked in the glyph color, centered on the label's cap height.
    nonisolated static func paintGlyphs(
        _ layout: NSLayoutManager, storage: NSTextStorage, container: NSTextContainer,
        characters: NSRange, origin: CGPoint
    ) {
        storage.enumerateAttribute(.cypherFileGlyph, in: characters) { value, range, _ in
            guard let color = value as? UIColor,
                let font = storage.attribute(.font, at: range.location, effectiveRange: nil) as? UIFont
            else { return }
            let glyphs = layout.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
            guard glyphs.length > 0 else { return }
            let box = layout.boundingRect(forGlyphRange: glyphs, in: container)
            let line = layout.lineFragmentRect(forGlyphAt: glyphs.location, effectiveRange: nil)
            let baseline = line.minY + layout.location(forGlyphAt: glyphs.location).y
            let side = min((font.pointSize * 0.95).rounded(), box.width)
            let rect = CGRect(
                x: origin.x + box.minX + (box.width - side) / 2 - 1,
                y: origin.y + baseline - font.capHeight / 2 - side / 2,
                width: side, height: side)
            let path = UIBezierPath(cgPath: LineIconShape(icon: .document).path(in: rect).cgPath)
            path.lineWidth = 1.5 * side / 24
            path.lineCapStyle = .round
            path.lineJoinStyle = .round
            color.setStroke()
            path.stroke()
        }
    }
}

// MARK: - Long-press preview

/// A reference's cited lines with a little context, from the checkout as it
/// is now.
struct FileReferenceSnippet: Equatable {
    /// Where the reference resolved, relative to the checkout.
    var path: String
    /// The first line `lines` holds.
    var firstLine: Int
    var lines: [String]
    /// The cited range, to tint.
    var cited: ClosedRange<Int>?

    /// Context lines either side of the cited ones.
    static let context = 3
    /// Lines a preview shows at most.
    static let maxLines = 14

    static func cut(_ text: String, path: String, cited: ClosedRange<Int>?) -> FileReferenceSnippet {
        let all = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        guard let cited, cited.lowerBound <= all.count else {
            return FileReferenceSnippet(path: path, firstLine: 1, lines: Array(all.prefix(maxLines)), cited: nil)
        }
        let first = max(1, cited.lowerBound - context)
        let last = min(all.count, cited.upperBound + context, first + maxLines - 1)
        return FileReferenceSnippet(
            path: path, firstLine: first, lines: Array(all[(first - 1)..<last]), cited: cited)
    }
}

/// The long-press card: the resolved path over the cited lines.
struct FileReferencePreviewCard: View {
    let reference: WorkspaceReference
    let load: (WorkspaceReference) async throws -> FileReferenceSnippet
    @State private var snippet: FileReferenceSnippet?
    @State private var error: String?

    static let width: CGFloat = 340
    static func height(for reference: WorkspaceReference) -> CGFloat {
        let lines = reference.lines.map {
            min($0.count + 2 * FileReferenceSnippet.context, FileReferenceSnippet.maxLines)
        }
        return 44 + CGFloat(lines ?? FileReferenceSnippet.maxLines) * 18 + 16
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text").font(.system(size: 11))
                Text(
                    (snippet?.path ?? reference.expandedPath)
                        + FileReference(path: "", lines: reference.lines).lineSuffix
                )
                .font(Theme.mono(11))
                .lineLimit(1)
                .truncationMode(.head)
            }
            .foregroundStyle(Theme.textMuted)
            .padding(.horizontal, 12)
            .frame(height: 36)
            Rectangle().fill(Theme.border).frame(height: 1)
            if let snippet {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(snippet.lines.enumerated()), id: \.offset) { ix, line in
                        let number = snippet.firstLine + ix
                        let cited = snippet.cited?.contains(number) ?? false
                        HStack(spacing: 10) {
                            Text("\(number)")
                                .foregroundStyle(Theme.textFaint)
                                .frame(minWidth: 28, alignment: .trailing)
                            Text(line.isEmpty ? " " : line)
                                .foregroundStyle(cited ? Theme.text : Theme.textMuted)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                        }
                        .font(Theme.mono(11.5))
                        .frame(height: 18)
                        .padding(.horizontal, 8)
                        .background(cited ? Theme.accent.opacity(0.12) : .clear)
                    }
                }
                .padding(.vertical, 8)
            } else if let error {
                Text(error)
                    .font(Theme.sans(12))
                    .foregroundStyle(Theme.textMuted)
                    .padding(12)
            } else {
                ProgressView().frame(maxWidth: .infinity).padding(16)
            }
            Spacer(minLength: 0)
        }
        .frame(width: Self.width, height: Self.height(for: reference), alignment: .top)
        .background(Theme.sheetPanel)
        .task {
            do {
                snippet = try await load(reference)
            } catch {
                self.error = WorkspaceBrowserSession.errorMessage(error)
            }
        }
    }
}

extension WorkspaceBrowserSession {
    /// The snippet a reference's long-press preview shows.
    func snippet(_ reference: WorkspaceReference) async throws -> FileReferenceSnippet {
        switch try await resolve(reference) {
        case .file(let path):
            let content = try await file(path)
            guard let text = content.text, !content.binary else {
                throw WorkspacePreviewError.notText
            }
            return .cut(text, path: path, cited: reference.lines)
        case .directory:
            throw WorkspacePreviewError.folder
        case .ambiguous(let paths):
            throw WorkspacePreviewError.ambiguous(paths.count)
        case .missing:
            throw WorkspacePreviewError.missing
        case .outside:
            throw WorkspacePreviewError.outside
        }
    }
}

enum WorkspacePreviewError: LocalizedError {
    case notText, folder
    case ambiguous(Int)
    case missing, outside
    var errorDescription: String? {
        switch self {
        case .notText: "This file has no text preview."
        case .folder: "A folder — open it to see what it holds."
        case .ambiguous(let count): "\(count) files end in this path. Open to choose one."
        case .missing: "Not found in this checkout."
        case .outside: "Outside this session's checkout."
        }
    }
}

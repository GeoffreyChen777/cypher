// File references and file links in transcript text (desktop
// transcript/code_ref.rs, files/links.rs): a reference in inline code shows
// as a chip — a file glyph, the file name and muted lines on a faint neutral
// wash with a hairline outline, told from plain inline code by its shape,
// not by color — and a tap on a chip or a file link opens the file in the session's Files sheet
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
    /// Over a whole chip (glue, label and its right padding): its index in
    /// its paragraph, so two chips side by side stay two boxes.
    static let cypherFileChip = NSAttributedString.Key("cypher.fileChip")
}

/// A file reference chip (desktop markdown/render.rs `ChipStyle`): the file
/// glyph, the name in medium-weight code type and the muted lines, on a
/// faint neutral wash with a hairline outline, so a reference stands out
/// from the prose and from plain inline code by its shape, not by color.
///
/// The text holds the chip's padding — its leading glue the left padding,
/// the glyph and the gap to the name, a kern after its last character the
/// right padding — so its width, and the line breaking around it, is
/// settled by the text alone. The box is painted under the text and never
/// changes the line height.
@MainActor
enum FileReferenceChip {
    /// Glue before a chip's label, painted over with a file glyph: two
    /// no-break spaces in the name's font, kerned to the chip's left padding,
    /// the glyph and the gap to the name (code_ref.rs `ICON_GLUE`).
    static let glue = "\u{a0}\u{a0}"
    /// Between a chip and the prose beside it (render.rs
    /// `INLINE_CODE_MARGIN`). Copies as nothing.
    static let margin = "\u{2009}"

    /// The name's and the lines' size, in em of the prose: the one code
    /// scale inline code is set at too (`Theme.codeScale`).
    nonisolated static let nameEm: CGFloat = Theme.codeScale
    /// The file glyph's side, in em of the prose (render.rs `CHIP_GLYPH_EM`).
    nonisolated static let glyphEm: CGFloat = 0.86
    /// The box's height, in em of the prose: 17 pt on 14 pt body text, a
    /// point under the desktop's 18. TextKit sets a line's text at its
    /// bottom (a 22 pt line has its baseline at 18), so an 18 pt box centered
    /// on the name would cross into the next line, and a text view clips
    /// its last line there; 17 pt centers on the name and stays inside.
    nonisolated static let heightEm: CGFloat = 17.0 / 14.0
    /// Corner radius, the box's edge to the glyph, the glyph to the name,
    /// and the label's end to the box's edge, in pt (render.rs `CHIP_RADIUS`,
    /// `CHIP_PAD_LEFT`, `CHIP_PAD_RIGHT`).
    nonisolated static let radius: CGFloat = 6
    nonisolated static let padLeft: CGFloat = 5
    nonisolated static let glyphGap: CGFloat = 4
    nonisolated static let padRight: CGFloat = 6
    /// The padding on the broken edges of a chip wider than its line
    /// (render.rs `CHIP_PAD_WRAP`).
    nonisolated static let padWrap: CGFloat = 3

    /// The wash and the hairline outline's alphas: black ink on light,
    /// white on dark, the same numbers on both (render.rs `CHIP_WASH_ALPHA`,
    /// `CHIP_HAIRLINE_ALPHA` at rest). A text view has no pressed state for a
    /// link to paint, so the desktop's hover values have no use here.
    nonisolated static let washAlpha: CGFloat = 0.03
    nonisolated static let hairlineAlpha: CGFloat = 0.08
    nonisolated static let wash = ink(washAlpha)
    nonisolated static let hairline = ink(hairlineAlpha)

    /// Neutral ink at exactly `alpha` in either appearance — not
    /// `whiteAlpha`, whose colors other surfaces may retune.
    nonisolated static func ink(_ alpha: CGFloat) -> UIColor {
        UIColor { traits in
            UIColor(white: traits.userInterfaceStyle == .dark ? 1 : 0, alpha: alpha)
        }
    }

    /// A chip label's weight: medium in prose, semibold in bold text, never
    /// lighter than the block's own (a heading's) (render.rs `chip_weight`).
    static func weight(base: UIFont.Weight, bold: Bool) -> UIFont.Weight {
        let floor: UIFont.Weight = bold ? .semibold : .medium
        return base.rawValue < floor.rawValue ? floor : base
    }

    /// The chip for `raw` (the inline code as written), showing `label` —
    /// the file name (or `folder/name` where names clash) and its lines —
    /// in prose of `size`. `index` tells it from a chip right beside it.
    static func string(
        raw: String, reference: FileReference, label: String, size: CGFloat,
        weight: UIFont.Weight = .medium, index: Int = 0
    ) -> NSAttributedString {
        let font = Theme.monoUI(size * nameEm, weight: weight)
        let suffix = reference.lineSuffix
        let name = String(label.dropLast(suffix.count))
        let chip = NSMutableAttributedString(
            string: glue,
            attributes: [.font: font, .cypherFileGlyph: UIColor(Theme.textMuted)])
        // The glue's last space carries the room the glyph needs past both.
        let space = NSAttributedString(string: "\u{a0}", attributes: [.font: font]).size().width
        let room = padLeft + glyphSide(size) + glyphGap
        chip.addAttribute(.kern, value: room - 2 * space, range: NSRange(location: 1, length: 1))
        chip.append(NSAttributedString(string: name, attributes: [.font: font, .foregroundColor: UIColor(Theme.text)]))
        if !suffix.isEmpty {
            chip.append(
                NSAttributedString(
                    string: suffix, attributes: [.font: font, .foregroundColor: UIColor(Theme.textMuted)])
            )
        }
        chip.addAttribute(.kern, value: padRight, range: NSRange(location: chip.length - 1, length: 1))
        let full = NSRange(location: 0, length: chip.length)
        chip.addAttribute(.cypherFileRef, value: raw, range: full)
        chip.addAttribute(.cypherFileChip, value: index, range: full)
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

    /// The file glyph's side in prose of `size`, whole points.
    nonisolated static func glyphSide(_ size: CGFloat) -> CGFloat {
        (size * glyphEm).rounded()
    }

    /// The chip around `chip` (a whole chip's characters), one box per line
    /// it spans, in the container's coordinates: as tall as `heightEm` of
    /// the prose, centered on the name's cap height but kept inside its line
    /// (the text sits low in a line, and the box must not reach into the
    /// next one or past the view's edge). Edges where a chip wider than its
    /// line breaks get `padWrap`; no box leaves the container.
    nonisolated static func boxes(
        _ layout: NSLayoutManager, storage: NSTextStorage, container: NSTextContainer, chip: NSRange
    ) -> [CGRect] {
        guard let font = storage.attribute(.font, at: chip.location, effectiveRange: nil) as? UIFont else {
            return []
        }
        let height = (font.pointSize / nameEm * heightEm).rounded()
        let glyphs = layout.glyphRange(forCharacterRange: chip, actualCharacterRange: nil)
        var lines: [(rect: CGRect, line: CGRect)] = []
        layout.enumerateLineFragments(forGlyphRange: glyphs) { line, _, _, lineGlyphs, _ in
            let part = NSIntersectionRange(lineGlyphs, glyphs)
            guard part.length > 0 else { return }
            let rect = layout.boundingRect(forGlyphRange: part, in: container)
            let baseline = line.minY + layout.location(forGlyphAt: part.location).y
            var top = baseline - font.capHeight / 2 - height / 2
            top = max(line.minY, min(top, line.maxY - height))
            lines.append((CGRect(x: rect.minX, y: top, width: rect.width, height: height), line))
        }
        let width = container.size.width > 0 ? container.size.width : .greatestFiniteMagnitude
        return lines.enumerated().map { ix, entry in
            var left = entry.rect.minX - (ix == 0 ? 0 : padWrap)
            var right = entry.rect.maxX + (ix == lines.count - 1 ? 0 : padWrap)
            left = max(0, left)
            right = min(width, right)
            return CGRect(x: left, y: entry.rect.minY, width: max(0, right - left), height: entry.rect.height)
        }
    }

    /// Paint each chip in `characters` under its text: the wash with its
    /// hairline outline, and the desktop's document glyph, stroked in the
    /// glyph color, `padLeft` in from the box's edge and centered in it.
    /// Edges snap to the device's pixels so the hairline stays crisp.
    nonisolated static func paint(
        _ layout: NSLayoutManager, storage: NSTextStorage, container: NSTextContainer,
        characters: NSRange, origin: CGPoint
    ) {
        let all = NSRange(location: 0, length: storage.length)
        let scale = UIGraphicsGetCurrentContext().map { abs($0.userSpaceToDeviceSpaceTransform.a) } ?? 1
        let snap = { (value: CGFloat) in (value * scale).rounded() / scale }
        var painted = Set<Int>()
        storage.enumerateAttribute(.cypherFileChip, in: characters) { value, range, _ in
            guard value != nil else { return }
            // A redraw may cover part of a chip; its box is the whole chip's.
            var chip = NSRange()
            _ = storage.attribute(.cypherFileChip, at: range.location, longestEffectiveRange: &chip, in: all)
            guard painted.insert(chip.location).inserted else { return }
            let boxes = boxes(layout, storage: storage, container: container, chip: chip)
            for box in boxes {
                let box = box.offsetBy(dx: origin.x, dy: origin.y)
                let rect = CGRect(
                    x: snap(box.minX), y: snap(box.minY),
                    width: snap(box.maxX) - snap(box.minX), height: snap(box.maxY) - snap(box.minY))
                wash.setFill()
                UIBezierPath(roundedRect: rect, cornerRadius: radius).fill()
                let edge = UIBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), cornerRadius: radius - 0.5)
                edge.lineWidth = 1
                hairline.setStroke()
                edge.stroke()
            }
            guard let first = boxes.first,
                let color = storage.attribute(.cypherFileGlyph, at: chip.location, effectiveRange: nil) as? UIColor,
                let font = storage.attribute(.font, at: chip.location, effectiveRange: nil) as? UIFont
            else { return }
            let side = glyphSide(font.pointSize / nameEm)
            let rect = CGRect(
                x: snap(origin.x + first.minX + padLeft),
                y: snap(origin.y + first.midY - side / 2),
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

/// Keeps a file reference chip on one line: a chip that no longer fits
/// moves to the next line whole, and only one wider than its line breaks
/// (render.rs's joined glue). Shared by the transcript's text views and
/// their measurer, so a measured height matches the laid-out one.
final class FileReferenceChipLineBreaks: NSObject, NSLayoutManagerDelegate {
    static let shared = FileReferenceChipLineBreaks()

    func layoutManager(
        _ layoutManager: NSLayoutManager, shouldBreakLineByWordBeforeCharacterAt charIndex: Int
    ) -> Bool {
        guard let storage = layoutManager.textStorage, charIndex > 0, charIndex < storage.length,
            let chip = storage.attribute(.cypherFileChip, at: charIndex, effectiveRange: nil) as? Int,
            let before = storage.attribute(.cypherFileChip, at: charIndex - 1, effectiveRange: nil) as? Int
        else { return true }
        // Inside a chip, never; between two chips side by side, as usual.
        return chip != before
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

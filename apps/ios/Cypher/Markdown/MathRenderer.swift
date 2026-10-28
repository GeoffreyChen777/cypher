// LaTeX formulas in chat Markdown — the rendering half (the desktop's
// crates/ui/src/markdown/math.rs, which typesets with RaTeX).
//
// SwiftMath typesets each formula into a black template image; as a text
// attachment it takes the run's foreground color, so appearance changes,
// thinking's muted tone and the streaming veil all reach it the way they
// reach glyphs. Inline formulas sit on the text baseline; display formulas
// get a centered paragraph. Either shrinks to fit a line too narrow for it.

import SwiftMath
import UIKit

extension NSAttributedString.Key {
    /// On a formula's attachment character: what it copies as (the TeX
    /// source with its Markdown delimiters).
    static let cypherMathSource = NSAttributedString.Key("cypher.mathSource")
    /// On a display formula's attachment character.
    static let cypherDisplayMath = NSAttributedString.Key("cypher.displayMath")
}

/// A formula's attachment: its image's own bounds, scaled down when the line
/// it lands on is narrower.
final class MathAttachment: NSTextAttachment {
    override func attachmentBounds(
        for textContainer: NSTextContainer?, proposedLineFragment lineFrag: CGRect,
        glyphPosition position: CGPoint, characterIndex charIndex: Int
    ) -> CGRect {
        let natural = bounds
        guard lineFrag.width > 0, natural.width > lineFrag.width else { return natural }
        let scale = lineFrag.width / natural.width
        return CGRect(
            x: 0, y: natural.origin.y * scale,
            width: natural.width * scale, height: natural.height * scale)
    }
}

@MainActor
enum MathRenderer {
    /// Formula size relative to the surrounding text: Latin Modern's x-height
    /// is smaller than Geist's (the desktop scales KaTeX's by 1.21).
    static let scale: CGFloat = 1.2
    /// Transparent margin around each image, so antialiased edges and
    /// glyphs overhanging their box are not cut off.
    static let padding: CGFloat = 1

    private struct Key: Hashable {
        var tex: String
        var kind: MathKind
        var size: CGFloat
    }

    /// Typeset formulas, failures included (nil). One attachment per key, so
    /// rebuilding a row's text yields an equal attributed string and the
    /// text view is left alone.
    private static var cache: [Key: MathAttachment?] = [:]
    private static let cacheLimit = 512

    /// `tex` as an attachment for text of `size` points, or nil when
    /// SwiftMath cannot typeset it (the caller shows the source as code).
    static func attachment(_ tex: String, kind: MathKind, size: CGFloat) -> MathAttachment? {
        let key = Key(tex: tex, kind: kind, size: size)
        if let hit = cache[key] { return hit }
        let made = typeset(tex, kind: kind, fontSize: size * scale)
        if cache.count >= cacheLimit { cache.removeAll() }
        cache[key] = made
        return made
    }

    private static func typeset(_ tex: String, kind: MathKind, fontSize: CGFloat) -> MathAttachment? {
        let source = aliased(tex.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !source.isEmpty, source.utf8.count <= 8 * 1024 else { return nil }
        func render(bottom: CGFloat) -> (image: UIImage, info: MathImage.LayoutInfo)? {
            var math = MathImage(
                latex: source, fontSize: fontSize, textColor: .black,
                labelMode: kind == .display ? .display : .text, textAlignment: .left)
            math.contentInsets = UIEdgeInsets(top: padding, left: padding, bottom: bottom, right: padding)
            let (error, image, info) = math.asImage()
            guard error == nil, let image, let info, image.size.width > 2 * padding else { return nil }
            return (image, info)
        }
        guard var drawn = render(bottom: padding) else { return nil }
        // MathImage centers a formula shorter than half the font size on that
        // height, which pushes it down; give it the room so nothing clips.
        let height = drawn.info.ascent + drawn.info.descent
        var bottom = padding
        if height < fontSize / 2 {
            bottom += (fontSize / 2 - height) / 2
            guard let again = render(bottom: bottom) else { return nil }
            drawn = again
        }
        let image = drawn.image
        // Where MathImage put the baseline, measured up from the image's
        // bottom edge (its own layout arithmetic).
        let centered = max(height, fontSize / 2)
        let baseline = (height - centered) / 2 + drawn.info.descent + bottom
        let attachment = MathAttachment()
        attachment.image = image.withRenderingMode(.alwaysTemplate)
        attachment.bounds = CGRect(
            x: 0, y: -baseline, width: image.size.width, height: image.size.height)
        return attachment
    }

    /// KaTeX commands models write that SwiftMath lacks, as their nearest
    /// SwiftMath spelling.
    private static func aliased(_ tex: String) -> String {
        guard tex.contains("\\") else { return tex }
        var result = tex
        for (from, to) in aliases {
            result = result.replacing(from, with: to)
        }
        return result
    }

    // Regex literals are never mutated; `Regex` just isn't marked Sendable.
    nonisolated(unsafe) private static let aliases: [(Regex<Substring>, String)] = [
        (/\\[td]frac(?![A-Za-z])/, "\\frac"),
        (/\\operatorname(?![A-Za-z])/, "\\mathrm"),
        (/\\(?:boldsymbol|bm)(?![A-Za-z])/, "\\mathbf"),
    ]

    /// The attachment character for a typeset formula, carrying `attributes`
    /// (its foreground color tints the template image).
    static func string(
        _ attachment: MathAttachment, tex: String, kind: MathKind,
        attributes: [NSAttributedString.Key: Any]
    ) -> NSAttributedString {
        let string = NSMutableAttributedString(attachment: attachment)
        var attributes = attributes
        attributes[.cypherMathSource] = kind == .inline ? "$\(tex)$" : "$$\(tex)$$"
        if kind == .display { attributes[.cypherDisplayMath] = true }
        string.addAttributes(attributes, range: NSRange(location: 0, length: string.length))
        return string
    }

    /// `text[range]` as plain text, formulas as their TeX source and file
    /// reference chips as the inline code they were written as — what a copy
    /// or a quote of a selection holds. nil when the range holds neither.
    nonisolated static func plainText(_ text: NSAttributedString, range: NSRange) -> String? {
        let full = NSRange(location: 0, length: text.length)
        let range = NSIntersectionRange(range, full)
        guard range.length > 0 else { return nil }
        var special = false
        for key in [NSAttributedString.Key.cypherMathSource, .cypherFileRef] where !special {
            text.enumerateAttribute(key, in: range) { value, _, stop in
                if value != nil {
                    special = true
                    stop.pointee = true
                }
            }
        }
        guard special else { return nil }
        var out = ""
        text.enumerateAttribute(.cypherFileRef, in: range) { reference, chip, _ in
            if let written = reference as? String {
                out += written
                return
            }
            text.enumerateAttribute(.cypherMathSource, in: chip) { value, part, _ in
                let piece = (text.string as NSString).substring(with: part)
                guard let source = value as? String else {
                    out += piece
                    return
                }
                // Adjacent copies of one formula share a run: one source per
                // attachment character.
                for character in piece {
                    out += character == "\u{FFFC}" ? source : String(character)
                }
            }
        }
        return out
    }
}

// Inline code in transcript text (desktop markdown/render.rs inline code):
// code type at `Theme.codeScale` of the prose, in the body ink, on a rounded
// neutral wash. The wash is set apart by shape, not color, and is painted
// under the text, so it never changes the line height.

import UIKit

/// The box under a run of inline code: `heightEm` of the code's own size,
/// centered on the code's cap height, `padX` past its glyphs on either side.
///
/// The text holds the room the box needs beside it — a kern of `padX` on the
/// prose character before the code and on the code's last character — so
/// the box never reaches into the prose around it, the spaces either side
/// stay whole, and a copy holds the code as written.
enum InlineCodeBox {
    /// The box's height, in em of the code: 15 pt on 14 pt prose (code
    /// 12.04 pt). TextKit sets a line's text low (a 22 pt line has its
    /// baseline at 18), and a 15 pt box centered on the code's cap height
    /// ends near 21, inside the line, so the last line never clips.
    nonisolated static let heightEm: CGFloat = 1.25
    /// How far the box reaches past the code's glyphs, in pt.
    nonisolated static let padX: CGFloat = 3
    /// The box's corner radius, in pt.
    nonisolated static let radius = MD.inlineCodeRadius

    /// Give inline code at `code` in `text` its room: a `padX` kern on the
    /// character before it (when that is prose on the same line) and on its
    /// own last character.
    static func addRoom(_ text: NSMutableAttributedString, code: NSRange, afterProse: Bool) {
        guard code.length > 0 else { return }
        let string = text.string as NSString
        if afterProse, code.location > 0 {
            let before = code.location - 1
            let isBreak = string.character(at: before) == 0x0A
            let kerned = text.attribute(.kern, at: before, effectiveRange: nil) != nil
            if !isBreak, !kerned {
                text.addAttribute(.kern, value: padX, range: NSRange(location: before, length: 1))
            }
        }
        // The last character's whole UTF-16 sequence, so a surrogate pair
        // stays one kerned character.
        let last = string.rangeOfComposedCharacterSequence(at: NSMaxRange(code) - 1)
        text.addAttribute(.kern, value: padX, range: last)
    }

    /// The box around `code` (a whole run of inline code), one per line it
    /// spans, in the container's coordinates: `heightEm` of the code's size,
    /// centered on its cap height but kept inside its line, `padX` past its
    /// glyphs (the last character's kern already holds the right one), and
    /// never outside the container.
    nonisolated static func boxes(
        _ layout: NSLayoutManager, storage: NSTextStorage, container: NSTextContainer, code: NSRange
    ) -> [CGRect] {
        guard code.length > 0,
            let font = storage.attribute(.font, at: code.location, effectiveRange: nil) as? UIFont
        else { return [] }
        let height = (font.pointSize * heightEm).rounded()
        let glyphs = layout.glyphRange(forCharacterRange: code, actualCharacterRange: nil)
        var lines: [CGRect] = []
        layout.enumerateLineFragments(forGlyphRange: glyphs) { line, _, _, lineGlyphs, _ in
            let part = NSIntersectionRange(lineGlyphs, glyphs)
            guard part.length > 0 else { return }
            let rect = layout.boundingRect(forGlyphRange: part, in: container)
            let baseline = line.minY + layout.location(forGlyphAt: part.location).y
            var top = baseline - font.capHeight / 2 - height / 2
            top = max(line.minY, min(top, line.maxY - height))
            lines.append(CGRect(x: rect.minX, y: top, width: rect.width, height: height))
        }
        let width = container.size.width > 0 ? container.size.width : .greatestFiniteMagnitude
        return lines.enumerated().map { ix, rect in
            let left = max(0, rect.minX - padX)
            let right = min(width, rect.maxX + (ix == lines.count - 1 ? 0 : padX))
            return CGRect(x: left, y: rect.minY, width: max(0, right - left), height: rect.height)
        }
    }

    /// Paint the box under each run of inline code in `characters`, in the
    /// wash color the run carries (`.cypherInlineCode`), edges snapped to the
    /// device's pixels.
    nonisolated static func paint(
        _ layout: NSLayoutManager, storage: NSTextStorage, container: NSTextContainer,
        characters: NSRange, origin: CGPoint
    ) {
        let all = NSRange(location: 0, length: storage.length)
        let scale = UIGraphicsGetCurrentContext().map { abs($0.userSpaceToDeviceSpaceTransform.a) } ?? 1
        let snap = { (value: CGFloat) in (value * scale).rounded() / scale }
        var painted = Set<Int>()
        storage.enumerateAttribute(.cypherInlineCode, in: characters) { value, range, _ in
            guard let color = value as? UIColor else { return }
            // A redraw may cover part of a run; its box is the whole run's.
            var code = NSRange()
            _ = storage.attribute(.cypherInlineCode, at: range.location, longestEffectiveRange: &code, in: all)
            guard painted.insert(code.location).inserted else { return }
            color.setFill()
            for box in boxes(layout, storage: storage, container: container, code: code) {
                let box = box.offsetBy(dx: origin.x, dy: origin.y)
                let rect = CGRect(
                    x: snap(box.minX), y: snap(box.minY),
                    width: snap(box.maxX) - snap(box.minX), height: snap(box.maxY) - snap(box.minY))
                UIBezierPath(roundedRect: rect, cornerRadius: radius).fill()
            }
        }
    }
}

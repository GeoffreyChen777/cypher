// How big a visual preview in a chat reply is: images (Markdown, inline,
// image file references, SVG), a PDF's pages and a video's frame. Tool-row
// thumbnails and csv/tsv tables keep their own sizes. The twin of the
// desktop's crates/ui/src/markdown/preview_size.rs; both run the `sizing`
// section of protocol/vectors/image-sources-v1.json and
// file-previews-v1.json (the same rows in both; its description is the spec):
//
// - A preview is always the reply column's width, a small image or an SVG
//   included (upscaled).
// - Landscape, square and up to exactly `crop` (4:5): its natural aspect.
// - Taller: `crop` tall, the content drawn whole-width from its top edge and
//   cut off below; a tap still opens all of it.
// - A PDF lays its first pages side by side in one row (`pdfRow`): up to
//   `pdfRowMax`, as many as keep every page at least `pdfPageMin` wide, all
//   one height so the row fills the column, never cropped. A single page
//   shows like an image (column wide, the 4:5 crop). Pages left over show as
//   a "+N" badge on the last one.
//
// Every visual preview clips its picture to its rounded frame and draws the
// hairline edge over it (`previewPicture()`).

import SwiftUI

/// A preview's size in a column, in points.
struct PreviewSize: Equatable {
    /// The tallest a preview gets, as width : height (4:5).
    static let crop = CGSize(width: 4, height: 5)
    /// The most pages a PDF's row shows.
    static let pdfRowMax = 4
    /// The narrowest a page in a PDF's row gets; a row that would make one
    /// narrower shows fewer pages.
    static let pdfPageMin: CGFloat = 150
    /// Space between the pages of a PDF's row.
    static let pdfGap: CGFloat = 8
    /// How far under `pdfPageMin` a page may come out and still count as
    /// wide enough (float slack, the same on every platform).
    private static let pdfPageMinSlack: CGFloat = 0.001
    /// The corner radius of every visual preview's frame.
    static let radius: CGFloat = 8

    static let none = PreviewSize(width: 0, height: 0, cropped: false, contentHeight: 0)

    var width: CGFloat
    /// The preview's height: the content's, or the `crop` cap.
    var height: CGFloat
    /// The content is taller than the preview: drawn from its top edge, the
    /// rest cut off.
    var cropped: Bool
    /// The height the whole content draws at (column wide).
    var contentHeight: CGFloat

    /// The size of a preview of content `natural` (only its aspect matters)
    /// in a column `column` wide.
    static func of(column: CGFloat, natural: CGSize) -> PreviewSize {
        guard column > 0, natural.width > 0, natural.height > 0 else { return .none }
        let contentHeight = column * natural.height / natural.width
        let cap = column * crop.height / crop.width
        let cropped = contentHeight > cap
        return PreviewSize(
            width: column, height: cropped ? cap : contentHeight, cropped: cropped, contentHeight: contentHeight)
    }

    /// The row a PDF of `pages` pages shows in a column `column` wide, given
    /// the aspects (width / height) of its first pages, in order (those
    /// known: a page with no positive aspect ends the list).
    ///
    /// n = the largest count up to min(pages, known aspects, `pdfRowMax`)
    /// whose row keeps every page at least `pdfPageMin` wide, where the
    /// row's shared height is h = (column − (n − 1) · `pdfGap`) / Σ aspectᵢ
    /// and page i is h · aspectᵢ wide (so the row fills the column exactly).
    /// No such count of two or more: one page, shown like an image.
    static func pdfRow(column: CGFloat, aspects: [CGFloat], pages: Int) -> PDFRow {
        let known = knownAspects(aspects)
        let most = min(max(pages, 0), known, pdfRowMax)
        guard column > 0, most > 0 else { return PDFRow(pages: [], height: 0, hidden: 0) }
        for n in stride(from: most, through: 2, by: -1) {
            let shown = aspects[..<n]
            let height = (column - CGFloat(n - 1) * pdfGap) / shown.reduce(0, +)
            if shown.allSatisfy({ height * $0 >= pdfPageMin - pdfPageMinSlack }) {
                return PDFRow(
                    pages: shown.map {
                        PreviewSize(width: height * $0, height: height, cropped: false, contentHeight: height)
                    },
                    height: height, hidden: pages - n)
            }
        }
        let page = of(column: column, natural: CGSize(width: aspects[0], height: 1))
        return PDFRow(pages: [page], height: page.height, hidden: pages - 1)
    }

    /// The widths to draw a PDF's first pages at so each stays sharp in any
    /// column up to `column`, given their aspects (as `pdfRow`): a page's
    /// widest across every row it can be in. A page's width in a row of a
    /// given count only grows with the column, and a narrower column may
    /// show fewer, wider pages; so this is, per page, the most over every
    /// count that shows it (one: the whole column), whether or not
    /// `pdfPageMin` would allow it.
    static func pdfDrawWidths(column: CGFloat, aspects: [CGFloat], pages: Int) -> [CGFloat] {
        let most = min(max(pages, 0), knownAspects(aspects), pdfRowMax)
        guard column > 0 else { return Array(repeating: 0, count: most) }
        return (0..<most).map { index in
            var widest: CGFloat = index == 0 ? column : 0
            for n in max(index + 1, 2)..<(most + 1) {
                let height = (column - CGFloat(n - 1) * pdfGap) / aspects[..<n].reduce(0, +)
                widest = max(widest, height * aspects[index])
            }
            return widest
        }
    }

    /// How many leading aspects are usable (finite and positive).
    private static func knownAspects(_ aspects: [CGFloat]) -> Int {
        aspects.firstIndex { !($0.isFinite && $0 > 0) } ?? aspects.count
    }
}

/// A PDF's row of pages in a column: what `PreviewSize.pdfRow` decides.
struct PDFRow: Equatable {
    /// Each shown page's size, in order. A row of two or more: all `height`
    /// tall, never cropped. A single page: as an image (`PreviewSize.of`, so
    /// a tall one is cropped to 4:5, its top kept).
    var pages: [PreviewSize]
    /// The shared height of the shown pages (0 when none show).
    var height: CGFloat
    /// Pages the document has past the shown ones.
    var hidden: Int

    var shown: Int { pages.count }

    /// The badge on the last shown page when some are hidden: "+N".
    var badge: String? { hidden > 0 ? "+\(hidden)" : nil }
}

/// Lays its one subview out by `PreviewSize`: as wide as offered, as tall as
/// the rule says, the subview drawn column wide and `contentHeight` tall from
/// the top (clip the result to cut a cropped one off: `previewPicture()`).
/// Without a width offered it takes the natural width.
struct PreviewFrame: Layout {
    let natural: CGSize

    private func size(_ proposal: ProposedViewSize) -> PreviewSize {
        let column = proposal.width ?? natural.width
        return PreviewSize.of(column: column.isFinite ? column : natural.width, natural: natural)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let size = size(proposal)
        return CGSize(width: size.width, height: size.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let size = size(ProposedViewSize(width: bounds.width, height: bounds.height))
        for subview in subviews {
            subview.place(
                at: bounds.origin, anchor: .topLeading,
                proposal: ProposedViewSize(width: size.width, height: size.contentHeight))
        }
    }
}

/// Lays a PDF's pages out side by side by `PreviewSize.pdfRow`: as wide as
/// offered, as tall as the row; one subview per page in order, each placed
/// at its page's frame. Subviews past the shown pages get no room. Without
/// a width offered it takes `fallbackColumn`.
struct PDFRowLayout: Layout {
    let aspects: [CGFloat]
    let pages: Int
    var fallbackColumn: CGFloat = 343

    private func row(_ width: CGFloat?) -> (column: CGFloat, row: PDFRow) {
        let column = width.flatMap { $0.isFinite ? $0 : nil } ?? fallbackColumn
        return (column, PreviewSize.pdfRow(column: column, aspects: aspects, pages: pages))
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let (column, row) = row(proposal.width)
        return CGSize(width: row.shown > 0 ? column : 0, height: row.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let (_, row) = row(bounds.width)
        var x = bounds.minX
        for (index, subview) in subviews.enumerated() {
            guard index < row.shown else {
                subview.place(at: bounds.origin, anchor: .topLeading, proposal: .zero)
                continue
            }
            let page = row.pages[index]
            subview.place(
                at: CGPoint(x: x, y: bounds.minY), anchor: .topLeading,
                proposal: ProposedViewSize(width: page.width, height: page.height))
            x += page.width + PreviewSize.pdfGap
        }
    }
}

extension View {
    /// A visual preview's picture clipped to its rounded frame
    /// (`PreviewSize.radius`), with the hairline edge drawn over it: no
    /// square corner shows past the rounding, and a light picture (or a
    /// cropped one's cut) still shows where it ends. Apply it to the frame
    /// (a `PreviewFrame` or a page), so a cropped picture's cut corners round
    /// too.
    func previewPicture() -> some View {
        let shape = RoundedRectangle(cornerRadius: PreviewSize.radius, style: .continuous)
        return
            self
            .clipShape(shape)
            .overlay(shape.strokeBorder(FileReferenceChip.hairline.swiftUI, lineWidth: 1))
            .contentShape(shape)
    }
}

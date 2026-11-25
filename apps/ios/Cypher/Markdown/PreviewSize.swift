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
// - A PDF stacks its first `pdfPages` pages, each uncropped at its own aspect
//   (`contentHeight`), then a row "N more pages".

import SwiftUI

/// A preview's size in a column, in points.
struct PreviewSize: Equatable {
    /// The tallest a preview gets, as width : height (4:5).
    static let crop = CGSize(width: 4, height: 5)
    /// Pages a PDF preview shows before its "N more pages" row.
    static let pdfPages = 3

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

    /// A PDF of `pages` pages: how many show, and how many the row below them
    /// counts.
    static func pdfPages(_ pages: Int) -> (shown: Int, more: Int) {
        let shown = min(max(pages, 0), pdfPages)
        return (shown, max(pages, 0) - shown)
    }

    /// The row under a PDF's shown pages, when some are left.
    static func morePagesLabel(_ more: Int) -> String? {
        switch more {
        case ..<1: nil
        case 1: "1 more page"
        default: "\(more) more pages"
        }
    }
}

/// Lays its one subview out by `PreviewSize`: as wide as offered, as tall as
/// the rule says, the subview drawn column wide and `contentHeight` tall from
/// the top (clip the result to cut a cropped one off). `crops` false keeps
/// the whole content (a PDF page). Without a width offered it takes the
/// natural width.
struct PreviewFrame: Layout {
    let natural: CGSize
    var crops = true

    private func size(_ proposal: ProposedViewSize) -> PreviewSize {
        let column = proposal.width ?? natural.width
        let size = PreviewSize.of(column: column.isFinite ? column : natural.width, natural: natural)
        guard !crops else { return size }
        return PreviewSize(
            width: size.width, height: size.contentHeight, cropped: false, contentHeight: size.contentHeight)
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

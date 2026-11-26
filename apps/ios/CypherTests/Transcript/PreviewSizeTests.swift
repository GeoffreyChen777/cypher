import SwiftUI
import XCTest

@testable import Cypher

/// Previews size as the desktop sizes them
/// (crates/ui/src/markdown/preview_size.rs): both run the `sizing` section
/// of protocol/vectors/image-sources-v1.json and file-previews-v1.json.
final class PreviewSizeTests: XCTestCase {
    private static let files = ["image-sources-v1.json", "file-previews-v1.json"]

    private func sizing(_ file: String) throws -> [String: Any] {
        let root = try TestSupport.repoRoot()
        let data = try Data(contentsOf: root.appendingPathComponent("protocol/vectors/\(file)"))
        let all = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        return try XCTUnwrap(all["sizing"] as? [String: Any], file)
    }

    private func number(_ value: Any?, _ label: String) throws -> CGFloat {
        CGFloat(try XCTUnwrap(value as? NSNumber, label).doubleValue)
    }

    func testBothVectorFilesCarryTheSameRule() throws {
        let images = try sizing(Self.files[0]) as NSDictionary
        let files = try sizing(Self.files[1]) as NSDictionary
        XCTAssertEqual(images, files)
    }

    func testSharedCaps() throws {
        for file in Self.files {
            let caps = try XCTUnwrap(try sizing(file)["caps"] as? [String: Any], file)
            let crop = try XCTUnwrap(caps["crop"] as? [String: Any], file)
            XCTAssertEqual(try number(crop["width"], file), PreviewSize.crop.width, file)
            XCTAssertEqual(try number(crop["height"], file), PreviewSize.crop.height, file)
            let row = try XCTUnwrap(caps["pdfRow"] as? [String: Any], file)
            XCTAssertEqual((row["maxPages"] as? NSNumber)?.intValue, PreviewSize.pdfRowMax, file)
            XCTAssertEqual(try number(row["minPageWidth"], file), PreviewSize.pdfPageMin, file)
            XCTAssertEqual(try number(row["gap"], file), PreviewSize.pdfGap, file)
        }
    }

    func testSharedCases() throws {
        for file in Self.files {
            let cases = try XCTUnwrap(try sizing(file)["cases"] as? [[String: Any]], file)
            XCTAssertGreaterThanOrEqual(cases.count, 12, file)
            for v in cases {
                let name = "\(file): \(try XCTUnwrap(v["name"] as? String))"
                let natural = try XCTUnwrap(v["natural"] as? [String: Any], name)
                let expect = try XCTUnwrap(v["expect"] as? [String: Any], name)
                let got = PreviewSize.of(
                    column: try number(v["column"], name),
                    natural: CGSize(
                        width: try number(natural["width"], name), height: try number(natural["height"], name)))
                XCTAssertEqual(got.width, try number(expect["width"], name), accuracy: 0.01, name)
                XCTAssertEqual(got.height, try number(expect["height"], name), accuracy: 0.01, name)
                XCTAssertEqual(got.contentHeight, try number(expect["contentHeight"], name), accuracy: 0.01, name)
                XCTAssertEqual(got.cropped, try XCTUnwrap(expect["cropped"] as? Bool, name), name)
            }
        }
    }

    func testSharedPDFRows() throws {
        for file in Self.files {
            let cases = try XCTUnwrap(try sizing(file)["pdfRow"] as? [[String: Any]], file)
            XCTAssertGreaterThanOrEqual(cases.count, 8, file)
            for v in cases {
                let name = "\(file): \(try XCTUnwrap(v["name"] as? String))"
                let pages = try XCTUnwrap((v["pages"] as? NSNumber)?.intValue, name)
                let aspects = try XCTUnwrap(v["aspects"] as? [NSNumber], name).map { CGFloat($0.doubleValue) }
                let expect = try XCTUnwrap(v["expect"] as? [String: Any], name)
                let row = PreviewSize.pdfRow(column: try number(v["column"], name), aspects: aspects, pages: pages)
                XCTAssertEqual(row.shown, (expect["shown"] as? NSNumber)?.intValue, name)
                XCTAssertEqual(row.height, try number(expect["height"], name), accuracy: 0.01, name)
                let widths = try XCTUnwrap(expect["widths"] as? [NSNumber], name)
                XCTAssertEqual(widths.count, row.shown, name)
                for (page, width) in zip(row.pages, widths) {
                    XCTAssertEqual(page.width, CGFloat(width.doubleValue), accuracy: 0.01, name)
                    XCTAssertEqual(page.height, try number(expect["height"], name), accuracy: 0.01, name)
                }
                XCTAssertEqual(row.pages.first?.cropped ?? false, try XCTUnwrap(expect["cropped"] as? Bool, name), name)
                XCTAssertEqual(
                    row.pages.first?.contentHeight ?? 0, try number(expect["contentHeight"], name), accuracy: 0.01,
                    name)
                XCTAssertEqual(row.hidden, (expect["hidden"] as? NSNumber)?.intValue, name)
                XCTAssertEqual(row.badge, expect["badge"] as? String, name)
                if !(expect["badge"] is String) { XCTAssertTrue(expect["badge"] is NSNull, name) }
            }
        }
    }

    func testARowFillsTheColumnExactly() {
        let letter: CGFloat = 612 / 792
        for column: CGFloat in [300, 343, 402, 480, 600, 640, 900] {
            let row = PreviewSize.pdfRow(column: column, aspects: [letter, 1 / letter, letter, 2], pages: 9)
            guard row.shown >= 2 else { continue }
            let total = row.pages.reduce(0) { $0 + $1.width } + CGFloat(row.shown - 1) * PreviewSize.pdfGap
            XCTAssertEqual(total, column, accuracy: 0.01, "\(column)")
            XCTAssertTrue(row.pages.allSatisfy { $0.width >= PreviewSize.pdfPageMin - 0.01 }, "\(column)")
        }
    }

    /// Pages drawn for a column stay sharp in any narrower one, where fewer,
    /// wider pages may show.
    func testDrawnPagesStaySharpInAnyNarrowerColumn() {
        let aspects: [CGFloat] = [612 / 792, 792 / 612, 612 / 792, 1]
        let drawn = PreviewSize.pdfDrawWidths(column: 768, aspects: aspects, pages: 7)
        XCTAssertEqual(drawn.count, 4)
        XCTAssertEqual(drawn[0], 768)
        for column in stride(from: CGFloat(100), through: 768, by: 7) {
            let row = PreviewSize.pdfRow(column: column, aspects: aspects, pages: 7)
            for (page, width) in zip(row.pages, drawn) {
                XCTAssertLessThanOrEqual(page.width, width + 0.01, "\(column)")
            }
        }
        XCTAssertEqual(PreviewSize.pdfDrawWidths(column: 600, aspects: aspects, pages: 1), [600])
        XCTAssertTrue(PreviewSize.pdfDrawWidths(column: 600, aspects: [], pages: 3).isEmpty)
        // A non-positive aspect ends the known ones.
        XCTAssertEqual(PreviewSize.pdfRow(column: 640, aspects: [0.77, 0, 0.77], pages: 3).shown, 1)
    }

    /// An SVG draws column wide at the screen's scale, and a tall one only
    /// down to its 4:5 crop, from its top: here the top tenth is red and the
    /// rest blue, so the bitmap starts red and ends blue.
    @MainActor
    func testSVGsDrawForTheColumnDownToTheCrop() async throws {
        SVGRaster.deadline = .seconds(60)
        let svg = """
            <svg xmlns="http://www.w3.org/2000/svg" width="10" height="40">\
            <rect width="10" height="4" fill="#ff0000"/><rect y="4" width="10" height="36" fill="#0000ff"/></svg>
            """
        let natural = ImageDecoder.svgSize(svg)
        let key = try XCTUnwrap(SVGRaster.key(source: svg, natural: natural, column: 100, scale: 2))
        XCTAssertEqual(key.pixels, CGSize(width: 200, height: 250))
        XCTAssertEqual(key.contentHeight, 800)
        let drawn = await SVGRaster().draw(key)
        let image = try XCTUnwrap(drawn)
        let cg = try XCTUnwrap(image.cgImage)
        XCTAssertEqual(cg.width, 200)
        XCTAssertEqual(cg.height, 250)
        func pixel(_ x: Int, _ y: Int) -> (UInt8, UInt8, UInt8, UInt8) {
            var rgba = [UInt8](repeating: 0, count: 4)
            let context = CGContext(
                data: &rgba, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4,
                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            context.draw(cg, in: CGRect(x: -x, y: -(cg.height - 1 - y), width: cg.width, height: cg.height))
            return (rgba[0], rgba[1], rgba[2], rgba[3])
        }
        let top = pixel(100, 5)
        XCTAssertGreaterThan(top.0, 200)
        XCTAssertLessThan(top.2, 60)
        let bottom = pixel(100, 245)
        XCTAssertLessThan(bottom.0, 60)
        XCTAssertGreaterThan(bottom.2, 200)
    }

    /// The layout the views use offers the subview the whole content, top
    /// aligned, and reports the capped height.
    @MainActor
    func testFrameLaysOutByTheRule() throws {
        let natural = CGSize(width: 1170, height: 2532)
        let expected = PreviewSize.of(column: 343, natural: natural)
        let frame = PreviewFrame(natural: natural)
        let host = UIHostingController(
            rootView: frame { Color.red }.frame(width: 343))
        let fitted = host.sizeThatFits(in: CGSize(width: 343, height: 10_000))
        XCTAssertEqual(fitted.width, 343, accuracy: 0.5)
        XCTAssertEqual(fitted.height, expected.height, accuracy: 0.5)
    }

    /// A PDF's row is the column wide and the row's height; each page gets
    /// its frame side by side, and a page past the shown ones no room.
    @MainActor
    func testPDFRowLaysOutByTheRule() throws {
        let letter: CGFloat = 612 / 792
        let aspects = [letter, letter, letter, letter]
        let expected = PreviewSize.pdfRow(column: 343, aspects: aspects, pages: 5)
        XCTAssertEqual(expected.shown, 2)
        let host = UIHostingController(
            rootView: PDFRowLayout(aspects: aspects, pages: 5) {
                ForEach(0..<4, id: \.self) { _ in Color.red }
            }
            .frame(width: 343))
        let fitted = host.sizeThatFits(in: CGSize(width: 343, height: 10_000))
        XCTAssertEqual(fitted.width, 343, accuracy: 0.5)
        XCTAssertEqual(fitted.height, expected.height, accuracy: 0.5)
    }
}

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
            XCTAssertEqual((caps["pdfPages"] as? NSNumber)?.intValue, PreviewSize.pdfPages, file)
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

    func testSharedPDFPages() throws {
        for file in Self.files {
            let cases = try XCTUnwrap(try sizing(file)["pdf"] as? [[String: Any]], file)
            XCTAssertGreaterThanOrEqual(cases.count, 5, file)
            for v in cases {
                let name = "\(file): \(try XCTUnwrap(v["name"] as? String))"
                let pages = try XCTUnwrap((v["pages"] as? NSNumber)?.intValue, name)
                let expect = try XCTUnwrap(v["expect"] as? [String: Any], name)
                let (shown, more) = PreviewSize.pdfPages(pages)
                XCTAssertEqual(shown, (expect["shown"] as? NSNumber)?.intValue, name)
                XCTAssertEqual(more, (expect["more"] as? NSNumber)?.intValue, name)
                XCTAssertEqual(PreviewSize.morePagesLabel(more), expect["label"] as? String, name)
                if !(expect["label"] is String) { XCTAssertTrue(expect["label"] is NSNull, name) }
            }
        }
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
        let page = UIHostingController(
            rootView: PreviewFrame(natural: natural, crops: false) { Color.red }.frame(width: 343))
        XCTAssertEqual(
            page.sizeThatFits(in: CGSize(width: 343, height: 10_000)).height, expected.contentHeight, accuracy: 0.5)
    }
}

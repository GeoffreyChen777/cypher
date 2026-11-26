import ImageIO
import UIKit
import XCTest

@testable import Cypher

/// Image sources resolve as the desktop resolves them
/// (crates/ui/src/markdown/image_source.rs): both run
/// protocol/vectors/image-sources-v1.json.
final class ImageSourceTests: XCTestCase {
    private func vectors() throws -> [String: Any] {
        let root = try TestSupport.repoRoot()
        let data = try Data(contentsOf: root.appendingPathComponent("protocol/vectors/image-sources-v1.json"))
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    /// The table's shape for a classified source.
    private func json(_ kind: ImageSource.Kind) -> NSDictionary {
        switch kind {
        case .local(let candidates, let format):
            return [
                "kind": "local", "mime": format.mime,
                "candidates": candidates.map { ["root": $0.root, "path": $0.path] },
            ]
        case .outside(let path): return ["kind": "outside", "path": path]
        case .remote(let url): return ["kind": "remote", "url": url]
        case .data(let format, _): return ["kind": "data", "mime": format.mime]
        case .rejected(let reason): return ["kind": "rejected", "reason": reason.rawValue]
        case .notImage: return ["kind": "not_image"]
        }
    }

    func testSharedVectors() throws {
        let file = try vectors()
        XCTAssertEqual(file["version"] as? Int, 1)
        XCTAssertEqual((file["max_bytes"] as? NSNumber)?.uint64Value, ImageSource.maxBytes)
        let formats = try XCTUnwrap(file["formats"] as? [String: String])
        XCTAssertFalse(formats.isEmpty)
        for (ext, mime) in formats {
            XCTAssertEqual(ImageSource.Format(extension: ext)?.mime, mime, ext)
            XCTAssertEqual(ImageSource.Format(extension: ext.uppercased())?.mime, mime, ext)
        }
        let cases = try XCTUnwrap(file["cases"] as? [[String: Any]])
        XCTAssertGreaterThan(cases.count, 20)
        for v in cases {
            let name = try XCTUnwrap(v["name"] as? String)
            let context = ImageSource.Context(
                cwd: v["cwd"] as? String, roots: v["roots"] as? [String] ?? [], home: v["home"] as? String)
            let src = try XCTUnwrap(v["src"] as? String, name)
            let expected = try XCTUnwrap(v["expect"] as? NSDictionary, name)
            XCTAssertEqual(json(ImageSource.classify(src, context)), expected, "\(name): \(src)")
        }
        let lines = try XCTUnwrap(file["lines"] as? [[String: Any]])
        XCTAssertGreaterThan(lines.count, 8)
        for v in lines {
            let name = try XCTUnwrap(v["name"] as? String)
            let line = try XCTUnwrap(v["line"] as? String, name)
            XCTAssertEqual(ImageSource.imageLine(line), v["src"] as? String, "\(name): \(line)")
        }
    }

    func testBytesNameTheirFormat() {
        func sniff(_ bytes: [UInt8]) -> ImageSource.Format? { ImageSource.Format(sniffing: Data(bytes)) }
        XCTAssertEqual(sniff([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]), .png)
        XCTAssertEqual(sniff([0xFF, 0xD8, 0xFF, 0xE0]), .jpeg)
        XCTAssertEqual(sniff(Array("GIF89a....".utf8)), .gif)
        XCTAssertEqual(sniff(Array("RIFF\0\0\0\0WEBPVP8 ".utf8)), .webp)
        XCTAssertEqual(sniff(Array("<?xml version=\"1.0\"?>\n<svg xmlns=\"\"/>".utf8)), .svg)
        XCTAssertEqual(sniff(Array("  <svg/>".utf8)), .svg)
        XCTAssertNil(sniff(Array("BM\0\0".utf8)))
        XCTAssertNil(sniff(Array("%PDF-1.7".utf8)))
    }

    // MARK: Parser (iOS-only)

    func testAnImageInASentenceIsABlockAfterItsParagraph() {
        let blocks = MarkdownParser.parse("Here is the chart ![Revenue](shots/chart.png) for Q3.").map(\.block)
        XCTAssertEqual(blocks.count, 2)
        guard case .paragraph(let runs) = blocks[0] else { return XCTFail("\(blocks)") }
        // The sentence keeps the alt text (desktop parser.rs).
        XCTAssertEqual(runs.map(\.text).joined(), "Here is the chart Revenue for Q3.")
        // The sentence says the alt, so the image doesn't caption it again.
        XCTAssertEqual(blocks[1], .image(src: "shots/chart.png", alt: "Revenue", caption: false))
    }

    func testAParagraphOfOnlyImagesIsJustTheImages() {
        let blocks = MarkdownParser.parse("![a](a.png) ![b](https://x.test/b.webp)").map(\.block)
        XCTAssertEqual(blocks, [.image(src: "a.png", alt: "a"), .image(src: "https://x.test/b.webp", alt: "b")])
    }

    func testHeadingsAndTablesAreFollowedByTheirImages() {
        let heading = MarkdownParser.parse("# Logo ![x](a.png)").map(\.block)
        XCTAssertEqual(heading.count, 2)
        guard case .heading(_, let runs) = heading[0] else { return XCTFail() }
        XCTAssertEqual(runs.map(\.text).joined(), "Logo x")
        XCTAssertEqual(heading[1], .image(src: "a.png", alt: "x", caption: false))
        let table = MarkdownParser.parse("| a |\n|---|\n| ![p](p.gif) |").map(\.block)
        XCTAssertEqual(table.last, .image(src: "p.gif", alt: "p", caption: false))
    }

    func testAnImagePathParagraphIsAnImage() {
        XCTAssertEqual(
            MarkdownParser.parse("`out/shot.png`").map(\.block), [.image(src: "out/shot.png", alt: "out/shot.png")])
        guard case .paragraph = MarkdownParser.parse("/tmp/a b.png")[0].block else { return XCTFail() }
        guard case .paragraph = MarkdownParser.parse("`src/main.rs`")[0].block else { return XCTFail() }
        // In a list item too, after its marker.
        guard case .list(_, let items) = MarkdownParser.parse("- shots/a.png")[0].block else { return XCTFail() }
        XCTAssertEqual(items.first?.children, [.image(src: "shots/a.png", alt: "shots/a.png")])
    }

    /// Desktop parser/tests.rs: an image whose alt the text around it keeps
    /// has no caption; one that was all its paragraph said keeps it.
    func testTheAltIsSaidOnce() {
        let said = MarkdownParser.parse("The icon ![app icon](a.png) is unchanged.").map(\.block)
        XCTAssertEqual(said.count, 2)
        XCTAssertEqual(said[1], .image(src: "a.png", alt: "app icon", caption: false))
        XCTAssertEqual(
            MarkdownParser.parse("![App icon](a.png)").map(\.block),
            [.image(src: "a.png", alt: "App icon", caption: true)])
        // Quotes and list items: the same rule inside them.
        guard case .blockquote(let quoted) = MarkdownParser.parse("> ![q](q.jpg)")[0].block else { return XCTFail() }
        XCTAssertEqual(quoted, [.image(src: "q.jpg", alt: "q", caption: true)])
        guard case .list(_, let items) = MarkdownParser.parse("- shot: ![a](a.png)\n- b")[0].block else {
            return XCTFail()
        }
        XCTAssertEqual(items[0].children.last, .image(src: "a.png", alt: "a", caption: false))
        // The comment text still reads the alt.
        XCTAssertEqual(MDBlock.image(src: "a.png", alt: "app icon", caption: false).commentText, "app icon")
    }

    func testNonImageSourcesStayAltText() {
        let blocks = MarkdownParser.parse("See ![the spec](docs/spec.pdf).").map(\.block)
        XCTAssertEqual(blocks.count, 1)
        guard case .paragraph(let runs) = blocks[0] else { return XCTFail() }
        XCTAssertEqual(runs.map(\.text).joined(), "See the spec.")
    }

    func testStreamingAfterAnImageKeepsEveryBlockOnce() {
        let parser = IncrementalMarkdownParser()
        var text = "One ![a](a.png) two.\n\nNext"
        parser.setText(text)
        for word in [" para", "graph.\n\n", "Third", " one."] {
            text += word
            parser.setText(text)
            XCTAssertEqual(parser.blocks.map(\.block), MarkdownParser.parse(text).map(\.block), text)
        }
    }

    func testImagesAreNotProse() {
        XCTAssertFalse(TranscriptTextStyle.isProse(.image(src: "a.png", alt: "")))
    }

    // MARK: Resolution on iOS

    func testTheHostHomeIsReadOffATildeFolder() {
        let context = WorkspaceBrowserSession.imageContext(cwd: "~/code/app", roots: ["/Users/me/code/app"])
        XCTAssertEqual(context.home, "/Users/me")
        XCTAssertEqual(context.roots, ["~/code/app", "/Users/me/code/app"])
        XCTAssertEqual(
            ImageSource.classify("~/code/app/shots/a.png", context),
            .local(candidates: [.init(root: "/Users/me/code/app", path: "shots/a.png")], format: .png))
        XCTAssertNil(WorkspaceBrowserSession.imageContext(cwd: "/srv/app", roots: ["/srv/app"]).home)
    }

    func testToolCallsThatReadImagesHaveThumbnails() {
        let read = RenderToolCall(tag: "readFile", fields: ["path": "/Users/me/app/shot.PNG"])
        XCTAssertEqual(read.imagePath, "/Users/me/app/shot.PNG")
        XCTAssertNil(RenderToolCall(tag: "readFile", fields: ["path": "src/a.rs"]).imagePath)
        XCTAssertNil(RenderToolCall(tag: "exec", fields: ["command": "open a.png"]).imagePath)
    }

    func testChipsNamingImagesAreTold() {
        XCTAssertTrue(FileReferenceChip.namesImage("assets/logo.svg"))
        XCTAssertTrue(FileReferenceChip.namesImage("shots/a.png:3"))
        XCTAssertFalse(FileReferenceChip.namesImage("src/a.rs:12"))
    }

    // MARK: Decoding

    private func png(width: Int, height: Int) -> Data {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        return UIGraphicsImageRenderer(size: CGSize(width: width, height: height), format: format).pngData { _ in
            UIColor.systemTeal.setFill()
            UIRectFill(CGRect(x: 0, y: 0, width: width, height: height))
        }
    }

    func testDecodingDownsamplesButKeepsTheNaturalSize() throws {
        let image = try XCTUnwrap(
            ImageDecoder.decode(png(width: 800, height: 400), hint: nil, maxPixel: 200, fileName: "a"))
        XCTAssertEqual(image.pixelSize, CGSize(width: 800, height: 400))
        XCTAssertEqual(image.format, .png)
        let bitmap = try XCTUnwrap(image.bitmap)
        XCTAssertEqual(bitmap.size.width * bitmap.scale, 200)
        // Never past its own size.
        let small = try XCTUnwrap(
            ImageDecoder.decode(png(width: 20, height: 10), hint: nil, maxPixel: 200, fileName: "b"))
        XCTAssertEqual(small.bitmap.map { $0.size.width * $0.scale }, 20)
        XCTAssertNil(ImageDecoder.decode(Data("not an image".utf8), hint: .png, maxPixel: 200, fileName: "c"))
    }

    func testAnimatedGifsKeepTheirFrames() throws {
        let data = NSMutableData()
        let destination = try XCTUnwrap(
            CGImageDestinationCreateWithData(data, "com.compuserve.gif" as CFString, 3, nil))
        for color in [UIColor.red, .green, .blue] {
            let format = UIGraphicsImageRendererFormat()
            format.scale = 1
            let frame = UIGraphicsImageRenderer(size: CGSize(width: 8, height: 8), format: format).image { _ in
                color.setFill()
                UIRectFill(CGRect(x: 0, y: 0, width: 8, height: 8))
            }
            let properties = [kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFDelayTime: 0.2]] as CFDictionary
            CGImageDestinationAddImage(destination, try XCTUnwrap(frame.cgImage), properties)
        }
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        let image = try XCTUnwrap(ImageDecoder.decode(data as Data, hint: nil, maxPixel: 200, fileName: "a.gif"))
        XCTAssertEqual(image.format, .gif)
        XCTAssertTrue(image.animated)
        XCTAssertEqual(image.bitmap?.images?.count, 3)
        XCTAssertEqual(image.bitmap?.duration ?? 0, 0.6, accuracy: 0.05)
    }

    func testSVGsKeepTheirSourceAndSize() throws {
        let svg = #"<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120"><rect/></svg>"#
        let image = try XCTUnwrap(ImageDecoder.decode(Data(svg.utf8), hint: nil, maxPixel: 200, fileName: "a.svg"))
        guard case .svg(let text) = image.content else { return XCTFail() }
        XCTAssertEqual(text, svg)
        XCTAssertEqual(image.pixelSize, CGSize(width: 240, height: 120))
        XCTAssertEqual(ImageDecoder.svgSize(#"<svg viewBox="0 0 30 10">"#), CGSize(width: 30, height: 10))
    }

    /// The block sizes by the image's pixel size (PreviewSizeTests covers the
    /// rule), else by the decoded bitmap.
    func testTheBlockSizesByThePixelSize() {
        let bitmap = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 2)).image { _ in }
        XCTAssertEqual(
            InlineImageBlock.natural(CGSize(width: 1600, height: 1200), bitmap), CGSize(width: 1600, height: 1200))
        XCTAssertEqual(InlineImageBlock.natural(.zero, bitmap), CGSize(width: 4, height: 2))
    }

    // MARK: Loading

    @MainActor
    func testDataURIsLoadAndHttpIsRefused() async throws {
        let loader = ImageLoader()
        let uri = "data:image/png;base64," + png(width: 4, height: 2).base64EncodedString()
        let loaded = try await loader.load(uri, files: nil, size: .block).get()
        XCTAssertEqual(loaded.pixelSize, CGSize(width: 4, height: 2))
        XCTAssertEqual(loaded.fileName, "image.png")
        XCTAssertNotNil(loader.cached(uri, files: nil, size: .block))
        let insecure = await loader.load("http://x.test/a.png", files: nil, size: .block)
        XCTAssertEqual(insecure.failure, .insecure)
        let local = await loader.load("shots/a.png", files: nil, size: .block)
        XCTAssertEqual(local.failure, .noFiles)
        let text = await loader.load("data:image/svg+xml,<svg/>", files: nil, size: .block)
        XCTAssertEqual(text.failure, .unsupported)
    }

    @MainActor
    func testLocalImagesReadTheirCandidatesInOrderAndStayInTheirChat() async throws {
        let loader = ImageLoader()
        final class Reads { var list: [ImageSource.Candidate] = [] }
        let reads = Reads()
        let bytes = png(width: 6, height: 3)
        let files = TranscriptImageFiles(
            scope: "chat-a",
            context: { ImageSource.Context(cwd: "/w/app", roots: ["/w/app", "/w/lib"]) },
            read: { candidate in
                reads.list.append(candidate)
                guard candidate.root == "/w/lib" else { throw ImageLoadError.notFound }
                return bytes
            })
        let image = try await loader.load("logo.png", files: files, size: .thumbnail).get()
        XCTAssertEqual(image.pixelSize, CGSize(width: 6, height: 3))
        XCTAssertEqual(reads.list, [.init(root: "/w/app", path: "logo.png"), .init(root: "/w/lib", path: "logo.png")])
        // Read once: another size decodes the same bytes again.
        _ = try await loader.load("logo.png", files: files, size: .block).get()
        XCTAssertEqual(reads.list.count, 2)
        // Another chat writing the same path is another image.
        XCTAssertNil(
            loader.cached(
                "logo.png",
                files: TranscriptImageFiles(
                    scope: "chat-b", context: { ImageSource.Context() }, read: { _ in Data() }), size: .thumbnail))
        let outside = await loader.load("/etc/a.png", files: files, size: .block)
        XCTAssertEqual(outside.failure, .outside)
        let tooLarge = TranscriptImageFiles(
            scope: "chat-c", context: { ImageSource.Context(cwd: "/w", roots: ["/w"]) },
            read: { _ in throw WorkspaceBrowserError.tooLarge })
        let big = await loader.load("a.png", files: tooLarge, size: .block)
        XCTAssertEqual(big.failure, .tooLarge)
    }
}

extension Result {
    fileprivate var failure: Failure? {
        if case .failure(let error) = self { return error }
        return nil
    }
}

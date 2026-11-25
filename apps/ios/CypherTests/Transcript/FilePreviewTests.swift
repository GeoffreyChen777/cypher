import UIKit
import XCTest

@testable import Cypher

/// File previews follow the desktop's rules
/// (crates/ui/src/markdown/file_preview.rs): both run
/// protocol/vectors/file-previews-v1.json.
final class FilePreviewTests: XCTestCase {
    private func vectors() throws -> [String: Any] {
        let root = try TestSupport.repoRoot()
        let data = try Data(contentsOf: root.appendingPathComponent("protocol/vectors/file-previews-v1.json"))
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func testSharedCaps() throws {
        let file = try vectors()
        XCTAssertEqual(file["version"] as? Int, 1)
        let caps = try XCTUnwrap(file["caps"] as? [String: NSNumber])
        XCTAssertEqual(caps["tableReadBytes"]?.uint64Value, FilePreview.tableReadBytes)
        XCTAssertEqual(caps["tableRows"]?.intValue, FilePreview.tableRows)
        XCTAssertEqual(caps["columnChars"]?.intValue, FilePreview.columnChars)
        XCTAssertEqual(caps["pdfMaxBytes"]?.uint64Value, FilePreview.pdfMaxBytes)
        XCTAssertEqual(caps["videoMaxBytes"]?.uint64Value, FilePreview.videoMaxBytes)
        XCTAssertEqual(caps["htmlMaxBytes"]?.uint64Value, FilePreview.htmlMaxBytes)
        XCTAssertEqual(caps.count, 6)
    }

    func testSharedKinds() throws {
        let cases = try XCTUnwrap(try vectors()["kinds"] as? [[String: Any]])
        XCTAssertGreaterThan(cases.count, 20)
        for v in cases {
            let name = try XCTUnwrap(v["name"] as? String)
            let path = try XCTUnwrap(v["path"] as? String, name)
            let expected = try XCTUnwrap((v["expect"] as? [String: Any])?["kind"] as? String, name)
            XCTAssertEqual(FilePreview.kind(path)?.rawValue ?? "none", expected, "\(name): \(path)")
        }
    }

    func testSharedTables() throws {
        let cases = try XCTUnwrap(try vectors()["table"] as? [[String: Any]])
        XCTAssertGreaterThan(cases.count, 20)
        for v in cases {
            let name = try XCTUnwrap(v["name"] as? String)
            let text = try XCTUnwrap(v["text"] as? String, name)
            let delimiter = try XCTUnwrap(v["delimiter"] as? String, name)
            XCTAssertEqual(delimiter.unicodeScalars.count, 1, name)
            let complete = v["complete"] as? Bool ?? true
            let table = FilePreview.parseTable(
                text, delimiter: try XCTUnwrap(delimiter.unicodeScalars.first), complete: complete)
            var got: [String: Any] = [
                "header": table.columns != nil, "rows": table.rows, "totalRows": table.totalRows,
                "truncated": table.truncated,
            ]
            if let columns = table.columns { got["columns"] = columns }
            let expected = try XCTUnwrap(v["expect"] as? NSDictionary, name)
            XCTAssertEqual(got as NSDictionary, expected, name)
        }
    }

    func testSharedLines() throws {
        let cases = try XCTUnwrap(try vectors()["lines"] as? [[String: Any]])
        XCTAssertGreaterThan(cases.count, 6)
        for v in cases {
            let name = try XCTUnwrap(v["name"] as? String)
            let line = try XCTUnwrap(v["line"] as? String, name)
            XCTAssertEqual(FilePreview.fileLine(line), v["src"] as? String, "\(name): \(line)")
        }
    }

    func testCountLabels() {
        func table(_ total: Int, _ truncated: Bool) -> FilePreview.Table {
            FilePreview.Table(columns: nil, rows: [], totalRows: total, truncated: truncated)
        }
        XCTAssertEqual(table(1, false).countLabel, "1 row")
        XCTAssertEqual(table(12, false).countLabel, "12 rows")
        XCTAssertEqual(table(20, true).countLabel, "20+ rows")
        XCTAssertEqual(table(1, true).countLabel, "1+ rows")
    }

    func testKindsCarryTheirCapsAndDelimiters() {
        XCTAssertEqual(FilePreview.Kind.csv.delimiter, ",")
        XCTAssertEqual(FilePreview.Kind.tsv.delimiter, "\t")
        XCTAssertNil(FilePreview.Kind.pdf.delimiter)
        XCTAssertEqual(FilePreview.Kind.csv.maxBytes, FilePreview.tableReadBytes)
        XCTAssertEqual(FilePreview.Kind.svg.maxBytes, FilePreview.htmlMaxBytes)
        XCTAssertEqual(FilePreview.Kind.video.maxBytes, FilePreview.videoMaxBytes)
    }

    func testNumbers() {
        for yes in ["0", "-1.5", ".5", "3.", "+2e10", "3E-2", "12%", "-0.5%"] {
            XCTAssertTrue(FilePreview.isNumeric(yes), yes)
        }
        for no in ["", "-", ".", "e5", "1e", "1,000", "1.2.3", "abc", "%", "2024-01-01"] {
            XCTAssertFalse(FilePreview.isNumeric(no), no)
        }
    }

    func testNamesAndSizes() {
        XCTAssertEqual(FilePreview.fileName("out/results.csv:3"), "results.csv")
        XCTAssertEqual(FilePreview.fileName("file:///tmp/my%20clip.mov"), "my clip.mov")
        XCTAssertEqual(FilePreview.humanSize(512), "512 B")
        XCTAssertEqual(FilePreview.humanSize(12 * 1024), "12 KB")
        XCTAssertEqual(FilePreview.humanSize(3 * 1024 * 1024 + 400 * 1024), "3.4 MB")
        XCTAssertEqual(LoadedFilePreview.clock(4.2), "0:04")
        XCTAssertEqual(LoadedFilePreview.clock(3723), "1:02:03")
    }

    // MARK: Parser (desktop parser/tests.rs a_previewable_file_alone_is_a_preview_block)

    func testAPreviewableFileAloneIsAPreviewBlock() {
        XCTAssertEqual(
            MarkdownParser.parse("`out/results.csv`\n").map(\.block), [.filePreview(src: "out/results.csv", kind: .csv)]
        )
        XCTAssertEqual(
            MarkdownParser.parse("report.PDF\n\n<clips/demo.mov>\n\n`site/index.html`\n").map(\.block),
            [
                .filePreview(src: "report.PDF", kind: .pdf), .filePreview(src: "clips/demo.mov", kind: .video),
                .filePreview(src: "site/index.html", kind: .html),
            ])
        // An svg is a picture; a file in a sentence and a URL stay text.
        XCTAssertEqual(
            MarkdownParser.parse("`out/logo.svg`\n").map(\.block), [.image(src: "out/logo.svg", alt: "out/logo.svg")])
        for text in ["See `out/results.csv` for the rows.\n", "https://x.dev/a.pdf\n"] {
            guard case .paragraph = MarkdownParser.parse(text).first?.block, MarkdownParser.parse(text).count == 1
            else { return XCTFail(text) }
        }
        // In a list item, after its marker.
        guard case .list(_, let items) = MarkdownParser.parse("- data/x.tsv")[0].block else { return XCTFail() }
        XCTAssertEqual(items.first?.children, [.filePreview(src: "data/x.tsv", kind: .tsv)])
        XCTAssertFalse(TranscriptTextStyle.isProse(.filePreview(src: "a.csv", kind: .csv)))
    }

    // MARK: Loading

    /// A host with these files under `/w`, counting reads.
    @MainActor
    private final class Host {
        var files: [String: Data] = [:]
        var reads: [(path: String, limit: UInt64, head: Bool)] = []

        func images(scope: String = UUID().uuidString) -> TranscriptImageFiles {
            TranscriptImageFiles(
                scope: scope, context: { ImageSource.Context(cwd: "/w", roots: ["/w"]) },
                read: { _ in throw ImageLoadError.notFound },
                preview: { [self] candidate, limit, head in
                    reads.append((candidate.path, limit, head))
                    guard let data = files[candidate.path] else {
                        throw RelayError.rpc("No such file or directory")
                    }
                    let size = UInt64(data.count)
                    if head { return (data.prefix(Int(min(limit, size))), size) }
                    guard size <= limit else { throw WorkspaceBrowserError.tooLarge }
                    return (data, size)
                })
        }
    }

    @MainActor
    func testATableReadsOnlyItsHead() async throws {
        let host = Host()
        let big = "a,b\n" + (1...40_000).map { "\($0),x" }.joined(separator: "\n") + "\n"
        host.files["big.csv"] = Data(big.utf8)
        host.files["small.tsv"] = Data("name\tn\nada\t1\nbob\t2\n".utf8)
        let loader = FilePreviewLoader()
        let files = host.images()
        let loaded = try await loader.load("big.csv", kind: .csv, files: files).get()
        guard case .table(let table) = loaded.body else { return XCTFail("\(loaded.body)") }
        XCTAssertTrue(table.truncated)
        XCTAssertEqual(table.rows.count, FilePreview.tableRows)
        XCTAssertEqual(table.columns, ["a", "b"])
        XCTAssertTrue(loaded.detail.hasSuffix("+ rows"), loaded.detail)
        XCTAssertEqual(host.reads.last?.limit, FilePreview.tableReadBytes)
        XCTAssertEqual(host.reads.last?.head, true)
        let small = try await loader.load("small.tsv:2", kind: .tsv, files: files).get()
        XCTAssertEqual(small.detail, "2 rows")
        XCTAssertEqual(small.name, "small.tsv")
        // Loaded once: a row scrolled back in doesn't read again.
        let count = host.reads.count
        _ = try await loader.load("small.tsv:2", kind: .tsv, files: files).get()
        XCTAssertEqual(host.reads.count, count)
        XCTAssertNotNil(loader.cached("small.tsv:2", files: files))
    }

    @MainActor
    func testPagesCarryTheirTitleAndSource() async throws {
        let host = Host()
        host.files["site/index.html"] = Data("<html><title>Q3 report</title><body>hi</body></html>".utf8)
        let loaded = try await FilePreviewLoader().load("site/index.html", kind: .html, files: host.images()).get()
        guard case .page(let title, let code) = loaded.body else { return XCTFail("\(loaded.body)") }
        XCTAssertEqual(title, "Q3 report")
        XCTAssertTrue(code.contains("<body>hi"))
        XCTAssertEqual(host.reads.last?.limit, FilePreview.htmlMaxBytes)
        XCTAssertEqual(host.reads.last?.head, false)
    }

    @MainActor
    func testAPDFShowsItsFirstPage() async throws {
        let host = Host()
        host.files["paper.pdf"] = UIGraphicsPDFRenderer(bounds: CGRect(x: 0, y: 0, width: 200, height: 100)).pdfData {
            $0.beginPage()
            $0.beginPage()
        }
        let loaded = try await FilePreviewLoader().load("paper.pdf", kind: .pdf, files: host.images()).get()
        guard case .thumb(let image) = loaded.body else { return XCTFail("\(loaded.body)") }
        XCTAssertEqual(image.size.width / image.size.height, 2, accuracy: 0.01)
        XCTAssertEqual(loaded.pages, 2)
        XCTAssertTrue(loaded.detail.hasPrefix("2 pages · "), loaded.detail)
        let file = try XCTUnwrap(loaded.file)
        XCTAssertEqual(file.lastPathComponent, "paper.pdf")
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path))
        host.files["broken.pdf"] = Data("not a pdf".utf8)
        let broken = await FilePreviewLoader().load("broken.pdf", kind: .pdf, files: host.images())
        XCTAssertEqual(broken.failure, .unreadable)
    }

    @MainActor
    func testAVideoReadsItsSizeFirst() async throws {
        let host = Host()
        host.files["clip.mp4"] = Data(count: Int(FilePreviewLoader.videoFrameBytes) + 1)
        let loader = FilePreviewLoader()
        let files = host.images()
        // Past the frame cap: only its size was read, and it shows as plain.
        let plain = try await loader.load("clip.mp4", kind: .video, files: files).get()
        guard case .plain = plain.body else { return XCTFail("\(plain.body)") }
        XCTAssertEqual(host.reads.map(\.limit), [1])
        XCTAssertNil(plain.file)
        XCTAssertEqual(plain.detail, FilePreview.humanSize(FilePreviewLoader.videoFrameBytes + 1))
        // Tapped, the whole file comes (here not a playable one: no frame).
        let whole = try await loader.load("clip.mp4", kind: .video, files: files, whole: true).get()
        XCTAssertEqual(host.reads.last?.limit, FilePreview.videoMaxBytes)
        XCTAssertNotNil(whole.file)
    }

    /// The demo checkout's files (bundled clip, drawn PDF) preview for real.
    @MainActor
    func testTheDemoFilesPreview() async throws {
        let clip = try XCTUnwrap(WorkspaceBrowserSession.demoBytes("notes/demo.mp4"))
        XCTAssertLessThan(clip.count, 64 * 1024)
        let video = try await FilePreviewLoader.preview(
            kind: .video, name: "demo.mp4", data: clip, size: UInt64(clip.count)
        ).get()
        guard case .thumb(let frame) = video.body else { return XCTFail("\(video.body)") }
        XCTAssertEqual(frame.size.width / frame.size.height, 16 / 9, accuracy: 0.02)
        XCTAssertEqual(try XCTUnwrap(video.duration), 3, accuracy: 0.2)
        XCTAssertTrue(video.detail.hasPrefix("0:03 · "), video.detail)
        let pdf = try XCTUnwrap(WorkspaceBrowserSession.demoBytes("notes/report.pdf"))
        let report = try await FilePreviewLoader.preview(
            kind: .pdf, name: "report.pdf", data: pdf, size: UInt64(pdf.count)
        ).get()
        XCTAssertEqual(report.pages, 2)
        let csv = FilePreview.parseTable(DemoDataset.latencyCSV, delimiter: ",", complete: true)
        XCTAssertEqual(csv.countLabel, "24 rows")
        XCTAssertEqual(csv.rows.count, 20)
        let tsv = FilePreview.parseTable(DemoDataset.regionsTSV, delimiter: "\t", complete: true)
        XCTAssertEqual(tsv.columns, ["region", "median_ms", "note"])
        XCTAssertEqual(tsv.rows[2], ["ap-south", "112", "two hops, via Singapore"])
    }

    @MainActor
    func testWhatCantShowSaysWhy() async throws {
        let host = Host()
        let files = host.images()
        let loader = FilePreviewLoader()
        let missing = await loader.load("gone.csv", kind: .csv, files: files)
        XCTAssertEqual(missing.failure, .notFound)
        XCTAssertEqual(missing.failure?.retryable, true)
        let outside = await loader.load("/etc/a.csv", kind: .csv, files: files)
        XCTAssertEqual(outside.failure, .outside)
        let escape = await loader.load("../a.pdf", kind: .pdf, files: files)
        XCTAssertEqual(escape.failure, .outside)
        host.files["big.html"] = Data(count: Int(FilePreview.htmlMaxBytes) + 1)
        let big = await loader.load("big.html", kind: .html, files: files)
        XCTAssertEqual(big.failure, .tooLarge(.html))
        XCTAssertEqual(big.failure?.localizedDescription, "Too large (over 5 MB)")
        let none = await loader.load("a.csv", kind: .csv, files: nil)
        XCTAssertEqual(none.failure, .noFiles)
        // A chat whose files only read images has no previews either.
        let imagesOnly = TranscriptImageFiles(
            scope: "x", context: { ImageSource.Context(cwd: "/w", roots: ["/w"]) }, read: { _ in Data() })
        let old = await loader.load("a.csv", kind: .csv, files: imagesOnly)
        XCTAssertEqual(old.failure, .noFiles)
    }
}

extension Result {
    fileprivate var failure: Failure? {
        if case .failure(let error) = self { return error }
        return nil
    }
}

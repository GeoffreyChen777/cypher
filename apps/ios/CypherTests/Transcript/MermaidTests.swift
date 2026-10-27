import XCTest
import UIKit
@testable import Cypher

/// ```mermaid fences: the parser knows when a fence has closed (a streaming
/// one stays code), and the bundled renderer draws diagrams in a real
/// WKWebView or explains why it cannot.
@MainActor
final class MermaidTests: XCTestCase {
    private func closedFlags(_ source: String) -> [Bool] {
        func flags(_ block: MDBlock) -> [Bool] {
            switch block {
            case .codeBlock(_, _, let closed): return [closed]
            case .blockquote(let children): return children.flatMap(flags)
            case .list(_, let items): return items.flatMap { $0.children.flatMap(flags) }
            default: return []
            }
        }
        return MarkdownParser.parse(source).flatMap { flags($0.block) }
    }

    // MARK: Parsing (desktop parser/tests.rs `code_blocks_know_whether_their_fence_closed`)

    func testCodeBlocksKnowWhetherTheirFenceClosed() {
        XCTAssertEqual(closedFlags("```mermaid\ngraph TD\n  A --> B\n```\n"), [true])
        XCTAssertEqual(closedFlags("```mermaid\ngraph TD\n  A --> B\n```"), [true])
        XCTAssertEqual(closedFlags("```mermaid\ngraph TD\n  A --> B\n"), [false])
        XCTAssertEqual(closedFlags("```mermaid\ngraph TD\n  A --> B\n``"), [false])
        XCTAssertEqual(closedFlags("```mermaid"), [false])
        // A shorter run inside a longer fence is content, not a closer.
        XCTAssertEqual(closedFlags("````md\n```\nstill code\n```\n"), [false])
        XCTAssertEqual(closedFlags("````md\n```\ncode\n````\n"), [true])
        XCTAssertEqual(closedFlags("~~~\nx\n~~~\n"), [true])
        XCTAssertEqual(closedFlags("    indented\n    code\n"), [true])
        XCTAssertEqual(closedFlags("intro\n\n```mermaid\ngraph TD\n```\n\nafter\n"), [true])
        // Container markers ride the block's lines.
        XCTAssertEqual(closedFlags("- ```mermaid\n  graph TD\n  ```\n"), [true])
        XCTAssertEqual(closedFlags("> ```mermaid\n> graph TD\n> ```\n"), [true])
        XCTAssertEqual(closedFlags("> ```mermaid\n> graph TD\n"), [false])
        XCTAssertEqual(closedFlags("1. item\n\n   ```mermaid\n   graph TD\n   ```\n"), [true])
    }

    func testAStreamedFenceClosesWithItsLastLine() {
        let source = "intro\n\n```mermaid\ngraph TD\n  A --> B\n```\n"
        let parser = IncrementalMarkdownParser()
        let closer = source.range(of: "```", options: .backwards)!.lowerBound
        let code = "graph TD\n  A --> B"
        parser.setText(String(source[..<closer]))
        XCTAssertEqual(parser.blocks.last?.block, .codeBlock(language: "mermaid", code: code, closed: false))
        parser.setText(source)
        XCTAssertEqual(parser.blocks.last?.block, .codeBlock(language: "mermaid", code: code, closed: true))
    }

    func testFenceTags() {
        XCTAssertTrue(isMermaidFence("mermaid"))
        XCTAssertTrue(isMermaidFence("Mermaid"))
        XCTAssertTrue(isMermaidFence("mmd"))
        XCTAssertFalse(isMermaidFence("markdown"))
        XCTAssertFalse(isMermaidFence(nil))
    }

    func testPalettesFollowTheAppearance() {
        let dark = MermaidPalette.of(dark: true)
        let light = MermaidPalette.of(dark: false)
        XCTAssertNotEqual(dark, light)
        let hex = /^#[0-9a-f]{6}$/
        for palette in [dark, light] {
            XCTAssertEqual(palette.roles.count, 13)
            XCTAssertEqual(palette.series.count, 6)
            XCTAssertTrue((Array(palette.roles.values) + palette.series).allSatisfy { $0.wholeMatch(of: hex) != nil })
        }
        XCTAssertEqual(light.roles["canvas"], "#f6f6f6")
    }

    // MARK: Rendering

    private var savedDeadline = MermaidRenderer.renderDeadline

    override func setUp() async throws {
        savedDeadline = MermaidRenderer.renderDeadline
        // A cold WebContent launch has taken 23s on a loaded CI runner.
        MermaidRenderer.renderDeadline = .seconds(60)
    }

    override func tearDown() async throws {
        MermaidRenderer.renderDeadline = savedDeadline
    }

    private func render(_ source: String, dark: Bool = true) async -> MermaidOutcome {
        await MermaidRenderer.shared.render(.init(source: source, palette: .of(dark: dark), scale: 2))
    }

    /// Distinct opaque colors in the raster: a blank or single-color image
    /// has one or two.
    private func inkColors(_ image: UIImage) -> Int {
        guard let cg = image.cgImage, let data = cg.dataProvider?.data,
            let bytes = CFDataGetBytePtr(data)
        else { return 0 }
        var seen = Set<UInt32>()
        let step = max(1, (cg.width * cg.height) / 20_000)
        var ix = 0
        while ix < cg.width * cg.height, seen.count < 64 {
            let offset = (ix / cg.width) * cg.bytesPerRow + (ix % cg.width) * cg.bitsPerPixel / 8
            seen.insert(
                UInt32(bytes[offset]) << 24 | UInt32(bytes[offset + 1]) << 16
                    | UInt32(bytes[offset + 2]) << 8 | UInt32(bytes[offset + 3]))
            ix += step
        }
        return seen.count
    }

    func testCommonDiagramsDraw() async throws {
        for source in [
            "flowchart TD\n  A[Start] -->|go| B{Ready?}\n  B -->|yes| C[(Store)]",
            "sequenceDiagram\n  UI->>Engine: send\n  Engine-->>UI: reply",
            "stateDiagram-v2\n  [*] --> Idle\n  Idle --> Busy: run",
            "classDiagram\n  class Row {\n    +id: String\n  }",
            "erDiagram\n  CHAT ||--o{ MESSAGE : contains",
            "pie title Tokens\n  \"In\" : 40\n  \"Out\" : 60",
            "mindmap\n  root((Cypher))\n    UI\n    Engine",
            "flowchart LR\n  用户 --> 引擎[引擎处理请求]",
        ] {
            let outcome = await render(source)
            guard case .image(let image, let size) = outcome else {
                return XCTFail("\(source): \(outcome)")
            }
            XCTAssertGreaterThan(size.width, 10, source)
            XCTAssertGreaterThan(size.height, 10, source)
            XCTAssertEqual(image.size.width, size.width, accuracy: 1, source)
            XCTAssertEqual(image.scale, 2, accuracy: 0.01, source)
            XCTAssertGreaterThan(inkColors(image), 4, "\(source) drew nothing")
        }
    }

    func testBadSourceExplainsItself() async {
        guard case .failed(let error) = await render("flowchart TD\n  A --> ") else {
            return XCTFail("a broken diagram must not draw")
        }
        XCTAssertFalse(error.isEmpty)
        XCTAssertFalse(error.contains("\n"))
        guard case .failed(let unknown) = await render("hello world") else {
            return XCTFail("prose is not a diagram")
        }
        XCTAssertFalse(unknown.isEmpty)
        // The renderer recovers for the next diagram.
        guard case .image = await render("flowchart LR\n  A --> B") else {
            return XCTFail("a good diagram after a bad one draws")
        }
    }

    func testDiagramsAskingForElkFallBackToDagre() async {
        let outcome = await render("---\nconfig:\n  layout: elk\n---\nflowchart TD\n  A --> B")
        guard case .image = outcome else { return XCTFail("\(outcome)") }
    }

    func testMarkupInLabelsStaysText() async {
        // `securityLevel: strict`: a label cannot script or load anything.
        let outcome = await render(
            "flowchart TD\n  A[\"<img src=x onerror=alert(1)>\"] --> B[\"<script>alert(2)</script>\"]")
        guard case .image = outcome else { return XCTFail("\(outcome)") }
    }

    func testRepeatedRendersComeFromTheCache() async {
        let key = MermaidRenderer.Key(source: "flowchart LR\n  Cache --> Hit", palette: .of(dark: false), scale: 2)
        _ = await MermaidRenderer.shared.render(key)
        XCTAssertTrue(MermaidRenderer.shared.isSettled(key))
        guard case .image = MermaidRenderer.shared.cached(key) else { return XCTFail("cached") }
        // Another palette stands in until its own drawing lands.
        var dark = key
        dark.palette = .of(dark: true)
        XCTAssertFalse(MermaidRenderer.shared.isSettled(dark))
        guard case .image = MermaidRenderer.shared.cached(dark) else { return XCTFail("stand-in") }
    }

    func testSourceSwitchIsSharedBySource() {
        let toggles = MermaidSourceToggles.shared
        let source = "flowchart TD\n  Toggle --> Source"
        XCTAssertFalse(toggles.showsSource(source))
        toggles.set(source, showsSource: true)
        XCTAssertTrue(toggles.showsSource(source))
        toggles.set(source, showsSource: false)
        XCTAssertFalse(toggles.showsSource(source))
    }
}

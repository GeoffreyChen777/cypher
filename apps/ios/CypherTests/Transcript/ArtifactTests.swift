import XCTest

@testable import Cypher

/// Artifact fences read the way the desktop reads them (parser.rs
/// `artifact_info` / `fence_closed`, whose tests these mirror), and open as a
/// page.
final class ArtifactTests: XCTestCase {
    private func blocks(_ source: String) -> [MDBlock] {
        MarkdownParser.parse(source).map(\.block)
    }

    private func artifact(_ source: String) -> (ArtifactKind, String?, String, Bool)? {
        guard case .artifact(let kind, let title, let code, let closed, false) = blocks(source).first else {
            return nil
        }
        return (kind, title, code, closed)
    }

    func testArtifactFencesCarryKindTitleAndCode() throws {
        let page = try XCTUnwrap(artifact("```html artifact title=\"Bubble sort\"\n<p>a</p>\n<p>b</p>\n```\n"))
        XCTAssertEqual(page.0, .html)
        XCTAssertEqual(page.1, "Bubble sort")
        XCTAssertEqual(page.2, "<p>a</p>\n<p>b</p>")
        XCTAssertTrue(page.3)
        // The marker can come first; titles may be single-quoted or bare.
        XCTAssertEqual(artifact("```artifact svg title='Logo'\n<svg/>\n```")?.0, .svg)
        XCTAssertEqual(artifact("```Artifact HTML title=Plain\nx\n```")?.1, "Plain")
        // No language: HTML. A blank title is none.
        XCTAssertEqual(artifact("```artifact\nx\n```")?.0, .html)
        XCTAssertNil(try XCTUnwrap(artifact("```html artifact title=\"  \"\nx\n```")).1)
    }

    func testOnlyMarkedRenderableFencesAreArtifacts() {
        for source in [
            "```html\n<p>x</p>\n```",
            "```python artifact\nprint(1)\n```",
            "```html title=\"artifact\"\n<p>x</p>\n```",
            "```html artifacts\n<p>x</p>\n```",
        ] {
            guard case .codeBlock = blocks(source).first else {
                return XCTFail("not a code block: \(source)")
            }
        }
    }

    func testPatchFencesNameThePageTheyEdit() {
        let source =
            "```html artifact-patch title=\"Session explainer\"\n<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n```\n"
        XCTAssertEqual(
            blocks(source).first,
            .artifact(
                kind: .html, title: "Session explainer",
                code: "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE", closed: true, patch: true))
        guard case .artifact(.svg, _, _, _, true) = blocks("```Artifact-Patch svg title=Logo\nx\n```").first else {
            return XCTFail("an SVG patch")
        }
        // A patch to no page in particular, or in a language that doesn't
        // render, is only code.
        for source in ["```html artifact-patch\nx\n```", "```python artifact-patch title=X\nx\n```"] {
            guard case .codeBlock = blocks(source).first else {
                return XCTFail("not a code block: \(source)")
            }
        }
    }

    func testArtifactsKnowWhenTheirFenceClosed() {
        XCTAssertEqual(artifact("```html artifact\n<p>x</p>")?.3, false)
        XCTAssertEqual(artifact("```html artifact\n<p>x</p>\n``")?.3, false)
        XCTAssertEqual(artifact("```html artifact\n<p>x</p>\n```")?.3, true)
        // A longer opening fence needs a closing one as long.
        XCTAssertEqual(artifact("````html artifact\n```\n")?.3, false)
        XCTAssertEqual(artifact("~~~html artifact\nx\n~~~~\n")?.3, true)
        // After other blocks, the fence's own lines decide.
        guard
            case .artifact(_, _, _, let closed, _) = blocks("intro\n\n```html artifact\nx\n```\n\nafter").dropFirst()
                .first
        else { return XCTFail("no artifact after the intro") }
        XCTAssertTrue(closed)
        // Inside a quote, the markers before the fences don't count.
        guard case .blockquote(let children) = blocks("> ```html artifact\n> <p>x</p>\n> ```\n").first,
            case .artifact(_, _, _, true, _) = children.first
        else { return XCTFail("no closed artifact in the quote") }
    }

    /// cmark hands over the whole info string; only its first word names
    /// the language, as on the desktop.
    func testCodeBlockLanguageIsTheInfoStringsFirstWord() {
        XCTAssertEqual(
            blocks("```rust title=main.rs\nfn main() {}\n```"), [.codeBlock(language: "rust", code: "fn main() {}")])
        XCTAssertEqual(blocks("```\nplain\n```"), [.codeBlock(language: nil, code: "plain")])
    }

    func testTitlesFallBackToThePageThenTheKind() {
        XCTAssertEqual(
            ArtifactFence.pageTitle("<html><head><TITLE>\n  Sorting &amp; searching </TITLE>"), "Sorting & searching")
        XCTAssertNil(ArtifactFence.pageTitle("<title></title>"))
        XCTAssertNil(ArtifactFence.pageTitle("<title>never closed"))
        XCTAssertEqual(ArtifactFence.pageTitle("<title>Ünïcode İ</title>"), "Ünïcode İ")
        XCTAssertEqual(ArtifactFence.displayTitle(kind: .html, title: "Named", code: "<title>Page</title>"), "Named")
        XCTAssertEqual(ArtifactFence.displayTitle(kind: .html, title: nil, code: "<title>Page</title>"), "Page")
        XCTAssertEqual(ArtifactFence.displayTitle(kind: .svg, title: nil, code: "<svg/>"), "SVG image")
    }

    func testArtifactsAreTheirOwnRowsAndCommentAsTheirSource() {
        let block = MDBlock.artifact(kind: .html, title: "Demo", code: "<p>x</p>", closed: true)
        XCTAssertFalse(TranscriptTextStyle.isProse(block))
        XCTAssertEqual(block.commentText, "<p>x</p>")
    }

    func testSvgPagesWrapTheImageAndHtmlStaysAsWritten() {
        let svg = ArtifactViewer.document(for: ArtifactPage(kind: .svg, title: "Logo", code: "<svg/>"))
        XCTAssertTrue(svg.hasPrefix("<!doctype html>"))
        XCTAssertTrue(svg.contains("<body><svg/></body>"))
        let html = ArtifactViewer.document(for: ArtifactPage(kind: .html, title: "Page", code: "<p>x</p>"))
        XCTAssertEqual(html, "<p>x</p>")
    }
}

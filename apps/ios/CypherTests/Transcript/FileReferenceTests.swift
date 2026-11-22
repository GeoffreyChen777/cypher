import UIKit
import XCTest

@testable import Cypher

/// Mirrors crates/ui/src/markdown/code_ref.rs and crates/ui/src/files/links.rs.
final class FileReferenceTests: XCTestCase {
    private func ref(_ path: String, _ lines: ClosedRange<Int>? = nil) -> FileReference {
        FileReference(path: path, lines: lines)
    }

    func testCitedFilesAreReferences() {
        XCTAssertEqual(
            FileReference.detect("mvp_engine/engine/engine.py:394"), ref("mvp_engine/engine/engine.py", 394...394))
        XCTAssertEqual(
            FileReference.detect("utils/checkpointing/processors/file.py:69-73"),
            ref("utils/checkpointing/processors/file.py", 69...73))
        XCTAssertEqual(FileReference.detect("file.py:128-131"), ref("file.py", 128...131))
        XCTAssertEqual(FileReference.detect("core/context.py"), ref("core/context.py"))
        XCTAssertEqual(FileReference.detect("./src/a.rs:12:5"), ref("./src/a.rs", 12...12))
        XCTAssertEqual(FileReference.detect("/abs/a.rs#L7"), ref("/abs/a.rs", 7...7))
        XCTAssertEqual(FileReference.detect("a.rs#L7-L9"), ref("a.rs", 7...9))
        XCTAssertEqual(FileReference.detect("a.rs#L7-9"), ref("a.rs", 7...9))
        XCTAssertEqual(FileReference.detect("~/x/.env"), ref("~/x/.env"))
        XCTAssertEqual(FileReference.detect("docker/Dockerfile"), ref("docker/Dockerfile"))
        XCTAssertEqual(FileReference.detect("Makefile:12"), ref("Makefile", 12...12))
        XCTAssertEqual(FileReference.detect(" src/lib.rs "), ref("src/lib.rs"))
        // A reversed range keeps its first line.
        XCTAssertEqual(FileReference.detect("a.rs:20-12"), ref("a.rs", 20...20))
    }

    func testOtherCodeIsNotAReference() {
        for code in [
            "", "identity()", "rank", "world_size", "Cargo.toml", "data/rank_<n>.pt", "src/**/*.rs",
            "https://example.com/a.rs", "std::fs::read", "a.rs:x", "a.rs:0", "src/", "foo/bar", "v1.2.3",
            "1/2.5", "x.rs:12 y.rs:3", "$HOME/a.sh", "-rf/a.rs", "a//b.rs", "../..", "self.engine.step",
        ] {
            XCTAssertNil(FileReference.detect(code), code)
        }
    }

    func testBareNamesExpandToTheOneLongerPathBesideThem() {
        let context = ["utils/processors/file.py", "mvp/engine.py", "file.py", "core/context.py"]
        XCTAssertEqual(ref("file.py", 128...131).expanded(with: context), "utils/processors/file.py")
        XCTAssertEqual(ref("context.py", 1...1).expanded(with: context), "core/context.py")
        // Two different longer paths: no guess.
        XCTAssertNil(ref("x.rs", 1...1).expanded(with: ["a/x.rs", "b/x.rs"]))
        XCTAssertNil(ref("y.rs", 1...1).expanded(with: context))
        XCTAssertEqual(
            WorkspaceReference(path: "file.py", lines: 1...1, context: context).expandedPath,
            "utils/processors/file.py")
    }

    func testLabelsAreNamesWithLinesAndClashingNamesKeepAFolder() {
        XCTAssertEqual(
            FileReference.labels([
                ref("utils/checkpointing/processors/file.py", 69...73), ref("file.py", 128...131),
                ref("core/context.py", 146...160),
            ]),
            ["file.py:69-73", "file.py:128-131", "context.py:146-160"])
        XCTAssertEqual(FileReference.labels([ref("a/mod.rs", 1...1), ref("b/mod.rs")]), ["a/mod.rs:1", "b/mod.rs"])
        XCTAssertEqual(
            FileReference.labels([ref("a/mod.rs", 1...1), ref("a/mod.rs", 9...9)]), ["mod.rs:1", "mod.rs:9"])
    }

    func testFileLinksReadTheirPathAndLines() {
        func link(_ path: String, _ lines: ClosedRange<Int>? = nil, dir: Bool = false) -> FileLink {
            FileLink(path: path, lines: lines, dir: dir)
        }
        XCTAssertEqual(FileLink.parse("src/a.rs"), link("src/a.rs"))
        XCTAssertEqual(FileLink.parse("./src/a.rs:12"), link("./src/a.rs", 12...12))
        XCTAssertEqual(FileLink.parse("/abs/a.rs:12:5"), link("/abs/a.rs", 12...12))
        XCTAssertEqual(FileLink.parse("a.rs:12-20"), link("a.rs", 12...20))
        XCTAssertEqual(FileLink.parse("a.rs:20-12"), link("a.rs", 20...20))
        XCTAssertEqual(FileLink.parse("a.rs#L7"), link("a.rs", 7...7))
        XCTAssertEqual(FileLink.parse("a.rs#L7-L9"), link("a.rs", 7...9))
        XCTAssertEqual(FileLink.parse("a.rs#L7-9"), link("a.rs", 7...9))
        XCTAssertEqual(FileLink.parse("a.rs#L7C3"), link("a.rs", 7...7))
        XCTAssertEqual(FileLink.parse("guide/x.md#usage"), link("guide/x.md"))
        XCTAssertEqual(FileLink.parse("file:///abs/my%20a.rs"), link("/abs/my a.rs"))
        XCTAssertEqual(FileLink.parse("file://localhost/abs/a.rs"), link("/abs/a.rs"))
        XCTAssertEqual(FileLink.parse("cypher-file:src/lib.rs"), link("src/lib.rs"))
        XCTAssertEqual(FileLink.parse("cypher-file:crates/ui/"), link("crates/ui", dir: true))
        XCTAssertEqual(FileLink.parse("Cargo.toml"), link("Cargo.toml"))
        for url in [
            "https://example.com/a.rs", "http://localhost:3000", "mailto:a@b.c", "cypher-session:chat-1", "#heading",
            "",
        ] {
            XCTAssertNil(FileLink.parse(url), url)
        }
    }

    func testPathsResolveInsideTheCheckoutOrNot() {
        let cwd = "/repo/app/"
        XCTAssertEqual(FileLink.resolve("src/a.rs", cwd: cwd), .success("src/a.rs"))
        XCTAssertEqual(FileLink.resolve("./src/../b.rs", cwd: cwd), .success("b.rs"))
        XCTAssertEqual(FileLink.resolve("/repo/app/src/a.rs", cwd: cwd), .success("src/a.rs"))
        XCTAssertEqual(FileLink.resolve("/repo/app", cwd: cwd), .success(""))
        XCTAssertEqual(
            FileLink.resolve("/repo/application/a.rs", cwd: cwd), .failure(.init(path: "/repo/application/a.rs")))
        XCTAssertEqual(FileLink.resolve("../other/a.rs", cwd: cwd), .failure(.init(path: "/repo/other/a.rs")))
        XCTAssertEqual(FileLink.resolve("/etc/hosts", cwd: cwd), .failure(.init(path: "/etc/hosts")))
    }

    func testChipAndLinkURLsRoundTripToWhatTheyOpen() throws {
        let chip = try XCTUnwrap(
            TranscriptFileLinks.url(scheme: TranscriptFileLinks.referenceScheme, raw: "file.py:3-4"))
        let reference = try XCTUnwrap(TranscriptFileLinks.reference(chip, context: ["src/file.py"]))
        XCTAssertEqual(reference.expandedPath, "src/file.py")
        XCTAssertEqual(reference.lines, 3...4)
        XCTAssertEqual(TranscriptFileLinks.written(chip), "file.py:3-4")

        let link = try XCTUnwrap(TranscriptFileLinks.url(forLink: "docs/my guide.md#L2"))
        XCTAssertEqual(link.scheme, TranscriptFileLinks.linkScheme)
        let linked = try XCTUnwrap(TranscriptFileLinks.reference(link, context: ["x/docs/my guide.md"]))
        // A link means its own path: no expansion.
        XCTAssertEqual(linked.expandedPath, "docs/my guide.md")
        XCTAssertEqual(linked.lines, 2...2)

        let web = try XCTUnwrap(TranscriptFileLinks.url(forLink: "https://example.com"))
        XCTAssertEqual(web.scheme, "https")
        XCTAssertNil(TranscriptFileLinks.reference(web, context: []))
    }

    func testRepliesCiteThePathsTheirInlineCodeNames() {
        let entry = MessageEntry(
            id: "a", role: .assistant,
            parts: [
                .text(id: "t0", text: "See `src/a.rs:3` and `b.rs:4`, not `rank` or `Cargo.toml`. Again `src/a.rs`."),
                .reasoning(id: "r", text: "`hidden/c.rs:1`"),
            ],
            createdAt: 0, deviceId: "d", status: .complete, continuationOf: nil)
        XCTAssertEqual(TranscriptFileLinks.citedPaths(in: entry), ["src/a.rs", "b.rs"])
    }

    @MainActor
    func testChipsShowNameAndLinesAndCopyAsWritten() throws {
        let runs = [
            InlineRun(text: "Fixed in ", style: .plain),
            InlineRun(text: "utils/processors/file.py:69-73", style: InlineStyle(code: true)),
            InlineRun(text: " and ", style: .plain),
            InlineRun(text: "rank", style: InlineStyle(code: true)),
        ]
        let text = TranscriptTextStyle.inline(runs)
        XCTAssertTrue(text.string.contains("file.py:69-73"))
        XCTAssertFalse(text.string.contains("utils/processors"), "the chip shows the name, not the path")
        XCTAssertTrue(text.string.hasSuffix("rank"), "other inline code stays code")
        var chips = 0
        var margins = 0
        text.enumerateAttribute(.cypherFileRef, in: NSRange(location: 0, length: text.length)) { value, range, _ in
            guard let written = value as? String else { return }
            guard !written.isEmpty else {
                // The thin space either side, outside the chip's link.
                margins += 1
                XCTAssertEqual((text.string as NSString).substring(with: range), "\u{2009}")
                XCTAssertNil(text.attribute(.link, at: range.location, effectiveRange: nil))
                return
            }
            chips += 1
            XCTAssertNotNil(text.attribute(.link, at: range.location, effectiveRange: nil) as? URL)
            // The glyph is painted over the leading glue; no code wash.
            XCTAssertNotNil(text.attribute(.cypherFileGlyph, at: range.location, effectiveRange: nil))
            XCTAssertNil(text.attribute(.cypherFileGlyph, at: range.location + 2, effectiveRange: nil))
            XCTAssertNil(text.attribute(.cypherInlineCode, at: range.location + 2, effectiveRange: nil))
        }
        XCTAssertEqual(chips, 1)
        XCTAssertEqual(margins, 2)
        XCTAssertEqual(
            MathRenderer.plainText(text, range: NSRange(location: 0, length: text.length)),
            "Fixed in utils/processors/file.py:69-73 and rank")
        // Rebuilding yields an equal string, so a text view is left alone.
        XCTAssertTrue(text.isEqual(to: TranscriptTextStyle.inline(runs)))
    }

    /// The desktop's chip at rest (render.rs `ChipStyle`): exact ink alphas,
    /// black on light and white on dark.
    func testChipPaintIsNeutralInkAtExactAlphas() {
        for (style, white) in [(UIUserInterfaceStyle.light, CGFloat(0)), (.dark, 1)] {
            let traits = UITraitCollection(userInterfaceStyle: style)
            for (color, alpha) in [(FileReferenceChip.wash, 0.03), (FileReferenceChip.hairline, 0.08)] {
                var w: CGFloat = -1
                var a: CGFloat = -1
                XCTAssertTrue(color.resolvedColor(with: traits).getWhite(&w, alpha: &a))
                XCTAssertEqual(w, white, accuracy: 0.0001)
                XCTAssertEqual(a, CGFloat(alpha), accuracy: 0.0001)
            }
        }
        // The desktop's geometry (render.rs): inline code's box and radius,
        // 4 pt to the glyph, 5 pt past the label, 3 pt at a line break, the
        // glyph 0.9 of the name's size, the glue 1.45 of it.
        XCTAssertEqual(FileReferenceChip.heightEm, InlineCodeBox.heightEm)
        XCTAssertEqual(FileReferenceChip.radius, InlineCodeBox.radius)
        XCTAssertEqual(FileReferenceChip.radius, 5)
        XCTAssertEqual(FileReferenceChip.padLeft, 4)
        XCTAssertEqual(FileReferenceChip.padRight, 5)
        XCTAssertEqual(FileReferenceChip.padWrap, 3)
        XCTAssertEqual(FileReferenceChip.glyphEm, 0.9)
        XCTAssertEqual(FileReferenceChip.glyphSide(MD.textSize), 11)
        XCTAssertEqual(FileReferenceChip.glueWidth(MD.textSize), MD.textSize * 0.86 * 1.45, accuracy: 0.0001)
    }

    @MainActor
    func testChipNameIsMediumCodeTypeAndTheLinesMuted() throws {
        let runs = [
            InlineRun(text: "See ", style: .plain),
            InlineRun(text: "src/a.rs:3-9", style: InlineStyle(code: true)),
            InlineRun(text: " and ", style: .plain),
            InlineRun(text: "src/b.rs", style: InlineStyle(bold: true, code: true)),
        ]
        let text = TranscriptTextStyle.inline(runs)
        let string = text.string as NSString
        let name = string.range(of: "a.rs")
        let font = try XCTUnwrap(text.attribute(.font, at: name.location, effectiveRange: nil) as? UIFont)
        // The one code scale, as inline code.
        XCTAssertEqual(FileReferenceChip.nameEm, Theme.codeScale)
        XCTAssertEqual(font.pointSize, MD.textSize * 0.86, accuracy: 0.001)
        XCTAssertTrue(font.fontName.hasPrefix("GeistMono"), font.fontName)
        let variation = CTFontCopyVariation(font as CTFont) as? [Int: Double]
        XCTAssertEqual(variation?[Theme.monoWeightAxis], 500)
        XCTAssertEqual(
            text.attribute(.foregroundColor, at: name.location, effectiveRange: nil) as? UIColor, UIColor(Theme.text))
        let lines = string.range(of: ":3-9")
        XCTAssertEqual(text.attribute(.font, at: lines.location, effectiveRange: nil) as? UIFont, font)
        XCTAssertEqual(
            text.attribute(.foregroundColor, at: lines.location, effectiveRange: nil) as? UIColor,
            UIColor(Theme.textMuted))
        // The right padding rides on the label's last character.
        XCTAssertEqual(
            text.attribute(.kern, at: lines.location + lines.length - 1, effectiveRange: nil) as? CGFloat,
            FileReferenceChip.padRight)
        // In bold text, semibold.
        let bold = try XCTUnwrap(
            text.attribute(.font, at: string.range(of: "b.rs").location, effectiveRange: nil) as? UIFont)
        XCTAssertEqual((CTFontCopyVariation(bold as CTFont) as? [Int: Double])?[Theme.monoWeightAxis], 600)
        // Two chips, told apart.
        XCTAssertEqual(text.attribute(.cypherFileChip, at: name.location, effectiveRange: nil) as? Int, 0)
        XCTAssertEqual(
            text.attribute(.cypherFileChip, at: string.range(of: "b.rs").location, effectiveRange: nil) as? Int, 1)
    }

    /// Lays `text` out as a transcript text view does, `width` wide (the
    /// layout manager holds its storage weakly: keep the storage).
    private func layOut(
        _ text: NSAttributedString, width: CGFloat
    ) -> (NSTextStorage, NSLayoutManager, NSTextContainer) {
        let storage = NSTextStorage(attributedString: text)
        let layout = TranscriptTextLayoutManager()
        let container = NSTextContainer(size: CGSize(width: width, height: .greatestFiniteMagnitude))
        container.lineFragmentPadding = 0
        layout.addTextContainer(container)
        storage.addLayoutManager(layout)
        layout.ensureLayout(for: container)
        return (storage, layout, container)
    }

    private func chipRanges(_ text: NSAttributedString) -> [NSRange] {
        var ranges: [NSRange] = []
        text.enumerateAttribute(.cypherFileChip, in: NSRange(location: 0, length: text.length)) { value, range, _ in
            if value != nil { ranges.append(range) }
        }
        return ranges
    }

    @MainActor
    func testAChipBoxHoldsItsPaddingInsideItsLine() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "The greeting lives in ", style: .plain),
            InlineRun(text: "Sources/Example.swift:20", style: InlineStyle(code: true)),
            InlineRun(text: ", see.", style: .plain),
        ])
        let (storage, layout, container) = layOut(text, width: 1_000)
        let chip = try XCTUnwrap(chipRanges(text).first)
        let boxes = FileReferenceChip.boxes(layout, storage: storage, container: container, chip: chip)
        XCTAssertEqual(boxes.count, 1)
        let box = try XCTUnwrap(boxes.first)
        XCTAssertEqual(box.height, 15, "inline code's box: 1.25 em of the 12.04 pt name")
        let line = layout.lineFragmentRect(forGlyphAt: 0, effectiveRange: nil)
        XCTAssertEqual(line.height, MD.lineHeight, "a chip never changes the line height")
        XCTAssertGreaterThanOrEqual(box.minY, line.minY)
        XCTAssertLessThanOrEqual(box.maxY, line.maxY)
        let font = try XCTUnwrap(storage.attribute(.font, at: chip.location, effectiveRange: nil) as? UIFont)
        // Centered on the name's cap height.
        let baseline = line.minY + layout.location(forGlyphAt: layout.glyphIndexForCharacter(at: chip.location)).y
        XCTAssertEqual(box.midY, baseline - font.capHeight / 2, accuracy: 0.5)
        // Left padding, glyph and gap before the name; the right padding
        // after the label.
        let advance = NSAttributedString(string: "x", attributes: [.font: font]).size().width
        let label = CGFloat("Example.swift:20".count) * advance
        let glue = FileReferenceChip.glueWidth(MD.textSize)
        XCTAssertGreaterThan(glue, FileReferenceChip.padLeft + FileReferenceChip.glyphSide(MD.textSize) + 2)
        XCTAssertEqual(box.width, glue + label + FileReferenceChip.padRight, accuracy: 0.01)
        let name = (text.string as NSString).range(of: "Example")
        let nameX = layout.location(forGlyphAt: layout.glyphIndexForCharacter(at: name.location)).x
        XCTAssertEqual(nameX - box.minX, glue, accuracy: 0.01)
    }

    @MainActor
    func testAChipThatNoLongerFitsMovesToTheNextLineWhole() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "Read it in ", style: .plain),
            InlineRun(text: "src/a/lib.rs:41", style: InlineStyle(code: true)),
            InlineRun(text: " and ", style: .plain),
            InlineRun(text: "src/b/lib.rs:7", style: InlineStyle(code: true)),
            InlineRun(text: " then.", style: .plain),
        ])
        let string = text.string as NSString
        // Labels `a/lib.rs:41`, `b/lib.rs:7`: a word break after a `/` would
        // split one. Cut the line inside the second chip, past its `/`.
        let slash = string.range(of: "b/")
        let (probeStorage, probe, _) = layOut(text, width: 10_000)
        let cut = withExtendedLifetime(probeStorage) {
            probe.location(forGlyphAt: probe.glyphIndexForCharacter(at: slash.location + 3)).x
        }
        let (storage, layout, container) = layOut(text, width: cut)
        let chip = chipRanges(text)[1]
        let boxes = FileReferenceChip.boxes(layout, storage: storage, container: container, chip: chip)
        XCTAssertEqual(boxes.count, 1, "the chip moved to the next line whole")
        XCTAssertEqual(boxes.first?.minX ?? -1, 0, accuracy: 0.01)
    }

    @MainActor
    func testAChipWiderThanItsLineGetsABoxPerLineInsideTheView() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "Long: ", style: .plain),
            InlineRun(
                text: "src/a_rather_long_generated_preview_fixture_name.swift:120-148",
                style: InlineStyle(code: true)),
            InlineRun(text: " done.", style: .plain),
        ])
        let width: CGFloat = 200
        let (storage, layout, container) = layOut(text, width: width)
        let chip = try XCTUnwrap(chipRanges(text).first)
        let boxes = FileReferenceChip.boxes(layout, storage: storage, container: container, chip: chip)
        XCTAssertGreaterThan(boxes.count, 1)
        for box in boxes {
            XCTAssertGreaterThanOrEqual(box.minX, 0)
            XCTAssertLessThanOrEqual(box.maxX, width)
            XCTAssertEqual(box.height, 15)
        }
        XCTAssertEqual(Set(boxes.map(\.minY)).count, boxes.count, "one box per line")
    }

    @MainActor
    func testFileLinksGetAURLTheTextViewKeeps() throws {
        let text = TranscriptTextStyle.inline([
            InlineRun(text: "the config", style: InlineStyle(link: "src/my config.toml:3"))
        ])
        let url = try XCTUnwrap(text.attribute(.link, at: 0, effectiveRange: nil) as? URL)
        XCTAssertEqual(TranscriptFileLinks.reference(url, context: [])?.path, "src/my config.toml")
    }

    func testSnippetsCutTheCitedLinesWithContext() {
        let text = (1...40).map { "line \($0)" }.joined(separator: "\n")
        let snippet = FileReferenceSnippet.cut(text, path: "a.txt", cited: 20...21)
        XCTAssertEqual(snippet.firstLine, 17)
        XCTAssertEqual(snippet.lines.first, "line 17")
        XCTAssertEqual(snippet.lines.last, "line 24")
        let whole = FileReferenceSnippet.cut(text, path: "a.txt", cited: nil)
        XCTAssertEqual(whole.lines.count, FileReferenceSnippet.maxLines)
        let past = FileReferenceSnippet.cut("one\ntwo", path: "a.txt", cited: 9...9)
        XCTAssertEqual(past.lines, ["one", "two"])
    }

    func testPlainTextFallbackFindsTheCitedLines() throws {
        let text = "a\nbb\nccc\n"
        let range = try XCTUnwrap(WorkspaceCodeView.characterRange(of: 2...3, in: text))
        XCTAssertEqual((text as NSString).substring(with: range), "bb\nccc\n")
        XCTAssertNil(WorkspaceCodeView.characterRange(of: 9...9, in: text))
    }
}

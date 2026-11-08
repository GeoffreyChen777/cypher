import XCTest

@testable import Cypher

/// Mirrors crates/ui/src/markdown/diff_fence.rs's parser tests.
final class DiffFenceTests: XCTestCase {
    private let chatDiff = """
        fingerprint = {
        -        "name": a,
                 "chat": b,
        @@ slow path
        +        if x:
        +            y = 1
        \\ No newline at end of file
        """

    func testDiffTagsAreRecognized() {
        XCTAssertTrue(DiffFence.isDiff("diff"))
        XCTAssertTrue(DiffFence.isDiff("Patch"))
        XCTAssertTrue(DiffFence.isDiff("udiff"))
        XCTAssertFalse(DiffFence.isDiff("python"))
        XCTAssertFalse(DiffFence.isDiff(nil))
    }

    func testChatDiffsParseIntoOneUnnumberedFile() {
        let files = DiffFence.parse(chatDiff)
        XCTAssertEqual(files.count, 1)
        let file = files[0]
        XCTAssertEqual(file.additions, 2)
        XCTAssertEqual(file.deletions, 1)
        XCTAssertEqual(file.maxLine, 0)
        XCTAssertEqual(file.hunks.map(\.header), ["", "@@ slow path"])
        // An unprefixed line keeps its text whole.
        XCTAssertEqual(file.hunks[0].lines[0], .init(kind: .context, text: "fingerprint = {"))
        XCTAssertEqual(file.hunks[0].lines[2].text, "        \"chat\": b,")
        XCTAssertEqual(file.hunks[1].lines[2].kind, .meta)
        XCTAssertEqual(file.hunks[1].lines[2].text, "No newline at end of file")
    }

    func testGitPatchesKeepPathsAndNumbers() {
        let files = DiffFence.parse(
            """
            diff --git a/src/a.py b/src/a.py
            index 1..2 100644
            --- a/src/a.py
            +++ b/src/a.py
            @@ -10,3 +10,3 @@ def f():
             ctx
            -old
            +new
            --- b.rs\t2024-01-01
            +++ b.rs
            @@ -1 +1 @@
            -x
            +y
            """)
        XCTAssertEqual(files.map(\.path), ["src/a.py", "b.rs"])
        let lines = files[0].hunks[0].lines
        XCTAssertEqual(lines.map(\.oldNo), [10, 11, nil])
        XCTAssertEqual(lines.map(\.newNo), [10, nil, 11])
        XCTAssertEqual(files[0].maxLine, 11)
        XCTAssertEqual(files[1].hunks[0].lines.count, 2)
    }

    func testNewFilesAndLoneDashes() {
        let files = DiffFence.parse("--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1 @@\n+a\n--- not a header")
        XCTAssertEqual(files[0].status, .added)
        XCTAssertEqual(files[0].path, "new.ts")
        XCTAssertEqual(files[0].notices, ["New file"])
        // A `---` without its `+++` partner is a deleted line.
        let last = files[0].hunks[0].lines.last
        XCTAssertEqual(last?.kind, .del)
        XCTAssertEqual(last?.text, "-- not a header")
    }

    func testRowsColorOnlyTheFenceLanguageAndStripSeveralFiles() {
        let rows = DiffFenceRow.rows(
            DiffFence.parse(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-let a;\n+let b;\n\\ meta\n--- a/y.py\n+++ b/y.py\n+z"))
        XCTAssertEqual(rows.first?.kind, .file(additions: 1, deletions: 1))
        XCTAssertEqual(rows.first?.text, "x.rs")
        let code = rows.filter { if case .line = $0.kind { return true } else { return false } }
        XCTAssertEqual(code.map(\.language), [.rust, .rust, nil, nil])
        XCTAssertTrue(rows.contains { $0.kind == .hunk && $0.numbered })
        // A single file names the fence: no strip.
        XCTAssertFalse(
            DiffFenceRow.rows(DiffFence.parse(chatDiff)).contains {
                if case .file = $0.kind { return true } else { return false }
            })
    }

    func testMarkdownFencesTaggedDiffParseAsCodeBlocks() {
        let blocks = MarkdownParser.parse("```diff\n-a\n+b\n```")
        XCTAssertEqual(blocks.first?.block, .codeBlock(language: "diff", code: "-a\n+b", closed: true))
    }
}

extension DiffFence.Line {
    fileprivate init(kind: DiffFence.LineKind, text: String) {
        self.init(kind: kind, oldNo: nil, newNo: nil, text: text)
    }
}

import Loro
import XCTest
@testable import Cypher

/// The chat's named pages fold the way the desktop's index does
/// (crates/ui/src/transcript/artifacts.rs), over the whole doc.
final class ArtifactOutlineTests: XCTestCase {
    private let first = "```html artifact title=\"Demo\"\n<p>one</p>\n```\n"
    private let second = "Revised:\n\n```html artifact title=\"Demo\"\n<p>two</p>\n```\n"
    private let patch = """
        ```html artifact-patch title="Demo"
        <<<<<<< SEARCH
        <p>one</p>
        =======
        <p>one, patched</p>
        >>>>>>> REPLACE
        <<<<<<< SEARCH
        <p>gone</p>
        =======
        x
        >>>>>>> REPLACE
        ```

        """

    private var patchCode: String {
        ArtifactOutline.fences(in: [patch])[0].code
    }

    private func reply(_ id: String, _ text: String, status: MessageStatus = .complete) -> MessageEntry {
        .fixture(id, parts: [.text(id: "t0", text: text)], status: status)
    }

    func testAPatchRevisesThePageBeforeItAndReportsHowItWent() {
        let pages = ArtifactOutline.of([reply("a", first), reply("b", patch)]).pages
        XCTAssertEqual(pages.latest["Demo"]?.code, "<p>one, patched</p>")
        XCTAssertEqual(pages.latest["Demo"]?.key, "b#0")
        XCTAssertEqual(
            pages.outcomes[.init(entry: "b", code: patchCode)],
            PatchOutcome(applied: 1, total: 2, foundPage: true))
        // A whole page after it replaces the patched one.
        let rewritten = ArtifactOutline.of([reply("a", first), reply("b", patch), reply("c", second)]).pages
        XCTAssertEqual(rewritten.latest["Demo"]?.code, "<p>two</p>")
    }

    func testAPatchWithNoPageBeforeItFindsNothing() {
        let pages = ArtifactOutline.of([reply("b", patch)]).pages
        XCTAssertNil(pages.latest["Demo"])
        XCTAssertEqual(
            pages.outcomes[.init(entry: "b", code: patchCode)],
            PatchOutcome(applied: 0, total: 2, foundPage: false))
    }

    func testAStreamingReplyCountsOnlyItsClosedFences() {
        let streaming = "```html artifact-patch title=\"Demo\"\n<<<<<<< SEARCH\n<p>one</p>\n=======\n<p>t"
        let open = ArtifactOutline.of([reply("a", first), reply("b", streaming, status: .streaming)])
        XCTAssertEqual(open.pages.latest["Demo"]?.code, "<p>one</p>")
        let closed = ArtifactOutline.of([reply("a", first), reply("b", patch, status: .streaming)])
        XCTAssertEqual(closed.pages.latest["Demo"]?.code, "<p>one, patched</p>")
        // Prompts aren't pages, and unnamed fences aren't revised.
        let other = ArtifactOutline.of([
            .fixture("u", role: .user, parts: [.text(id: "t0", text: second)]),
            reply("c", "```html artifact\n<p>no name</p>\n```"),
        ])
        XCTAssertTrue(other.pages.latest.isEmpty)
    }

    func testTheDocIsReadOnceForSettledMessagesAndAgainForTheTail() throws {
        let doc = LoroDoc()
        try append(to: doc, "a", first)
        for ix in 0..<45 { try append(to: doc, "p\(ix)", "Prose \(ix).") }
        // More than a page of new messages waits for the background read.
        XCTAssertNil(ArtifactOutline.extend(ArtifactOutline(), of: doc, limit: 40))
        var outline = try XCTUnwrap(ArtifactOutline.extend(ArtifactOutline(), of: doc))
        XCTAssertEqual(outline.upTo, 46)
        XCTAssertEqual(outline.pages.latest["Demo"]?.code, "<p>one</p>")

        // The page from far above the window is there for a streaming patch.
        let text = try append(to: doc, "b", "", status: .streaming)
        try text.insert(pos: 0, s: patch)
        doc.commit()
        outline = try XCTUnwrap(ArtifactOutline.extend(outline, of: doc, limit: 40))
        XCTAssertEqual(outline.upTo, 46, "a streaming message stays the tail")
        XCTAssertEqual(outline.pages.latest["Demo"]?.code, "<p>one, patched</p>")

        // Rewound under what was read: read again from the start.
        try doc.getList(id: "messages").delete(pos: 1, len: 46)
        doc.commit()
        outline = try XCTUnwrap(ArtifactOutline.extend(outline, of: doc))
        XCTAssertEqual(outline.upTo, 1)
        XCTAssertEqual(outline.pages.latest["Demo"]?.code, "<p>one</p>")
    }

    @MainActor
    func testTheShelfOpensANamedPageAtItsNewestVersion() {
        let shelf = ArtifactShelf()
        shelf.update(ArtifactOutline.of([reply("a", first), reply("b", patch)]).pages)
        let card = ArtifactPage(kind: .html, title: "Demo", code: "<p>one</p>", name: "Demo")
        XCTAssertEqual(shelf.latest(card).code, "<p>one, patched</p>")
        let unnamed = ArtifactPage(kind: .html, title: "Demo", code: "<p>mine</p>")
        XCTAssertEqual(shelf.latest(unnamed).code, "<p>mine</p>")
        XCTAssertEqual(shelf.outcome(entry: "b", code: patchCode)?.applied, 1)
    }

    @MainActor
    func testTheDemoChatsPatchAppliesEveryEdit() throws {
        let store = DemoDataset.standard().sessionStore(for: "chat-artifacts")
        let pages = store.artifactShelf.pages
        let page = try XCTUnwrap(pages.latest["Binary search, step by step"])
        XCTAssertEqual(page.key, "m4#0")
        XCTAssertTrue(page.code.contains("const counted = () =>"))
        XCTAssertEqual(page.code.components(separatedBy: "counted();").count - 1, 2)
        XCTAssertEqual(pages.outcomes.values.first, PatchOutcome(applied: 5, total: 5, foundPage: true))
    }

    @MainActor
    func testTheDemoExplainersPatchesLandAsTheirRepliesSay() throws {
        let store = DemoDataset.standard().sessionStore(for: "chat-explainer")
        let pages = store.artifactShelf.pages
        let outcomes = Dictionary(
            uniqueKeysWithValues: pages.outcomes.map { ($0.key.entry, $0.value) })
        XCTAssertEqual(outcomes["a1"], PatchOutcome(applied: 2, total: 2, foundPage: true))
        XCTAssertEqual(outcomes["a2"], PatchOutcome(applied: 2, total: 3, foundPage: true))
        XCTAssertEqual(outcomes["a3"], PatchOutcome(applied: 0, total: 1, foundPage: false))
        let page = try XCTUnwrap(pages.latest["Session explainer"])
        XCTAssertEqual(page.key, "a2#0")
        XCTAssertTrue(page.code.contains("reading the whole chat"))
        XCTAssertTrue(page.code.contains("revisions yet."))
        XCTAssertNil(pages.latest["Release notes"])
    }

    func testPatchCardsReadHowTheirEditsWent() {
        let status = { (writing: Bool, hunks: Int, outcome: PatchOutcome?) in
            ArtifactCard.patchStatus(writing: writing, hunks: hunks, outcome: outcome)
        }
        XCTAssertEqual(status(true, 1, nil).subtitle, "Updating… · 1 edit")
        XCTAssertEqual(status(false, 2, .init(applied: 2, total: 2, foundPage: true)).subtitle, "Updated · 2 edits")
        let partial = status(false, 3, .init(applied: 2, total: 3, foundPage: true))
        XCTAssertEqual(partial.subtitle, "Updated · 2 of 3 edits applied")
        XCTAssertTrue(partial.warns && partial.opens)
        XCTAssertEqual(
            status(false, 1, .init(applied: 0, total: 1, foundPage: true)).subtitle, "Update didn't apply · 0 of 1 edit"
        )
        let orphan = status(false, 1, .init(applied: 0, total: 1, foundPage: false))
        XCTAssertEqual(orphan.subtitle, "No earlier version to update")
        XCTAssertFalse(orphan.opens)
        XCTAssertFalse(status(false, 1, nil).opens)
    }

    /// A reply as the host writes it: its text a LoroText container.
    @discardableResult
    private func append(
        to doc: LoroDoc, _ id: String, _ text: String, status: MessageStatus = .complete
    ) throws -> LoroText {
        let message = try doc.getList(id: "messages").pushContainer(child: LoroMap())
        try message.insert(key: "id", v: id)
        try message.insert(key: "role", v: "assistant")
        try message.insert(key: "status", v: status.rawValue)
        let parts = try message.insertContainer(key: "parts", child: LoroList())
        let part = try parts.pushContainer(child: LoroMap())
        try part.insert(key: "id", v: "t0")
        try part.insert(key: "kind", v: "text")
        let body = try part.insertContainer(key: "text", child: LoroText())
        if !text.isEmpty { try body.insert(pos: 0, s: text) }
        doc.commit()
        return body
    }
}

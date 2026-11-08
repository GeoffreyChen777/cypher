import Loro
import XCTest

@testable import Cypher

@MainActor
final class TurnChangesTests: XCTestCase {
    private let record: [String: Any] = [
        "cwd": "/repo", "base": String(repeating: "a", count: 40), "head": String(repeating: "b", count: 40),
        "files": [
            ["path": "src/a.rs", "status": "modified", "additions": 3, "deletions": 1],
            ["path": "img.png", "status": "added", "additions": 0, "deletions": 0, "binary": true],
            ["path": "new/b.rs", "oldPath": "old/b.rs", "status": "renamed", "additions": 0, "deletions": 0],
        ],
        "fileCount": 9, "additions": 30, "deletions": 4,
    ]

    private func entries(status: String = "complete", changes: Any? = nil) throws -> [MessageEntry] {
        var answer: [String: Any] = [
            "id": "a1", "role": "assistant", "createdAt": 2, "status": status,
            "parts": [["id": "t0", "kind": "text", "text": "Done."]],
        ]
        if let changes { answer["turnChanges"] = changes }
        let json: [String: Any] = [
            "messages": [
                ["id": "u1", "role": "user", "createdAt": 1, "parts": [["id": "t0", "kind": "text", "text": "Go"]]],
                answer,
            ]
        ]
        return SessionStore.decodeEntries(root: try XCTUnwrap(LoroValue.fromJSON(json).mapValue))
    }

    func testTheRecordDecodesFromTheTranscriptDoc() throws {
        let changes = try XCTUnwrap(try entries(changes: record)[1].turnChanges)
        XCTAssertEqual(changes.files.map(\.path), ["src/a.rs", "img.png", "new/b.rs"])
        XCTAssertFalse(changes.files[0].binary, "binary defaults off")
        XCTAssertTrue(changes.files[1].binary)
        XCTAssertEqual(changes.files[2].oldPath, "old/b.rs")
        XCTAssertEqual(changes.label, "9 files changed")
        XCTAssertEqual(changes.hiddenFiles, 6)
        // A record this app can't read costs only the card.
        let entries = try entries(changes: ["cwd": "/repo"])
        XCTAssertNil(entries[1].turnChanges)
        XCTAssertEqual(entries.count, 2)
    }

    func testLabelsAndStatusWords() {
        let one = TurnChanges(
            cwd: "/r", base: "a", head: "b",
            files: [.init(path: "a", status: "deleted", additions: 0, deletions: 3, binary: false)],
            fileCount: 1, additions: 0, deletions: 3)
        XCTAssertEqual(one.label, "1 file changed")
        XCTAssertEqual(one.hiddenFiles, 0)
        XCTAssertEqual(TurnChanges.statusTag("added")?.word, "new")
        XCTAssertEqual(TurnChanges.statusTag("deleted")?.added, false)
        XCTAssertEqual(TurnChanges.statusTag("unmerged")?.word, "conflict")
        XCTAssertNil(TurnChanges.statusTag("modified"))
    }

    func testTheCardClosesASettledAnswerUnderItsLastRow() throws {
        let rows = buildRows(try entries(changes: record))
        let card = try XCTUnwrap(rows.last)
        guard case .turnChanges(let changes) = card.kind else { return XCTFail("No card: \(rows.map(\.id))") }
        XCTAssertEqual(card.id, "a1#changes")
        XCTAssertEqual(changes.fileCount, 9)
        // The strip rides under the card.
        XCTAssertNotNil(card.timestamp)
        // Streaming: no card yet.
        let live = buildRows(try entries(status: "streaming", changes: record))
        XCTAssertFalse(live.contains { if case .turnChanges = $0.kind { true } else { false } })
        // A round that changed nothing has no record and no card.
        XCTAssertFalse(buildRows(try entries()).contains { $0.id == "a1#changes" })
    }

    func testRoundDiffsAskForTheTreesMode() {
        let missing = RelayError.rpc("git diff-tree: fatal: bad object aaaa")
        XCTAssertTrue(WorkspaceBrowserSession.isMissingSnapshot(missing))
        let changes = TurnChanges(cwd: "/r", base: "a", head: "b", files: [], fileCount: 0, additions: 0, deletions: 0)
        XCTAssertEqual(
            WorkspaceBrowserSession.errorMessage(missing, scope: .round(changes)),
            "This turn's snapshot is no longer in the repository.")
        XCTAssertNotEqual(
            WorkspaceBrowserSession.errorMessage(missing, scope: .working),
            "This turn's snapshot is no longer in the repository.")
    }

    func testTheDemoShowsARoundWithItsFiles() throws {
        let demo = DemoDataset.standard()
        let entries = demo.sessionStore(for: "chat-references").entries
        let changes = try XCTUnwrap(entries.last?.turnChanges)
        XCTAssertEqual(changes.files.count, 3)
        let rows = buildRows(entries)
        XCTAssertTrue(rows.contains { if case .turnChanges = $0.kind { true } else { false } })
    }
}

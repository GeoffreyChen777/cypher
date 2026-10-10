import XCTest
@testable import Cypher

@MainActor
final class WorkspaceFilesTests: XCTestCase {
    private func snapshot(_ patch: String, files: [WorkspaceChange]) -> WorkspaceChanges {
        WorkspaceChanges(checkoutId: "checkout", deviceId: "host", cwd: "/repo", patch: patch,
                         files: files, additions: 1, deletions: 1, truncated: false,
                         checksum: "sha", updatedAt: "snapshot")
    }

    func testPathsNeverEscapeTheCheckoutOrEnterGitMetadata() {
        for path in ["/etc/passwd", "../x", "src/../../x", "src/./x", "src//x",
                     ".git/config", "src/.git/config", ".GIT/config", "src/.Git/config", "a\0b"] {
            XCTAssertFalse(WorkspaceFilePath.valid(path), path)
        }
        XCTAssertTrue(WorkspaceFilePath.valid(""))
        XCTAssertFalse(WorkspaceFilePath.valid("", allowRoot: false))
        XCTAssertEqual(WorkspaceFilePath.child("hello world.swift", in: "src"), "src/hello world.swift")
        XCTAssertNil(WorkspaceFilePath.child("../escape", in: "src"))
        XCTAssertNil(WorkspaceFilePath.child(".git", in: ""))
    }

    func testPerFileDiffUsesCompletePathsAndKeepsSnapshotBoundaries() {
        let a = WorkspaceChange(path: "a/file.swift", status: "M", additions: 1, deletions: 1, binary: false)
        let b = WorkspaceChange(path: "b/file.swift", status: "M", additions: 1, deletions: 0, binary: false)
        let patch = "diff --git a/a/file.swift b/a/file.swift\n--- a/a/file.swift\n+++ b/a/file.swift\n-old\n+new\ndiff --git a/b/file.swift b/b/file.swift\n+other\n"
        let diff = snapshot(patch, files: [a, b])
        XCTAssertTrue(diff.patch(for: a)?.contains("+new") == true)
        XCTAssertFalse(diff.patch(for: a)?.contains("+other") == true)
        XCTAssertTrue(diff.patch(for: b)?.contains("+other") == true)
        XCTAssertNil(snapshot("", files: [a]).patch(for: a), "Missing/truncated hunks are not empty diffs")
    }

    func testRenamesSpacesAndGitEscapedNamesAreMatchedWithoutGuessing() {
        let rename = WorkspaceChange(path: "new name", oldPath: "old name", status: "R", additions: 0, deletions: 0, binary: false)
        let patch = "diff --git a/old name b/new name\nsimilarity index 100%\nrename from old name\nrename to new name\n"
        XCTAssertNotNil(snapshot(patch, files: [rename]).patch(for: rename))
        let weird = WorkspaceChange(path: "a\t中", status: "M", additions: 1, deletions: 0, binary: false)
        let quoted = "diff --git \"a/a\\t\\344\\270\\255\" \"b/a\\t\\344\\270\\255\"\n+change\n"
        XCTAssertNotNil(snapshot(quoted, files: [weird]).patch(for: weird))
        let noQuotePath = "diff --git \"a/a\\t中\" \"b/a\\t中\"\n+change\n"
        XCTAssertNotNil(snapshot(noQuotePath, files: [weird]).patch(for: weird))
    }

    func testAmbiguousRenameHeadersAreNotAssignedToTheWrongFile() {
        let a = WorkspaceChange(path: "z", oldPath: "x b/y", status: "R", additions: 1, deletions: 1, binary: false)
        let b = WorkspaceChange(path: "y b/z", oldPath: "x", status: "R", additions: 1, deletions: 1, binary: false)
        let diff = snapshot("diff --git a/x b/y b/z\n+ambiguous\n", files: [a, b])
        XCTAssertNil(diff.patch(for: a))
        XCTAssertNil(diff.patch(for: b))
    }

    func testPreviewIsByteBoundedWithoutCorruptingUnicode() {
        let text = String(repeating: "a", count: WorkspaceTextPreview.limit - 1) + "世界"
        let preview = WorkspaceTextPreview(text)
        XCTAssertTrue(preview.truncated)
        XCTAssertEqual(preview.text.utf8.count, WorkspaceTextPreview.limit - 1)
        XCTAssertFalse(preview.text.contains("\u{fffd}"))
        XCTAssertFalse(WorkspaceTextPreview("").truncated)
    }

    func testWireRepliesDecodeTextBinaryAndPartialDirectories() throws {
        let text = try JSONDecoder().decode(WorkspaceFileContent.self, from: Data(
            #"{"text":"hello","bytes":1000000,"binary":false,"truncated":true}"#.utf8))
        XCTAssertEqual(text.text, "hello")
        XCTAssertTrue(text.truncated)
        let binary = try JSONDecoder().decode(WorkspaceFileContent.self, from: Data(
            #"{"text":null,"bytes":9,"binary":true,"truncated":false}"#.utf8))
        XCTAssertNil(binary.text)
        let directory = try JSONDecoder().decode(WorkspaceDirectory.self, from: Data(
            #"{"entries":[{"name":"src","isDir":true}],"truncated":true}"#.utf8))
        XCTAssertTrue(directory.entries[0].isDir)
        XCTAssertTrue(directory.truncated)
    }

    func testDemoAndCheckoutChangesCannotFallBackToAnotherContext() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let chat = try XCTUnwrap(model.chat(id: "chat-tabs"))
        let browser = WorkspaceBrowserSession(model: model, chat: chat)
        let root = try await browser.directory("")
        XCTAssertTrue(root.entries.contains { $0.name == "Sources" })
        let content = try await browser.file("Sources/Example.swift")
        XCTAssertTrue(content.text?.contains("Hello from Cypher") == true)
        let changes = try await browser.changes()
        XCTAssertNotNil(changes.patch(for: changes.files[0]))
        let index = try XCTUnwrap(model.demo?.chats.firstIndex { $0.id == chat.id })
        model.demo?.chats[index].cwd = "/different-checkout"
        do {
            _ = try await browser.file("README.md")
            XCTFail("Old sheet must not read a new checkout")
        } catch {
            XCTAssertTrue(error is WorkspaceBrowserError)
        }
        model.enterDemoMode()
        do {
            _ = try await browser.directory("")
            XCTFail("Replacing the source invalidates the old browser")
        } catch {
            XCTAssertTrue(error is WorkspaceBrowserError)
        }
    }

    func testOldEngineShowsUpgradeAndOtherErrorsKeepTheirMeaning() {
        XCTAssertTrue(WorkspaceBrowserSession.errorMessage(RelayError.rpc("unknown method: ReadWorkspaceFile")).contains("Update"))
        XCTAssertEqual(WorkspaceBrowserSession.errorMessage(RelayError.rpc("permission denied")), "permission denied")
        XCTAssertEqual(WorkspaceBrowserSession.errorMessage(RelayError.rpc("unknown file")), "unknown file")
    }

    func testFullContextRejectsStaleBinaryAndPartialSources() throws {
        let valid = WorkspaceDiffSources(diffChecksum: "sha", oldText: "old", newText: "new",
            binary: false, truncated: false, stale: false)
        XCTAssertEqual(try valid.validated(checksum: "sha").newText, "new")
        XCTAssertThrowsError(try valid.validated(checksum: "different"))
        for (binary, truncated, stale) in [(true, false, false), (false, true, false), (false, false, true)] {
            let invalid = WorkspaceDiffSources(diffChecksum: "sha", oldText: "old", newText: "new",
                binary: binary, truncated: truncated, stale: stale)
            XCTAssertThrowsError(try invalid.validated(checksum: "sha"))
        }
        let huge = WorkspaceDiffSources(diffChecksum: "sha",
            oldText: String(repeating: "x", count: 512 * 1024), newText: "new",
            binary: false, truncated: false, stale: false)
        XCTAssertThrowsError(try huge.validated(checksum: "sha"))
    }
}

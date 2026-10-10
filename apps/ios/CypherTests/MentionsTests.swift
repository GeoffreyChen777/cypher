import XCTest
import UIKit
import SwiftUI
import Loro
@testable import Cypher

/// `@` file and session mentions — the desktop composer's markup, chip
/// projection, token and candidate rules (composer.rs test vectors), the
/// reference snapshot (side_chats.rs) and the editor's atomic chips.
@MainActor
final class MentionsTests: XCTestCase {
    // MARK: Markup

    func testFileLinksSerializeToStrictLocalMarkdown() {
        let raw = Mentions.fileLink(path: "src/a file#[x].rs", isDir: false)
        XCTAssertEqual(raw, "[a file#\\[x\\].rs](cypher-file:src/a%20file%23%5Bx%5D.rs)")
        let links = Mentions.links(in: raw)
        XCTAssertEqual(links.count, 1)
        XCTAssertEqual(links.first?.kind, .file(path: "src/a file#[x].rs", isDir: false))
        XCTAssertEqual(links.first?.label, "a file#[x].rs")
        XCTAssertEqual(links.first?.range, NSRange(location: 0, length: raw.utf16.count))

        let folder = Mentions.fileLink(path: "src/components", isDir: true)
        XCTAssertEqual(folder, "[components](cypher-file:src/components/)")
        XCTAssertEqual(Mentions.links(in: folder).first?.kind, .file(path: "src/components", isDir: true))
        XCTAssertEqual(Mentions.fileLink(path: "docs/中文.md", isDir: false),
                       "[中文.md](cypher-file:docs/%E4%B8%AD%E6%96%87.md)")
        XCTAssertEqual(Mentions.links(in: Mentions.fileLink(path: "docs/中文.md", isDir: false)).first?.kind,
                       .file(path: "docs/中文.md", isDir: false))
    }

    func testFileLinksRejectExternalOrNoncanonicalMarkdown() {
        for raw in ["[composer.rs](other-file:src/composer.rs)", "[site](https://example.com/a)",
                    "[a.rs](../a.rs)", "[other](cypher-file:src/a.rs)", "[a.rs](cypher-file:/etc/a.rs)",
                    "[a.rs](cypher-file:src/../a.rs)", "[a.rs](cypher-file:src%5Cfake%5Ca.rs)",
                    "[a.rs](cypher-file:src/a%0A.rs)", "[a.rs](cypher-file:src/%61.rs)"] {
            XCTAssertTrue(Mentions.links(in: raw).isEmpty, raw)
        }
    }

    func testSessionLinksRoundTripAndEscapeTitles() {
        let raw = Mentions.sessionLink(title: "Fix the flaky test", chatId: "chat-123")
        XCTAssertEqual(raw, "[Fix the flaky test](cypher-session:chat-123)")
        XCTAssertEqual(Mentions.links(in: raw).first?.kind, .session(chatId: "chat-123"))

        let tricky = "A [weird] \\ title"
        let escaped = Mentions.sessionLink(title: tricky, chatId: "chat-456")
        XCTAssertEqual(escaped, "[A \\[weird\\] \\\\ title](cypher-session:chat-456)")
        XCTAssertEqual(Mentions.links(in: escaped).first?.label, tricky)
    }

    func testSessionLinksRejectHostileOrNoncanonicalTargets() {
        for raw in ["[t](cypher-session:)", "[t](cypher-session:%63hat-1)", "[t](cypher-session:chat%0A-1)",
                    "[t](cypher-session:%20%20)", "[t](cypher-session:chat%20id)",
                    "[a\nb](cypher-session:chat-1)", "[a\u{0007}b](cypher-session:chat-1)",
                    "[\(String(repeating: "w", count: Mentions.maxSessionLabelChars + 10))](cypher-session:chat-1)"] {
            XCTAssertTrue(Mentions.links(in: raw).isEmpty, raw)
        }
        let hugeId = "chat-" + String(repeating: "x", count: Mentions.maxSessionIdChars + 10)
        XCTAssertTrue(Mentions.links(in: Mentions.sessionLink(title: "t", chatId: hugeId)).isEmpty)
        let title = String(repeating: "t", count: Mentions.maxSessionTitleChars)
        XCTAssertEqual(Mentions.links(in: Mentions.sessionLink(title: title, chatId: "chat-1")).first?.label, title)
    }

    func testIssueLinksAreRecognizedForDisplay() {
        let raw = "[#482 Login \\[loops\\]](cypher-issue:GeoffreyChen777/cypher/482)"
        XCTAssertEqual(Mentions.links(in: raw).first?.kind,
                       .issue(repo: "GeoffreyChen777/cypher", number: 482, pull: false))
        XCTAssertEqual(Mentions.project(raw).display, "\u{a0}#482\u{a0}Login\u{a0}[loops]\u{a0}")
        XCTAssertTrue(Mentions.links(in: "[#48 x](cypher-issue:o/r/482)").isEmpty)
        XCTAssertTrue(Mentions.links(in: "[#482](cypher-pr:o/r/0482)").isEmpty)
    }

    func testSessionRefsDedupeInMentionOrderAndCapAtThree() {
        let raw = [Mentions.sessionLink(title: "one", chatId: "a"), Mentions.sessionLink(title: "two", chatId: "b"),
                   Mentions.sessionLink(title: "one again", chatId: "a")].joined(separator: " ")
        XCTAssertEqual(Mentions.sessionRefIds(in: raw), ["a", "b"])
        XCTAssertFalse(Mentions.sessionCapReached(existing: ["a", "b"], candidate: "c"))
        XCTAssertTrue(Mentions.sessionCapReached(existing: ["a", "b", "c"], candidate: "d"))
        XCTAssertFalse(Mentions.sessionCapReached(existing: ["a", "b", "c"], candidate: "b"))
    }

    // MARK: Display

    func testProjectionShowsChipsAndKeepsTheRestVerbatim() {
        let file = Mentions.fileLink(path: "src/main.rs", isDir: false)
        let session = Mentions.sessionLink(title: "Fix build", chatId: "chat-1")
        let raw = "Compare 中文 \(file) with \(session)!"
        let projection = Mentions.project(raw)
        XCTAssertEqual(projection.display, "Compare 中文 \u{a0}@main.rs\u{a0} with \u{a0}@Fix\u{a0}build\u{a0}!")
        XCTAssertEqual(projection.chips.map(\.text), ["\u{a0}@main.rs\u{a0}", "\u{a0}@Fix\u{a0}build\u{a0}"])
        let display = projection.display as NSString
        for chip in projection.chips {
            XCTAssertEqual(display.substring(with: chip.display), chip.text)
            XCTAssertEqual((raw as NSString).substring(with: chip.link.range),
                           chip.link.kind == .session(chatId: "chat-1") ? session : file)
        }
        XCTAssertEqual(Mentions.project("plain text").display, "plain text")
    }

    func testDuplicateBasenamesUseUniquePathSuffixes() {
        let raw = [Mentions.fileLink(path: "src/one/mod.rs", isDir: false),
                   Mentions.fileLink(path: "src/two/mod.rs", isDir: false),
                   Mentions.fileLink(path: "bar/oomod.rs", isDir: false)].joined(separator: " ")
        XCTAssertEqual(Mentions.displayLabels(Mentions.links(in: raw)), ["one/mod.rs", "two/mod.rs", "oomod.rs"])
    }

    func testSentMessagesRenderChipsAsInlineCode() {
        let raw = "see \(Mentions.fileLink(path: "a/b.rs", isDir: false)) now"
        XCTAssertEqual(Mentions.inlineRuns(raw), [
            InlineRun(text: "see ", style: .plain),
            InlineRun(text: "\u{a0}@b.rs\u{a0}", style: InlineStyle(code: true)),
            InlineRun(text: " now", style: .plain),
        ])
        XCTAssertEqual(Mentions.inlineRuns("[x](https://e.com)"), [InlineRun(text: "[x](https://e.com)", style: .plain)])
    }

    // MARK: Completion

    func testTokenRequiresABoundaryAndTracksTheWholeWord() {
        XCTAssertEqual(Mentions.token(in: "Fix @src/com", caret: 12),
                       MentionToken(range: NSRange(location: 4, length: 8), query: "src/com"))
        XCTAssertNil(Mentions.token(in: "mail@example.com", caret: 16))
        XCTAssertNil(Mentions.token(in: "word@file", caret: 9))
        XCTAssertNil(Mentions.token(in: "path/@file", caret: 10))
        XCTAssertEqual(Mentions.token(in: "See (@lib", caret: 9)?.range, NSRange(location: 5, length: 4))
        // Mid-word caret: the query is what's before it, the range the word.
        XCTAssertEqual(Mentions.token(in: "@compo rest", caret: 3),
                       MentionToken(range: NSRange(location: 0, length: 6), query: "co"))
        XCTAssertEqual(Mentions.token(in: "@", caret: 1), MentionToken(range: NSRange(location: 0, length: 1), query: ""))
        XCTAssertNil(Mentions.token(in: "@a b", caret: 4))
        // UTF-16 offsets past wide characters; a masked chip is no boundary.
        XCTAssertEqual(Mentions.token(in: "中文 @文件", caret: 6),
                       MentionToken(range: NSRange(location: 3, length: 3), query: "文件"))
        XCTAssertNil(Mentions.token(in: "\u{FFFC}\u{FFFC}@x", caret: 4))
        XCTAssertNil(Mentions.token(in: "👩🏽‍💻@x", caret: 1), "a caret inside a surrogate pair")
    }

    private func chat(_ id: String, title: String?, space: String? = "p1", device: String = "d1",
                      archived: Bool = false, at: Int64? = nil, child: Bool = false) -> Chat {
        Chat(id: id, deviceId: device, title: title, archived: archived, cwd: nil, branch: nil,
             checkoutId: nil, config: nil, lastMessagePreview: nil, lastMessageAt: at, createdAt: 0,
             spaceId: space, lastSeenAt: nil,
             child: child ? ChildChat(parentChatId: "x", parentRunId: "r", agent: "a", task: "t", mode: .sync) : nil)
    }

    func testSessionCandidatesExcludeCurrentAndChildrenAndRankNearestRecentFirst() {
        let chats = [
            chat("current", title: "Current", at: 100),
            chat("child", title: "Child", at: 100, child: true),
            chat("archived", title: "Old thing", archived: true, at: 999),
            chat("same-project-old", title: "Beta", at: 10),
            chat("same-project-new", title: "Alpha", at: 50),
            chat("same-device", title: "Gamma", space: "p2", at: 900),
            chat("elsewhere", title: "Delta", space: "p3", device: "d2", at: 950),
        ]
        let ids = Mentions.sessionCandidates(chats, query: "", currentChat: "current", project: "p1", device: "d1")
            .map(\.chatId)
        XCTAssertEqual(ids, ["same-project-new", "same-project-old", "same-device", "elsewhere"])
        // Archived sessions join once something is typed, after live ones.
        XCTAssertEqual(Mentions.sessionCandidates(chats, query: "THING", currentChat: "current",
                                                  project: "p1", device: "d1").map(\.chatId), ["archived"])
        XCTAssertEqual(Mentions.sessionCandidates(chats, query: "a", currentChat: "current",
                                                  project: "p1", device: "d1").last?.chatId, "archived")
        let many = (0..<12).map { chat("c\($0)", title: "S\($0)", at: Int64($0)) }
        XCTAssertEqual(Mentions.sessionCandidates(many, query: "", currentChat: nil, project: "p1", device: "d1").count,
                       Mentions.maxSessionCandidates)
    }

    func testSessionTitleFallsBackAndCaps() {
        var row = chat("a", title: "  ")
        row.lastMessagePreview = "preview text"
        XCTAssertEqual(Mentions.sessionTitle(row), "preview text")
        row.lastMessagePreview = nil
        XCTAssertEqual(Mentions.sessionTitle(row), "Untitled session")
        row.title = String(repeating: "长", count: 70)
        XCTAssertEqual(Mentions.sessionTitle(row), String(repeating: "长", count: 60) + "…")
    }

    // MARK: Reference prompt

    private func entry(_ id: String, _ role: MessageRole, _ lines: [String]) -> ReferenceEntry {
        ReferenceEntry(id: id, role: role, lines: lines)
    }

    func testBoundedContextKeepsTheNewestWholeMessages() {
        let entries = (0..<12).map { entry("m\($0)", .user, ["msg \($0)"]) }
        let context = try? XCTUnwrap(SessionReferences.boundedContext(entries))
        XCTAssertEqual(context, (4..<12).map { "user: msg \($0)" }.joined(separator: "\n\n"))
        XCTAssertNil(SessionReferences.boundedContext([entry("a", .user, ["  "])]))

        let big = String(repeating: "x", count: 30 * 1024)
        XCTAssertEqual(SessionReferences.boundedContext([entry("a", .user, [big]), entry("b", .assistant, [big])]),
                       "assistant: " + big, "older messages drop whole")
        let huge = String(repeating: "y", count: 60 * 1024)
        XCTAssertEqual(SessionReferences.boundedContext([entry("a", .assistant, [huge])])?.unicodeScalars.count,
                       SessionReferences.maxContextChars, "a lone newest message keeps its head")
    }

    func testDocEntriesReadAgentWordsJoinContinuationsAndDropAttachmentPaths() throws {
        let json: [String: Any] = ["messages": [
            ["id": "u1", "role": "user", "parts": [
                ["id": "t0", "kind": "text",
                 "text": "look\n\nAttached images (local files — open them to view):\n- /Users/me/.cypher/a.png"],
            ]],
            ["id": "a1", "role": "assistant", "parts": [
                ["id": "t0", "kind": "text", "text": "译文", "agentText": "original"],
                ["id": "x", "kind": "tool", "call": ["kind": "readFile"], "isError": true],
            ]],
            ["id": "a2", "role": "assistant", "continuationOf": "a1", "parts": [
                ["id": "e", "kind": "error", "message": " boom "],
                ["id": "q", "kind": "input", "questions": [
                    ["id": "q1", "header": "Go", "question": "Proceed?", "options": ["Yes", "No"]],
                ]],
            ]],
        ]]
        let root = try XCTUnwrap(LoroValue.fromJSON(json).mapValue)
        let entries = SessionReferences.entries(root: root)
        XCTAssertEqual(entries.map(\.id), ["u1", "a1"])
        XCTAssertEqual(SessionReferences.boundedContext(entries),
                       "user: look\n\nassistant: original\n[tool: read-file failed]\n[error: boom]\n[question: Proceed?]")
    }

    func testAgentPromptWrapsSessionsThenCommentsAroundTheProjectedRequest() throws {
        let file = Mentions.fileLink(path: "src/main.rs", isDir: false)
        let session = Mentions.sessionLink(title: "A \"quoted\" title", chatId: "chat-1")
        let hostile = "[bad](cypher-session:%63hat-2)"
        let visible = "before \(session); file \(file); literal \(hostile); after"
        XCTAssertEqual(SessionReferences.projectForAgent(visible),
                       "before @Session \"A \\\"quoted\\\" title\" (snapshot included above); file [main.rs](cypher-file:src/main.rs); literal [bad](cypher-session:%63hat-2); after")

        let prompt = try XCTUnwrap(SessionReferences.agentPrompt(
            sessions: [SessionReference(title: "Fix build", context: "user: broke\nassistant: fixed")],
            comments: [DraftComment(quote: "q", comment: "note")], visible: "go \(session)"))
        let blocks = prompt.components(separatedBy: "\n\nUser request:\n")
        XCTAssertEqual(blocks.count, 2)
        let lines = blocks[0].components(separatedBy: "\n\n")
        XCTAssertEqual(lines.count, 2)
        XCTAssertEqual(lines[0], SessionReferences.lead
                       + " {\"sessions\":[{\"title\":\"Fix build\",\"transcript\":\"user: broke\\nassistant: fixed\"}]}")
        XCTAssertTrue(lines[1].hasPrefix("Conversation annotations (JSON):"))
        XCTAssertEqual(blocks[1], "go @Session \"A \\\"quoted\\\" title\" (snapshot included above)")

        // Without sessions the comments-only envelope is unchanged.
        XCTAssertEqual(try SessionReferences.agentPrompt(sessions: [], comments: [DraftComment(quote: "q", comment: "n")],
                                                         visible: "v"),
                       try CommentPrompt.agentPrompt([DraftComment(quote: "q", comment: "n")], visible: "v"))
        XCTAssertNil(try SessionReferences.agentPrompt(sessions: [], comments: [], visible: "v"))
    }

    func testReferenceBlockDegradesTheOldestToTitleStubsOverBudget() {
        let big = String(repeating: "x", count: 45 * 1024)
        let block = SessionReferences.block([SessionReference(title: "first", context: big),
                                             SessionReference(title: "second", context: big),
                                             SessionReference(title: "third", context: big)])
        XCTAssertLessThanOrEqual(block.unicodeScalars.count, SessionReferences.maxReferenceChars)
        XCTAssertTrue(block.hasPrefix("{\"sessions\":[{\"title\":\"first\"},{\"title\":\"second\",\"transcript\":"))
        XCTAssertTrue(block.contains("{\"title\":\"third\",\"transcript\":"))
    }

    func testSendValidationRejectsCurrentChatSideChatsUnknownAndTooMany() {
        let chats = [chat("a", title: "A"), chat("side", title: "S", child: true), chat("b", title: "B"),
                     chat("c", title: "C"), chat("d", title: "D")]
        XCTAssertNil(SessionReferences.validationError(refs: ["a", "b"], currentChat: "x", chats: chats))
        XCTAssertNotNil(SessionReferences.validationError(refs: ["a"], currentChat: "a", chats: chats))
        XCTAssertNotNil(SessionReferences.validationError(refs: ["side"], currentChat: nil, chats: chats))
        XCTAssertNotNil(SessionReferences.validationError(refs: ["gone"], currentChat: nil, chats: chats))
        XCTAssertNotNil(SessionReferences.validationError(refs: ["a", "b", "c", "d"], currentChat: nil, chats: chats))
    }

    // MARK: Editor

    private func editor(_ raw: String) -> (UITextView, ComposerTextInput.Coordinator, () -> String) {
        var text = raw
        let binding = Binding(get: { text }, set: { text = $0 })
        let coordinator = ComposerTextInput.Coordinator(text: binding, focus: ComposerFocus())
        coordinator.attach()
        let view = UITextView()
        view.delegate = coordinator
        coordinator.render(raw, in: view)
        return (view, coordinator, { text })
    }

    func testTheEditorShowsChipsAndSerializesBackToRawMarkup() {
        let file = Mentions.fileLink(path: "src/main.rs", isDir: false)
        let (view, coordinator, _) = editor("open \(file) please")
        XCTAssertEqual(view.text, "open \u{a0}@main.rs\u{a0} please")
        XCTAssertEqual(coordinator.raw(of: view), "open \(file) please")
    }

    func testBackspaceAtAChipsEndDeletesTheWholeChip() {
        let file = Mentions.fileLink(path: "src/main.rs", isDir: false)
        let (view, coordinator, text) = editor("open \(file) please")
        let chipEnd = ("open \u{a0}@main.rs\u{a0}" as NSString).length
        let allowed = coordinator.textView(view, shouldChangeTextIn: NSRange(location: chipEnd - 1, length: 1),
                                           replacementText: "")
        XCTAssertFalse(allowed)
        XCTAssertEqual(text(), "open  please")
        XCTAssertEqual(view.text, "open  please")
        XCTAssertEqual(view.selectedRange, NSRange(location: 5, length: 0))
    }

    func testKeyboardBackspacesTakeTheSpaceThenTheWholeChip() {
        let session = Mentions.sessionLink(title: "Model picker", chatId: "c1")
        let (view, coordinator, text) = editor("with @mod")
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 200))
        view.frame = window.bounds
        window.addSubview(view)
        window.makeKeyAndVisible()
        XCTAssertTrue(view.becomeFirstResponder())
        view.selectedRange = NSRange(location: 9, length: 0)
        coordinator.splice(view, token: MentionToken(range: NSRange(location: 5, length: 4), query: "mod"), link: session)
        XCTAssertEqual(text(), "with \(session) ")
        view.deleteBackward()
        XCTAssertEqual(text(), "with \(session)")
        view.deleteBackward()
        XCTAssertEqual(text(), "with ")
        XCTAssertEqual(view.text, "with ")
        view.resignFirstResponder()
    }

    func testTypingInsideAChipLandsAfterItAndPlainEditsPassThrough() {
        let session = Mentions.sessionLink(title: "Fix", chatId: "c1")
        let (view, coordinator, text) = editor("\(session)")
        XCTAssertFalse(coordinator.textView(view, shouldChangeTextIn: NSRange(location: 2, length: 0),
                                            replacementText: "x"))
        XCTAssertEqual(text(), "\(session)x")
        XCTAssertTrue(coordinator.textView(view, shouldChangeTextIn: NSRange(location: view.textStorage.length, length: 0),
                                           replacementText: "y"))
    }

    func testPickingARowReplacesTheTokenWithTheLinkAndASpace() {
        let (view, coordinator, text) = editor("see @mai")
        view.selectedRange = NSRange(location: 8, length: 0)
        let token = MentionToken(range: NSRange(location: 4, length: 4), query: "mai")
        let link = Mentions.fileLink(path: "src/main.rs", isDir: false)
        coordinator.splice(view, token: token, link: link)
        XCTAssertEqual(text(), "see \(link) ")
        XCTAssertEqual(view.text, "see \u{a0}@main.rs\u{a0} ")
        XCTAssertEqual(view.selectedRange.location, view.textStorage.length)

        // An existing separator is reused, and the caret parks past it.
        let (other, otherCoordinator, otherText) = editor("@mai rest")
        other.selectedRange = NSRange(location: 4, length: 0)
        otherCoordinator.splice(other, token: MentionToken(range: NSRange(location: 0, length: 4), query: "mai"),
                                link: link)
        XCTAssertEqual(otherText(), "\(link) rest")
        XCTAssertEqual(other.selectedRange.location, ("\u{a0}@main.rs\u{a0} " as NSString).length)

        // A stale token (the caret moved on) splices nothing.
        let (stale, staleCoordinator, staleText) = editor("@mai rest")
        stale.selectedRange = NSRange(location: 9, length: 0)
        staleCoordinator.splice(stale, token: MentionToken(range: NSRange(location: 0, length: 4), query: "mai"),
                                link: link)
        XCTAssertEqual(staleText(), "@mai rest")
    }
}

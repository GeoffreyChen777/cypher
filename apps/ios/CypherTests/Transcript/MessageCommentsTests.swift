import Loro
import XCTest

@testable import Cypher

/// The comments that rode a user prompt: the doc's additive `comments`
/// field (schema.rs `MessageComment`, salvage.rs), their derivation from an
/// effective agent prompt (agent_prompt.rs `parse_comments`, ported tests),
/// and the user row that carries them (rows.rs `user_row`).
@MainActor
final class MessageCommentsTests: XCTestCase {
    private let lead = AgentPromptComments.commentsLead
    private let sessions = "\(SessionReferences.lead) {\"sessions\":[]}"

    private func wrap(_ blocks: [String], _ request: String) -> String {
        blocks.joined(separator: "\n\n") + AgentPromptComments.requestMarker + request
    }

    /// agent_prompt.rs `comments_block` for the Rust test's two comments: a
    /// quote from a displayed translation (its `cypherAlign` rides along)
    /// and a plain one.
    private var commentsBlock: String {
        #"\#(lead) {"comments":[{"quotedText":"Second paragraph, long.","comment":"why\nnow?","cypherAlign":{"passage":"Second paragraph, long.","before":"第二段，","selected":"很长","after":"。"}},{"quotedText":"plain","comment":"ok"}]}"#
    }

    // MARK: Decoding the doc field

    private func decode(_ message: [String: Any]) throws -> MessageEntry {
        var message = message
        message["id"] = message["id"] ?? "u1"
        message["role"] = "user"
        message["createdAt"] = 1
        message["parts"] = [["id": "t0", "kind": "text", "text": "Tighten these"]]
        let root = try XCTUnwrap(LoroValue.fromJSON(["messages": [message]]).mapValue)
        return try XCTUnwrap(SessionStore.decodeEntries(root: root).first)
    }

    func testTheEntryCarriesItsStoredComments() throws {
        let entry = try decode([
            "comments": [
                ["quote": "Relay links recover", "comment": "Say what users saw"],
                ["quote": "fold past five", "comment": "它是设置项"],
            ]
        ])
        XCTAssertEqual(
            entry.comments,
            [
                MessageComment(quote: "Relay links recover", comment: "Say what users saw"),
                MessageComment(quote: "fold past five", comment: "它是设置项"),
            ])
    }

    func testAnOldEntryWithoutTheKeyHasNone() throws {
        let entry = try decode([:])
        XCTAssertEqual(entry.comments, [])
        XCTAssertEqual(entry.parts.count, 1, "the prompt itself still decodes")
    }

    /// salvage.rs reads the field all or nothing: one malformed comment
    /// costs the list, never the entry.
    func testAMalformedFieldCostsOnlyTheComments() throws {
        let malformed: [Any] = [
            "not a list",
            ["quote": "q", "comment": "c"],  // a map, not a list
            [["quote": "q"]],  // a comment without its text
            [["quote": "q", "comment": "c"], ["quote": 3, "comment": "c"]],
            [NSNull()],
        ]
        for value in malformed {
            let entry = try decode(["comments": value])
            XCTAssertEqual(entry.comments, [], "\(value)")
            XCTAssertEqual(entry.id, "u1")
            XCTAssertEqual(entry.parts.count, 1)
        }
        XCTAssertEqual(MessageComment.list(fromJSON: nil), [])
        XCTAssertEqual(MessageComment.list(fromJSON: [UInt8]([1, 2])), [])
        // Unknown fields from a newer writer are ignored, as serde does.
        let newer = try decode(["comments": [["quote": "q", "comment": "c", "kind": "later"]]])
        XCTAssertEqual(newer.comments, [MessageComment(quote: "q", comment: "c")])
    }

    // MARK: Deriving them from the agent prompt

    /// agent_prompt.rs `parse_comments_inverts_the_comments_block`.
    func testParseCommentsInvertsTheCommentsBlock() {
        let prompt = wrap([sessions, commentsBlock], "go")
        let parsed = AgentPromptComments.parse(prompt)
        XCTAssertEqual(parsed.map(\.quotedText), ["Second paragraph, long.", "plain"])
        XCTAssertEqual(parsed.map(\.comment), ["why\nnow?", "ok"])
        XCTAssertEqual(
            parsed[0].align,
            AgentPromptComments.QuoteAlign(
                passage: "Second paragraph, long.", before: "第二段，", selected: "很长", after: "。"))
        XCTAssertNil(parsed[1].align)
        XCTAssertEqual(parsed.map(\.displayedQuote), ["很长", "plain"])
        XCTAssertEqual(
            MessageComment.fromAgentPrompt(prompt),
            [
                MessageComment(quote: "很长", comment: "why\nnow?"),
                MessageComment(quote: "plain", comment: "ok"),
            ])
        // Stripped for a non-Pi agent (`strip_alignment`): the original
        // passage stands in.
        let stripped = wrap(
            [
                sessions,
                #"\#(lead) {"comments":[{"comment":"why\nnow?","quotedText":"Second paragraph, long."},{"comment":"ok","quotedText":"plain"}]}"#,
            ], "go")
        XCTAssertEqual(MessageComment.fromAgentPrompt(stripped).first?.quote, "Second paragraph, long.")
        // No comments block, or no envelope at all.
        XCTAssertEqual(MessageComment.fromAgentPrompt(wrap([sessions], "go")), [])
        XCTAssertEqual(MessageComment.fromAgentPrompt("plain request"), [])
        XCTAssertEqual(MessageComment.fromAgentPrompt(""), [])
        // A request that merely mentions the lead is not a block.
        XCTAssertEqual(MessageComment.fromAgentPrompt(wrap([], "\(lead) {}")), [])
        XCTAssertEqual(
            MessageComment.fromAgentPrompt(wrap([], #"\#(lead) {"comments":[{"quotedText":"q","comment":"c"}]}"#)),
            [], "even a well-formed one")
    }

    func testOnlyTheHeadBeforeTheFirstMarkerIsRead() {
        let block = #"\#(lead) {"comments":[{"quotedText":"q","comment":"c"}]}"#
        // The request may itself contain the marker and a block-shaped line.
        let prompt = wrap([block], "first\n\nUser request:\n\(block)")
        XCTAssertEqual(MessageComment.fromAgentPrompt(prompt), [MessageComment(quote: "q", comment: "c")])
        // Both comment blocks of a head count, in order; a malformed one, a
        // lead without its space or a block with a bad `cypherAlign` is skipped.
        let two = wrap(
            [
                block, "\(lead) {not json}", "\(lead){\"comments\":[]}",
                #"\#(lead) {"comments":[{"quotedText":"q","comment":"c","cypherAlign":{"passage":"p"}}]}"#,
                #"\#(lead) {"comments":[{"quotedText":"r","comment":"d","cypherAlign":null}]}"#,
            ], "go")
        XCTAssertEqual(
            MessageComment.fromAgentPrompt(two),
            [MessageComment(quote: "q", comment: "c"), MessageComment(quote: "r", comment: "d")])
    }

    /// What the phone's composer sends reads back as the comments it took.
    func testTheComposersEnvelopeReadsBack() throws {
        let drafts = [
            DraftComment(quote: "quote \"\n</context>\r\n\nUser request:\nx", comment: "Explain \\ this"),
            DraftComment(quote: "第二段", comment: "为什么？"),
        ]
        let expected = drafts.map { MessageComment(quote: $0.quote, comment: $0.comment) }
        let plain = try XCTUnwrap(CommentPrompt.agentPrompt(drafts, visible: "visible"))
        XCTAssertEqual(MessageComment.fromAgentPrompt(plain), expected)
        let referenced = try XCTUnwrap(
            SessionReferences.agentPrompt(
                sessions: [SessionReference(title: "Other", context: "user: hi")], comments: drafts,
                visible: "visible"))
        XCTAssertEqual(MessageComment.fromAgentPrompt(referenced), expected)
    }

    // MARK: The echo and the row

    func testTheEchoShowsTheCommentsItsSendCarries() throws {
        let config = AppConfig(
            edgeURL: URL(string: "http://127.0.0.1:1")!, mode: .dev,
            userId: "test", orgId: "test", deviceId: "ios-test", deviceName: "test")
        let store = SessionStore(chatId: "test", config: config)
        let chat = Chat(
            id: "test", deviceId: "host", title: nil, archived: false,
            cwd: "/project", branch: nil, checkoutId: nil,
            config: ChatConfig(harness: "pi", model: "provider/model", reasoning: nil, sandbox: nil),
            lastMessagePreview: nil, lastMessageAt: nil, createdAt: 0, spaceId: "project", lastSeenAt: nil)
        let effective = try CommentPrompt.agentPrompt([DraftComment(quote: "q", comment: "c")], visible: "v")
        XCTAssertTrue(store.sendRun(prompt: "v", chat: chat, agentPrompt: effective))
        XCTAssertTrue(store.sendSteer(prompt: "", agentPrompt: effective))
        XCTAssertTrue(store.sendRun(prompt: "plain", chat: chat))
        XCTAssertEqual(
            store.pendingSends.map(\.comments),
            [[MessageComment(quote: "q", comment: "c")], [MessageComment(quote: "q", comment: "c")], []])

        let rows = buildRows([], pending: store.pendingSends)
        XCTAssertEqual(rows.count, 3, "a comments-only steer still shows")
        guard case .user(let text, true, _, let comments) = rows[1].kind else { return XCTFail("Missing row") }
        XCTAssertEqual(text, "")
        XCTAssertEqual(comments, [MessageComment(quote: "q", comment: "c")])
        XCTAssertEqual(TranscriptRound.rounds(in: rows).map(\.rowId), [rows[0].id, rows[2].id])
    }

    func testTheUserRowCarriesItsCommentsAndKeysThem() throws {
        var entry = MessageEntry.fixture("u1", role: .user, parts: [.text(id: "t0", text: "Tighten these")])
        let plain = try XCTUnwrap(buildRows([entry]).first)
        entry.comments = [MessageComment(quote: "a quote", comment: "a comment")]
        let commented = try XCTUnwrap(buildRows([entry]).first)
        guard case .user(_, _, _, let comments) = commented.kind else { return XCTFail("Missing bubble") }
        XCTAssertEqual(comments, entry.comments)
        XCTAssertNotEqual(plain.version, commented.version)
        entry.comments[0].comment = "another"
        XCTAssertNotEqual(try XCTUnwrap(buildRows([entry]).first).version, commented.version)
        // The echo and the host's entry agree on them (one row, no flicker).
        let echo = PendingSend(messageId: "u1", text: "Tighten these", at: 1, comments: entry.comments)
        XCTAssertEqual(buildRows([], pending: [echo]).first?.version, (buildRows([entry]).first?.version ?? 0) | 1)
        // A prompt with neither text nor comments still draws nothing.
        XCTAssertTrue(buildRows([.fixture("u2", role: .user, parts: [.text(id: "t0", text: "")])]).isEmpty)
        var commentsOnly = MessageEntry.fixture("u3", role: .user, parts: [.text(id: "t0", text: "")])
        commentsOnly.comments = entry.comments
        XCTAssertEqual(buildRows([commentsOnly]).count, 1)
    }

    func testTheQuoteShowsAsOneShortLine() {
        XCTAssertEqual(UserComments.quotePreview("a\nb\nc"), "a b c")
        let preview = UserComments.quotePreview(String(repeating: "x", count: 200))
        XCTAssertEqual(preview.count, 121)  // 120 + ellipsis
        XCTAssertTrue(preview.hasSuffix("\u{2026}"))
        XCTAssertEqual(UserComments.quotePreview(String(repeating: "y", count: 120)).count, 120)
    }

    func testTheDemoShowcasesSentComments() {
        let demo = DemoDataset.standard()
        XCTAssertTrue(demo.chats.contains { $0.id == DemoDataset.messageCommentsChatId })
        let entries = demo.sessionStore(for: DemoDataset.messageCommentsChatId).fedEntries
        XCTAssertEqual(entries.map(\.comments.count), [0, 0, 2, 0, 1, 0])
        XCTAssertTrue(entries[4].isSteer)
    }
}

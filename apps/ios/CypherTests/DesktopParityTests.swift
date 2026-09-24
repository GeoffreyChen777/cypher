import XCTest
@testable import Cypher

/// Slash commands, the context ring, rename/delete, quick chat, fork and side
/// chats — the desktop features the phone gained together.
@MainActor
final class DesktopParityTests: XCTestCase {
    // MARK: Slash commands

    func testSlashMenuBelongsToTheCommandNameOnly() {
        XCTAssertEqual(SlashMenu.query(in: "/"), "")
        XCTAssertEqual(SlashMenu.query(in: "/co"), "co")
        XCTAssertNil(SlashMenu.query(in: "/goal ship it"), "arguments close the menu")
        XCTAssertNil(SlashMenu.query(in: "/Users/dev"), "a typed path isn't a command")
        XCTAssertNil(SlashMenu.query(in: " /co"))
        XCTAssertNil(SlashMenu.query(in: "hello /co"))
    }

    func testSlashFilterRanksPrefixBeforeSubstringAndHidesPlumbing() {
        let commands = ["export-html", "compact", "skill:x", "mcp", "goal", "recompact"].map {
            SlashCommand(name: $0)
        }
        XCTAssertEqual(SlashMenu.filter(commands, query: "").map(\.name),
                       ["export-html", "compact", "goal", "recompact"])
        XCTAssertEqual(SlashMenu.filter(commands, query: "COMP").map(\.name), ["compact", "recompact"])
        XCTAssertEqual(SlashMenu.filter(commands, query: "zzz"), [])
        for hidden in ["skill:a", "llama-x", "newapi-x", "compact-ui", "mcp", "mcp-add", "pi-mcp-x"] {
            XCTAssertTrue(SlashMenu.hiddenByDefault(hidden), hidden)
        }
        XCTAssertFalse(SlashMenu.hiddenByDefault("compact"))
        XCTAssertEqual(SlashMenu.accept(SlashCommand(name: "goal")), "/goal ")
    }

    func testSlashCommandDecodesTheWireShape() throws {
        let json = #"[{"name":"compact","description":"Compact","inputHint":"custom instructions"},{"name":"x"}]"#
        let decoded = try JSONDecoder().decode([SlashCommand].self, from: Data(json.utf8))
        XCTAssertEqual(decoded[0].detail, "Compact · custom instructions")
        XCTAssertNil(decoded[1].detail)
    }

    func testSlashErrorsReadLikeTheDesktop() {
        XCTAssertTrue(SlashMenu.errorMessage(RelayError.rpc("unknown method: ListCommands")).contains("older Cypher"))
        XCTAssertEqual(SlashMenu.errorMessage(RelayError.hostOffline), "The session's device is unreachable")
        XCTAssertEqual(SlashMenu.errorMessage(RelayError.rpc("boom")), "Couldn't load this agent's commands")
    }

    // MARK: Context ring

    func testTokenCountsMatchTheDesktop() {
        // context_ring.rs test vectors.
        XCTAssertEqual(ContextUsage.formatTokens(950), "950")
        XCTAssertEqual(ContextUsage.formatTokens(1_000), "1k")
        XCTAssertEqual(ContextUsage.formatTokens(9_540), "9.5k")
        XCTAssertEqual(ContextUsage.formatTokens(124_400), "124k")
        XCTAssertEqual(ContextUsage.formatTokens(200_000), "200k")
        XCTAssertEqual(ContextUsage.formatTokens(1_000_000), "1M")
        XCTAssertEqual(ContextUsage.formatTokens(1_250_000), "1.2M")
        XCTAssertEqual(ContextUsage(used: 124_000, size: 200_000).summary, "62% context used · 124k / 200k")
        XCTAssertEqual(ContextUsage(used: 250_000, size: 200_000).fraction, 1)
        XCTAssertEqual(ContextUsage(used: 5, size: 0).fraction, 0)
    }

    func testContextUsageDecodesLeniently() {
        XCTAssertEqual(ContextUsage(.object(["used": .int(10), "size": .double(20)])),
                       ContextUsage(used: 10, size: 20))
        XCTAssertNil(ContextUsage(.object(["used": .string("x"), "size": .int(20)])))
        XCTAssertNil(ContextUsage(nil))
        XCTAssertNil(ContextUsage(.null))
    }

    // MARK: Rename / delete / quick chat

    func testRenameTrimsAndIgnoresBlank() {
        let model = AppModel()
        model.enterDemoMode()
        model.renameChat(chatId: "chat-tabs", title: "  Header colors  ")
        XCTAssertEqual(model.chat(id: "chat-tabs")?.title, "Header colors")
        model.renameChat(chatId: "chat-tabs", title: "   ")
        XCTAssertEqual(model.chat(id: "chat-tabs")?.title, "Header colors")
    }

    func testDeleteTakesTheSubagentChildrenWithIt() async {
        let model = AppModel()
        model.enterDemoMode()
        let parent = try! XCTUnwrap(model.chat(id: "chat-veil"))
        XCTAssertNotNil(model.chat(id: "demo-child-planner"))
        let notice = await model.deleteChat(parent)
        XCTAssertNil(notice)
        XCTAssertNil(model.chat(id: "chat-veil"))
        XCTAssertNil(model.chat(id: "demo-child-planner"))
        XCTAssertNil(model.chat(id: "demo-child-reviewer"))
    }

    func testQuickChatIsIdentifiedByItsScratchFolder() {
        func chat(_ id: String, cwd: String?, spaceId: String? = nil) -> Chat {
            Chat(id: id, deviceId: "d", title: nil, archived: false, cwd: cwd, branch: nil,
                 checkoutId: nil, config: nil, lastMessagePreview: nil, lastMessageAt: nil,
                 createdAt: 0, spaceId: spaceId, lastSeenAt: nil)
        }
        XCTAssertTrue(chat("abc-1", cwd: "/tmp/cypher-scratch/abc-1").isScratch)
        XCTAssertTrue(chat("abc-1", cwd: "/var/folders/x/T/cypher-scratch/abc-1/").isScratch)
        XCTAssertFalse(chat("abc-1", cwd: "/tmp/cypher-scratch/other").isScratch)
        XCTAssertFalse(chat("abc-1", cwd: "/tmp/cypher-scratch/abc-1", spaceId: "s").isScratch)
        XCTAssertFalse(chat("abc-1", cwd: "/tmp/scratch/abc-1").isScratch)
        XCTAssertFalse(chat("abc-1", cwd: nil).isScratch)
    }

    func testQuickChatsAndProjectlessSessionsAreListedApart() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let config = ChatConfig(harness: "pi", model: "demo/pi", reasoning: nil, sandbox: "workspace-write")
        let quickId = try await model.createQuickChat(deviceId: "dev-mac", config: config)
        XCTAssertEqual(model.quickChats.map(\.id), [quickId])
        XCTAssertFalse(model.projectlessChats.contains { $0.id == quickId })
        // A session whose project row is gone is "other", not quick.
        var orphan = try XCTUnwrap(model.chat(id: "chat-tabs"))
        orphan.id = "chat-orphan"
        orphan.spaceId = "space-deleted"
        model.demo?.chats.append(orphan)
        XCTAssertEqual(model.projectlessChats.map(\.id), ["chat-orphan"])
        XCTAssertFalse(model.overviewChats.contains { $0.id == "chat-orphan" })
    }

    // MARK: Fork

    func testForkResponseDecodes() throws {
        let created = #"{"kind":"created","chat":{"id":"f1","title":"T — Fork","deviceId":"d"},"mode":"editUser","composerText":"redo"}"#
        XCTAssertEqual(try JSONDecoder().decode(ForkResponse.self, from: Data(created.utf8)),
                       .created(chatId: "f1", title: "T — Fork", composerText: "redo"))
        let plain = #"{"kind":"created","chat":{"id":"f2"},"mode":"continueAfterAssistant"}"#
        XCTAssertEqual(try JSONDecoder().decode(ForkResponse.self, from: Data(plain.utf8)),
                       .created(chatId: "f2", title: nil, composerText: nil))
        let refused = #"{"kind":"unavailable","reason":"liveSession","message":"Wait for the run to finish."}"#
        XCTAssertEqual(try JSONDecoder().decode(ForkResponse.self, from: Data(refused.utf8)),
                       .unavailable(message: "Wait for the run to finish."))
    }

    func testForkBeforeAPromptHandsItBackAndAfterAReplyKeepsIt() async throws {
        let model = AppModel()
        model.enterDemoMode()
        let source = try XCTUnwrap(model.chat(id: "chat-tabs"))
        let entries = try XCTUnwrap(model.sessionStore(for: source)?.entries)
        let prompt = try XCTUnwrap(entries.last { $0.role == .user })
        let promptIx = try XCTUnwrap(entries.firstIndex { $0.id == prompt.id })

        guard case .created(let beforeId) = await model.forkSession(source, anchor: prompt) else {
            return XCTFail("fork failed")
        }
        XCTAssertEqual(model.sessionStore(for: try XCTUnwrap(model.chat(id: beforeId)))?.entries.count, promptIx)
        XCTAssertNotNil(model.takePendingDraft(chatId: beforeId))
        XCTAssertNil(model.takePendingDraft(chatId: beforeId), "the draft is handed over once")
        XCTAssertTrue(model.chat(id: beforeId)?.title?.hasSuffix("— Fork") == true)

        let reply = try XCTUnwrap(entries.last { $0.role == .assistant })
        let replyIx = try XCTUnwrap(entries.firstIndex { $0.id == reply.id })
        guard case .created(let afterId) = await model.forkSession(source, anchor: reply) else {
            return XCTFail("fork failed")
        }
        XCTAssertEqual(model.sessionStore(for: try XCTUnwrap(model.chat(id: afterId)))?.entries.count, replyIx + 1)
        XCTAssertNil(model.takePendingDraft(chatId: afterId))
    }

    func testTranscriptRowsCarryTheirEntrysRole() {
        var parsers: [String: IncrementalMarkdownParser] = [:]
        var completed: [String: CompletedParse] = [:]
        let entries = [
            MessageEntry(id: "u", role: .user, parts: [.text(id: "t", text: "hi")], createdAt: 1,
                         deviceId: "d", status: .complete, continuationOf: nil),
            MessageEntry(id: "a", role: .assistant, parts: [.text(id: "t", text: "one\n\ntwo")],
                         createdAt: 2, deviceId: "d", status: .complete, continuationOf: nil),
        ]
        let rows = TranscriptRowBuilder.rows(entries: entries, pendingSends: [
            PendingSend(messageId: "p", text: "later", at: 3)], parsers: &parsers, completed: &completed)
        XCTAssertEqual(rows.map(\.role), [.user, .assistant, .assistant, .user])
    }

    // MARK: Side chats

    func testTranscriptFeedAppliesDeltasLikeTheEngine() throws {
        func entry(_ id: String, _ text: String) -> [String: Any] {
            ["id": id, "role": "assistant", "createdAt": 1, "deviceId": "d",
             "parts": [["kind": "text", "id": "t0", "text": text]]]
        }
        var feed = TranscriptFeed()
        try feed.apply(["reset": [entry("a", "hello")]])
        try feed.apply(["upsert": [["after": "a", "entry": entry("b", "你")]], "count": 2])
        // `len` is the part's UTF-8 byte length (Rust String::len).
        try feed.apply(["append": [["entry": "b", "part": "t0", "text": "好", "len": 6]], "count": 2])
        XCTAssertEqual(feed.messages.map(\.id), ["a", "b"])
        guard case .text(_, let text) = feed.messages[1].parts[0] else { return XCTFail() }
        XCTAssertEqual(text, "你好")
        try feed.apply(["upsert": [["entry": entry("z", "head")]], "remove": ["a"], "count": 2])
        XCTAssertEqual(feed.messages.map(\.id), ["z", "b"])

        XCTAssertThrowsError(try feed.apply(["append": [["entry": "b", "part": "t0", "text": "!", "len": 99]],
                                             "count": 2]))
        var fresh = TranscriptFeed()
        XCTAssertThrowsError(try fresh.apply(["upsert": [["after": "missing", "entry": entry("c", "")]], "count": 1]))
        XCTAssertThrowsError(try fresh.apply(["count": 3]), "count mismatch")
    }

    func testDirectTransportSendsEchoAndSettle() {
        let store = SessionStore(chatId: "side-1", config: DemoDataset.dummyConfig, offline: true)
        var sent: [(String, String)] = []
        var stops = 0
        store.directTransport = SessionStore.DirectTransport(
            send: { prompt, id in sent.append((prompt, id)); return true },
            interrupt: { stops += 1; return true },
            respondInput: { _, _ in true })
        let chat = Chat(id: "side-1", deviceId: "d", title: nil, archived: false, cwd: "/w", branch: nil,
                        checkoutId: nil, config: nil, lastMessagePreview: nil, lastMessageAt: nil,
                        createdAt: 0, spaceId: nil, lastSeenAt: nil)
        XCTAssertTrue(store.sendRun(prompt: "why?", chat: chat))
        XCTAssertTrue(store.sendInterrupt())
        XCTAssertEqual(sent.map(\.0), ["why?"])
        XCTAssertEqual(stops, 1)
        XCTAssertEqual(store.pendingSends.map(\.messageId), [sent[0].1])
        store.setEntries([MessageEntry(id: sent[0].1, role: .user, parts: [.text(id: "t", text: "why?")],
                                       createdAt: 1, deviceId: "d", status: .complete, continuationOf: nil)])
        XCTAssertTrue(store.pendingSends.isEmpty, "the host's entry retires the echo")
    }

    func testPromotedSideChatTitle() {
        XCTAssertEqual(SideChatStore.promotedTitle("  why does the veil fade twice on reopen here  "),
                       "why does the veil fade")
        XCTAssertEqual(SideChatStore.promotedTitle(""), "Side chat")
    }
}

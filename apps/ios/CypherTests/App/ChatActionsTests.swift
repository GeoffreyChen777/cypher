import XCTest
@testable import Cypher

/// Chat actions: rename, delete, quick chats and session fork.
@MainActor
final class ChatActionsTests: XCTestCase {
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
}

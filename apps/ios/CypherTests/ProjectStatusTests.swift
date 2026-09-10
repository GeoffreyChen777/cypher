import XCTest
@testable import Cypher

final class ProjectStatusTests: XCTestCase {
    func testRunningWinsOverAllOtherProjectStates() {
        for other in [ChatIndicator.awaitingInput, .errored, .completed, .idle] {
            XCTAssertEqual(ChatIndicator.projectSummary([other, .working]), .working)
            XCTAssertEqual(ChatIndicator.projectSummary([.working, other]), .working)
        }
    }

    func testStoppedProjectsShowAttentionBeforeUnreadCompletion() {
        XCTAssertEqual(ChatIndicator.projectSummary([.completed, .errored, .awaitingInput]), .awaitingInput)
        XCTAssertEqual(ChatIndicator.projectSummary([.completed, .errored, .idle]), .errored)
        XCTAssertEqual(ChatIndicator.projectSummary([.idle, .completed]), .completed)
        XCTAssertEqual(ChatIndicator.projectSummary([.idle]), .idle)
        XCTAssertNil(ChatIndicator.projectSummary([]))
    }

    @MainActor
    func testProjectUpdatesFromRunningToUnreadToRead() throws {
        let model = AppModel()
        let demo = DemoDataset.standard()
        model.demo = demo
        var chat = try XCTUnwrap(demo.chats.first { $0.id == "chat-veil" })
        let project = try XCTUnwrap(chat.spaceId)
        chat.lastMessageAt = nowMs()
        chat.lastSeenAt = 0
        demo.chats = [chat]
        XCTAssertEqual(model.spaceIndicator(project), .working)

        demo.sessions[chat.id]?.status = .idle
        XCTAssertEqual(model.spaceIndicator(project), .completed)

        demo.chats[0].lastSeenAt = chat.lastMessageAt
        XCTAssertEqual(model.spaceIndicator(project), .idle)
    }

    @MainActor
    func testArchivedChildrenAndOtherProjectsDoNotAffectSummary() throws {
        let model = AppModel()
        let demo = DemoDataset.standard()
        model.demo = demo
        // This project's only visible session is read/idle. The other project
        // has running, input-needed and unread sessions.
        XCTAssertEqual(model.spaceIndicator("space-edge"), .idle)
        XCTAssertEqual(model.chats(in: "space-edge").count, 1)
        let index = try XCTUnwrap(demo.chats.firstIndex { $0.id == "chat-deploy" })
        demo.chats[index].archived = true
        XCTAssertNil(model.spaceIndicator("space-edge"))
        XCTAssertEqual(model.chats(in: "space-edge").count, 0)

        demo.chats.removeAll { !$0.isChild }
        XCTAssertNil(model.spaceIndicator("space-cypher"))
    }

    @MainActor
    func testExpiredWorkingSessionDoesNotMaskUnreadCompletion() throws {
        let model = AppModel()
        let demo = DemoDataset.standard()
        model.demo = demo
        let chat = try XCTUnwrap(demo.chats.first { $0.id == "chat-veil" })
        demo.chats = [chat]
        demo.chats[0].lastMessageAt = nowMs()
        demo.chats[0].lastSeenAt = 0
        demo.sessions[chat.id]?.updatedAt = nowMs() - sessionStaleMs - 1
        XCTAssertEqual(model.spaceIndicator(try XCTUnwrap(chat.spaceId)), .completed)
    }
}

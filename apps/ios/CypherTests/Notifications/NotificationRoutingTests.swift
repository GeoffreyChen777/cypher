import XCTest
@testable import Cypher

/// Where a tapped notice lands (`WorkspaceData.notificationTarget`), against
/// the demo: Atlas needs you for an approval, an asking session, a plan and
/// a review, and has work and routine threads.
@MainActor
final class NotificationRoutingTests: XCTestCase {
    private let scope = String(repeating: "a", count: 64)
    private var demo: DemoDataset!

    override func setUp() async throws {
        demo = DemoDataset.standard()
    }

    private func session(_ chat: String, project: String) -> PushPayload {
        PushPayload(eventId: UUID().uuidString, scope: scope, chatId: chat, projectId: project, kind: "completed")
    }

    private func workspace(
        _ bot: String = DemoDataset.atlasId, chat: String = "chat-atlas", item: PushItem? = nil
    ) -> PushPayload {
        PushPayload(
            eventId: UUID().uuidString, scope: scope, chatId: chat, projectId: bot,
            kind: item == nil ? "completed" : "input", workspaceId: bot, item: item)
    }

    private func page(_ item: PushItem) -> BotPage?? {
        guard case .workspace(_, _, let page) = demo.notificationTarget(workspace(item: item)) else { return nil }
        return .some(page)
    }

    func testSessionNoticesOpenTheirSessionAsBefore() {
        XCTAssertEqual(
            demo.notificationTarget(session("chat-veil", project: "space-cypher")), .session(chatId: "chat-veil"))
        XCTAssertEqual(
            demo.notificationTarget(session("chat-veil", project: "space-cypher"))?.route, .chat("chat-veil"))
        // Only a chat of the named project, and only one that exists here.
        XCTAssertNil(demo.notificationTarget(session("chat-veil", project: "space-edge")))
        XCTAssertNil(demo.notificationTarget(session("missing", project: "space-cypher")))
        // A workspace's chat has no project: without `workspaceId` it is
        // not a session notice.
        XCTAssertNil(demo.notificationTarget(session("chat-atlas", project: DemoDataset.atlasId)))
    }

    func testAWorkspaceNoticeOpensTheWorkspacesChat() throws {
        let target = try XCTUnwrap(demo.notificationTarget(workspace()))
        XCTAssertEqual(target, .workspace(botId: DemoDataset.atlasId, chatId: "chat-atlas", page: nil))
        XCTAssertEqual(target.route, .bot(DemoDataset.atlasId))
        XCTAssertEqual(target.chatId, "chat-atlas", "the workspace's chat is what is viewed")
        // Not this account's (or not synced yet), or a chat that isn't that
        // workspace's own: nothing opens.
        XCTAssertNil(demo.notificationTarget(workspace("bot-missing", chat: "chat-atlas")))
        XCTAssertNil(demo.notificationTarget(workspace(chat: "chat-scout")))
        XCTAssertNil(demo.notificationTarget(workspace(chat: "chat-atlas-thread-flaky")))
    }

    func testEachNeedsYouItemOpensItsBoardPage() {
        XCTAssertEqual(page(.plan("work-rounding")), .some(.workItem("work-rounding")))
        XCTAssertEqual(page(.approval("approval-staging")), .some(.approval("approval-staging")))
        XCTAssertEqual(page(.review("ex-review")), .some(.exchange("ex-review")))
        // A plan's work thread asks for its work; a routine's thread and a
        // project routine's session for no work.
        XCTAssertEqual(
            page(.asking("chat-atlas-thread-flaky")),
            .some(.asking(chatId: "chat-atlas-thread-flaky", workId: "work-flaky")))
        XCTAssertEqual(
            page(.asking("chat-atlas-routine-brief")), .some(.asking(chatId: "chat-atlas-routine-brief", workId: nil)))
        XCTAssertEqual(
            page(.asking("chat-atlas-routine-tests")), .some(.asking(chatId: "chat-atlas-routine-tests", workId: nil)))
        // Decided since: its page says how it ended.
        XCTAssertEqual(page(.plan("work-notes")), .some(.workItem("work-notes")))
    }

    func testAGoneItemFallsBackToTheWorkspacesChat() {
        for item: PushItem in [
            .plan("work-gone"), .approval("approval-gone"), .review("ex-gone"), .asking("chat-gone"),
            // Not a thread of this workspace: its own chat, another
            // workspace's chat, a project session.
            .asking("chat-atlas"), .asking("chat-scout"), .asking("chat-veil"),
        ] {
            XCTAssertEqual(page(item), .some(nil), "\(item)")
        }
        // Another workspace's record isn't this one's item.
        let scoutPlan = demo.notificationTarget(
            workspace(DemoDataset.scoutId, chat: "chat-scout", item: .plan("work-rounding")))
        XCTAssertEqual(scoutPlan, .workspace(botId: DemoDataset.scoutId, chatId: "chat-scout", page: nil))
    }

    func testABoardRequestExpires() {
        let request = NotificationBoardRequest(botId: DemoDataset.atlasId, page: .needsYou)
        XCTAssertFalse(request.expired())
        XCTAssertTrue(request.expired(now: Date().addingTimeInterval(11)))
    }

    func testAWorkspaceRouteOpensWithoutDuplicatingTheStack() {
        let bot = Route.bot(DemoDataset.atlasId)
        XCTAssertEqual(SessionNavigation.openingNotification(route: bot, in: []), [bot])
        XCTAssertEqual(SessionNavigation.openingNotification(route: bot, in: [.chat("a")]), [.chat("a"), bot])
        XCTAssertEqual(
            SessionNavigation.openingNotification(route: bot, in: [bot, .chat("t")]), [bot], "pops to it")
        XCTAssertEqual(SessionNavigation.openingNotification(route: bot, in: [.chat("a")], split: true), [bot])
        XCTAssertEqual(
            SessionNavigation.openingNotification(route: bot, in: [bot, .chat("t")], split: true), [bot])
        // The chat-id form is the same rule.
        XCTAssertEqual(
            SessionNavigation.openingNotification("c", in: [.chat("c"), .chat("d")]), [.chat("c")])
        XCTAssertEqual(SessionNavigation.openingNotification("c", in: [.chat("d")], split: true), [.chat("c")])
    }
}

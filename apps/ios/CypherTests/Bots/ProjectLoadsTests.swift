// The Projects pages' lists (crates/ui/src/shell/bots/board.rs
// `scoped_sessions`, projects.rs): a project's sessions in the workspace's
// scope, the granted projects grouped by device, and a project's work and
// routines.

import XCTest
@testable import Cypher

final class ProjectLoadsTests: XCTestCase {
    private func space(_ id: String, device: String) -> Space {
        Space(id: id, deviceId: device, path: "/p/\(id)", gitDetected: true, createdAt: 0)
    }

    private func chat(_ id: String, space: String?, at: Int64) -> Chat {
        Chat(
            id: id, deviceId: "mac", title: id, archived: false, cwd: nil, branch: nil, checkoutId: nil, config: nil,
            lastMessagePreview: nil, lastMessageAt: at, createdAt: 0, spaceId: space, lastSeenAt: at)
    }

    func testAProjectsSessionsAreTheScopesLiveRootSessions() {
        var archived = chat("archived", space: "a", at: 9)
        archived.archived = true
        var child = chat("child", space: "a", at: 8)
        child.child = ChildChat(parentChatId: "a1", parentRunId: "r", agent: "w", task: "t", mode: .sync)
        var thread = chat("thread", space: "a", at: 7)
        thread.bot = BotChatMarker(botId: "b", origin: .routine, thread: .routineSession(routineId: "r"))
        let chats = [
            chat("a1", space: "a", at: 1), chat("a2", space: "a", at: 5), archived, child, thread,
            chat("excluded", space: "a", at: 6), chat("b1", space: "b", at: 3),
        ]
        let scope = BotScope(projects: .selected(["a"]), excludeSessions: ["excluded"])
        XCTAssertEqual(ProjectLoads.sessions(scope, chats: chats, spaceId: "a").map(\.id), ["a2", "a1"])
        XCTAssertEqual(ProjectLoads.sessions(scope, chats: chats, spaceId: "b").map(\.id), [], "not granted")
    }

    func testProjectsGroupByDeviceInProjectOrder() {
        let loads = [
            BoardProjectLoad(space: space("web", device: "mac"), sessions: 2, running: 1),
            BoardProjectLoad(space: space("edge", device: "vps"), sessions: 0),
            BoardProjectLoad(space: space("cypher", device: "mac"), sessions: 1, asking: 1),
        ]
        let groups = ProjectLoads.groups(loads) { $0 == "edge" ? nil : 42 }
        XCTAssertEqual(groups.map(\.deviceId), ["mac", "vps"])
        XCTAssertEqual(groups[0].projects.map(\.id), ["web", "cypher"])
        XCTAssertEqual(groups[0].projects.map(\.lastActivity), [42, 42])
        XCTAssertEqual(groups[1].projects.first?.lastActivity, nil)
        XCTAssertEqual(groups[1].projects.first?.sessionsLine, "No sessions yet")
        XCTAssertEqual(groups[0].projects[1].sessionsLine, "1 session")
        XCTAssertEqual(ProjectLoads.groups([]) { _ in nil }, [])
    }

    func testAProjectsWorkAndRoutines() {
        func work(_ id: String, _ state: WorkState, project: String, at: Int64) -> BotWork {
            BotWork(
                id: id, botId: "b", title: id, steps: [WorkStep(id: "1", title: "s", project: project)], state: state,
                chatId: "c", createdAt: at)
        }
        let items = [
            work("old", .done, project: "a", at: 1), work("new", .running, project: "a", at: 9),
            work("cancelled", .cancelled, project: "a", at: 10), work("elsewhere", .done, project: "b", at: 5),
        ]
        XCTAssertEqual(ProjectLoads.work(items, spaceId: "a").map(\.id), ["new", "old"])
        XCTAssertEqual(ProjectLoads.work(items, spaceId: "a", limit: 1).map(\.id), ["new"])

        func routine(_ id: String, _ name: String, _ target: RoutineTarget) -> BotRoutine {
            BotRoutine(id: id, botId: "b", name: name, prompt: "p", target: target, createdAt: 0, updatedAt: 0)
        }
        let routines = [
            routine("1", "Tests", .project(spaceId: "a", worktree: true, model: nil)),
            routine("2", "brief", .workspace),
            routine("3", "Audit", .project(spaceId: "a", worktree: false, model: nil)),
        ]
        XCTAssertEqual(ProjectLoads.routines(routines, spaceId: "a").map(\.id), ["3", "1"])
    }

    @MainActor
    func testTheDemoWorkspacesProjects() throws {
        let demo = DemoDataset.standard()
        let atlas = try XCTUnwrap(demo.bot(id: DemoDataset.atlasId))
        let groups = demo.projectGroups(atlas)
        XCTAssertEqual(groups.map(\.deviceId), ["dev-mac"])
        XCTAssertEqual(Set(groups[0].projects.map(\.id)), ["space-cypher", "space-webapp"])
        let cypher = try XCTUnwrap(groups[0].projects.first { $0.id == "space-cypher" })
        XCTAssertGreaterThan(cypher.load.sessions, 0)
        XCTAssertNotNil(cypher.lastActivity)
        let sessions = ProjectLoads.sessions(atlas.scope, chats: demo.chats, spaceId: "space-cypher")
        XCTAssertFalse(sessions.contains { $0.id == "chat-atlas-routine-tests" }, "a routine's session is hidden")
    }
}

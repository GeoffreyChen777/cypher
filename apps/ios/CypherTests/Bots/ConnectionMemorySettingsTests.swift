// Track C's rules: invite codes (bot_wire.rs `split_invite_code`, the
// shared vectors), a connection's scope narrowed from its workspace's
// (network.rs `narrowed`), memory paths typed for a new file (memory.rs
// `memory_path`), Settings › Memory's store order, and the workspace
// settings form's scope and access summary.

import XCTest
@testable import Cypher

final class InviteCodeTests: XCTestCase {
    /// protocol/vectors/bot-frames-v1.json `codes`.
    func testInviteCodesSplitAsTheEngineDoes() {
        let cases: [(String, (String, String)?)] = [
            ("room-a.c2VjcmV0_x-1", ("room-a", "c2VjcmV0_x-1")),
            ("  room-a.secret  ", ("room-a", "secret")),
            ("nodot", nil),
            (".secret", nil),
            ("room.", nil),
            ("room.se.cret", nil),
            ("ro/om.secret", nil),
        ]
        for (code, expected) in cases {
            let split = splitInviteCode(code)
            XCTAssertEqual(split?.room, expected?.0, code)
            XCTAssertEqual(split?.secret, expected?.1, code)
        }
        XCTAssertNil(splitInviteCode("room.\(String(repeating: "s", count: 129))"))
        XCTAssertNotNil(splitInviteCode("room.\(String(repeating: "s", count: 128))"))
        XCTAssertNil(splitInviteCode("rööm.secret"))
    }

    func testInviteLines() {
        let now: Int64 = 1_762_761_600_000
        var invite = BotInvite(
            id: "i", botId: "b", createdAt: now - 3 * 3_600_000, expiresAt: now + 5 * 86_400_000, maxUses: 1)
        XCTAssertEqual(invite.title(now: now), "Invite code · made 3h ago")
        XCTAssertEqual(invite.detail(now: now, calendar: utc), "Expires in 5 days · 0 of 1 used")
        invite.revokeRequestedAt = now
        XCTAssertEqual(invite.detail(now: now, calendar: utc), "Revoking…")
        XCTAssertEqual(inviteExpiry(now + 86_400_000, now: now, calendar: utc), "tomorrow")
        XCTAssertEqual(inviteExpiry(now + 3_600_000, now: now, calendar: utc), "today 09:00")
    }

    func testConnectionAndExchangeLines() {
        let now: Int64 = 1_762_761_600_000
        var connection = BotConnection(
            id: "c", botId: "b", peer: PeerBot(roomId: "r", name: "Ledger", ownerName: "Dana"), createdAt: 0,
            replyMode: .review)
        XCTAssertEqual(connection.subLine(now: now), "Review replies · no questions yet")
        connection.lastExchangeAt = now - 2 * 3_600_000
        XCTAssertEqual(connection.subLine(now: now), "Review replies · 2h ago")
        connection.revokeRequestedAt = now
        XCTAssertEqual(connection.subLine(now: now), "Revoking…")
        XCTAssertEqual(connection.peerLabel, "Ledger (Dana)")

        var exchange = BotExchange(
            id: "x", botId: "b", connectionId: "c", direction: .incoming, question: "?", state: .awaitingReview,
            askedAt: 0)
        XCTAssertEqual(exchange.who(peer: connection.peer, bot: "Atlas"), "Ledger asked Atlas")
        XCTAssertEqual(exchange.who(peer: nil, bot: "Atlas"), "A former connection asked Atlas")
        XCTAssertEqual(exchange.stateTone, .waiting)
        exchange.direction = .outgoing
        XCTAssertEqual(exchange.who(peer: connection.peer, bot: "Atlas"), "Atlas asked Ledger")
        exchange.state = .expired
        XCTAssertEqual(exchange.stateTone, .muted)
    }

    private var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }
}

final class ConnectionReachTests: XCTestCase {
    private let workspace = BotScope(
        projects: .all, includeSessions: ["s"], excludeSessions: ["x"], includeArchived: true, transcripts: true)
    private let granted = ["a", "b", "c"]

    func testNoScopeReadsWhatTheWorkspaceReads() {
        let reach = ConnectionReach(scope: nil, bot: workspace, granted: granted)
        XCTAssertTrue(reach.sameProjects)
        XCTAssertEqual(reach.spaceIds, granted)
        XCTAssertTrue(reach.transcripts)
        XCTAssertNil(reach.scope(bot: workspace, granted: granted))
    }

    func testNarrowingKeepsTheWorkspacesReadingDepth() {
        // network.rs `narrowing_keeps_the_bots_reading_depth`.
        let reach = ConnectionReach(sameProjects: false, spaceIds: ["a"], transcripts: true)
        let scope = reach.scope(bot: workspace, granted: granted)
        XCTAssertEqual(scope?.projects, .selected(["a"]))
        XCTAssertEqual(scope?.includeSessions, [])
        XCTAssertEqual(scope?.excludeSessions, ["x"])
        XCTAssertEqual(scope?.includeArchived, true)
        XCTAssertEqual(scope?.transcripts, true)
    }

    func testNeverWiderThanTheWorkspace() {
        // A project the workspace no longer sees drops out, both read and
        // written; message text stays off when the workspace's is.
        var reading = workspace
        reading.projects = .selected(["a", "b"])
        reading.transcripts = false
        let stored = BotScope(projects: .selected(["b", "z"]), transcripts: true)
        let reach = ConnectionReach(scope: stored, bot: reading, granted: ["a", "b"])
        XCTAssertFalse(reach.sameProjects)
        XCTAssertEqual(reach.spaceIds, ["b"])
        XCTAssertFalse(reach.transcripts)
        var wider = reach
        wider.spaceIds = ["b", "z"]
        wider.transcripts = true
        let scope = wider.scope(bot: reading, granted: ["a", "b"])
        XCTAssertEqual(scope?.projects, .selected(["b"]))
        XCTAssertEqual(scope?.transcripts, false)
    }

    func testTextOnlyNarrowingKeepsTheProjects() {
        let reach = ConnectionReach(sameProjects: true, spaceIds: granted, transcripts: false)
        let scope = reach.scope(bot: workspace, granted: granted)
        XCTAssertEqual(scope?.projects, .all)
        XCTAssertEqual(scope?.includeSessions, ["s"])
        XCTAssertEqual(scope?.transcripts, false)
        // Read back, it is the same projects with text off.
        let back = ConnectionReach(scope: scope, bot: workspace, granted: granted)
        XCTAssertEqual(back, ConnectionReach(sameProjects: true, spaceIds: granted, transcripts: false))
    }

    func testTogglingKeepsTheWorkspacesOrder() {
        let reach = ConnectionReach(sameProjects: false, spaceIds: ["c"], transcripts: true)
        XCTAssertEqual(reach.toggling("a", granted: granted).spaceIds, ["a", "c"])
        XCTAssertEqual(reach.toggling("c", granted: granted).spaceIds, [])
    }
}

final class MemoryFormatTests: XCTestCase {
    func testTypedNamesBecomeStorePaths() {
        // memory.rs `new_file_names_become_store_paths`.
        XCTAssertEqual(MemoryFormat.path(typed: "testing"), "/testing.md")
        XCTAssertEqual(MemoryFormat.path(typed: "/testing.md"), "/testing.md")
        XCTAssertEqual(MemoryFormat.path(typed: "Build.MD"), "/Build.md")
        XCTAssertEqual(MemoryFormat.path(typed: "  decisions/rounding "), "/decisions/rounding.md")
        XCTAssertNil(MemoryFormat.path(typed: ""))
        XCTAssertNil(MemoryFormat.path(typed: "/"))
        XCTAssertNil(MemoryFormat.path(typed: ".hidden"))
        XCTAssertNil(MemoryFormat.path(typed: "a//b"))
        XCTAssertNil(MemoryFormat.path(typed: "a/../b"))
        XCTAssertNil(MemoryFormat.path(typed: "pipe|name"))
        // Within 160 bytes, the slash and `.md` included.
        XCTAssertNotNil(MemoryFormat.path(typed: String(repeating: "a", count: 156)))
        XCTAssertNil(MemoryFormat.path(typed: String(repeating: "a", count: 157)))
    }

    func testNamesSizesAndCosts() {
        XCTAssertEqual(MemoryFormat.fileName("/decisions/rounding.md"), "rounding.md")
        XCTAssertEqual(MemoryFormat.fileName(MemoryLimits.index), "MEMORY.md")
        XCTAssertEqual(MemoryFormat.displayPath("/decisions/rounding.md"), "decisions/rounding.md")
        XCTAssertEqual(MemoryFormat.size(820), "820 B")
        XCTAssertEqual(MemoryFormat.size(1229), "1.2 KB")
        XCTAssertEqual(MemoryFormat.size(64 * 1024), "64 KB")
        XCTAssertEqual(MemoryFormat.tokens(820), "820 tokens")
        XCTAssertEqual(MemoryFormat.tokens(12_400), "12.4k tokens")
        XCTAssertEqual(MemoryFormat.passCost(MemoryPass(changes: 3, tokens: 12_400)), "3 changes · 12.4k tokens")
        XCTAssertNil(MemoryFormat.passCost(MemoryPass()))
        XCTAssertEqual(MemoryFormat.hour(3), "03:00")
    }

    func testAPassWaitsWhileItRunsOrWasAskedFor() {
        var store = MemoryStore(id: "m", scope: .you)
        XCTAssertFalse(MemoryFormat.passWaiting(store))
        store.passRequestedAt = 10
        XCTAssertTrue(MemoryFormat.passWaiting(store))
        store.lastPass = MemoryPass(id: "p", at: 20, outcome: .done)
        XCTAssertFalse(MemoryFormat.passWaiting(store))
        store.lastPass?.outcome = .running
        XCTAssertTrue(MemoryFormat.passWaiting(store))
    }

    func testChangesSayWhatTheyDidAndWhetherTheyUndo() {
        var change = MemoryChange(id: "c", at: 0, author: .owner, path: "/rounding.md", action: .edit)
        XCTAssertEqual(MemoryFormat.title(change), "Edited rounding.md")
        XCTAssertFalse(MemoryFormat.undoable(change))
        change.undo = "v1"
        XCTAssertTrue(MemoryFormat.undoable(change))
        change.summary = "Added 2 lines"
        XCTAssertEqual(MemoryFormat.title(change), "rounding.md · Added 2 lines")
        change.undone = true
        XCTAssertFalse(MemoryFormat.undoable(change))
        let rename = MemoryChange(
            id: "r", at: 0, author: .owner, path: "/rounding.md", action: .rename(from: "/round.md"), undo: "v")
        XCTAssertEqual(MemoryFormat.title(rename), "Renamed round.md to rounding.md")
        let undo = MemoryChange(
            id: "u", at: 0, author: .owner, path: "/rounding.md", action: .undo(changeId: "c"), undo: "v")
        XCTAssertFalse(MemoryFormat.undoable(undo))
    }

    func testStoresListYoursThenProjectsThenWorkspacesByName() {
        let spaces = [
            Space(id: "s-web", deviceId: "d", path: "/p/webapp", gitDetected: true, createdAt: 0),
            Space(id: "s-cy", deviceId: "d", path: "/p/Cypher", gitDetected: true, createdAt: 0),
        ]
        let bots = [
            Bot(id: "b-scout", name: "scout", hostDeviceId: "d", chatId: "c1", createdAt: 0),
            Bot(id: "b-atlas", name: "Atlas", hostDeviceId: "d", chatId: "c2", createdAt: 0),
        ]
        let stores = [
            MemoryStore(id: "w-scout", scope: .workspace(botId: "b-scout")),
            MemoryStore(id: "p-web", scope: .project(spaceId: "s-web")),
            MemoryStore(id: "p-gone", scope: .project(spaceId: "s-gone")),
            MemoryStore(id: "you", scope: .you),
            MemoryStore(id: "w-gone", scope: .workspace(botId: "b-gone")),
            MemoryStore(id: "w-atlas", scope: .workspace(botId: "b-atlas")),
            MemoryStore(id: "role", scope: .role(agent: "reviewer", spaceId: nil)),
            MemoryStore(id: "p-cy", scope: .project(spaceId: "s-cy")),
        ]
        let listing = MemoryListing(stores: stores, spaces: spaces, bots: bots)
        XCTAssertEqual(listing.you?.id, "you")
        XCTAssertEqual(listing.projects.map(\.name), ["Cypher", "webapp"])
        XCTAssertEqual(listing.workspaces.map(\.name), ["Atlas", "scout"])
        XCTAssertNil(MemoryListing(stores: [], spaces: spaces, bots: bots).you)
    }
}

final class BotSettingsDraftTests: XCTestCase {
    func testANewWorkspaceStartsOnTheNextFaceAndColour() {
        let first = BotSettingsDraft(existingCount: 0, host: "mac")
        let second = BotSettingsDraft(existingCount: 1, host: "mac")
        XCTAssertEqual(first.icon, BotFace.allCases[0].rawValue)
        XCTAssertEqual(second.icon, BotFace.allCases[1].rawValue)
        XCTAssertNotEqual(first.color, second.color)
        XCTAssertEqual(first.access, .selected)
        XCTAssertEqual(first.scope.projects, .selected([]))
        XCTAssertEqual(first.capabilities, .newBot)
        XCTAssertEqual(first.hostDeviceId, "mac")
    }

    func testEditingRoundTripsTheScope() {
        let bot = Bot(
            id: "b", name: "Atlas", hostDeviceId: "mac", chatId: "c",
            instructions: "  Be brief.  ",
            scope: BotScope(
                projects: .selected(["a"]), includeSessions: ["s"], excludeSessions: ["x"], includeArchived: true,
                transcripts: false),
            createdAt: 0)
        var draft = BotSettingsDraft(bot: bot)
        XCTAssertEqual(draft.scope, bot.scope)
        XCTAssertEqual(draft.savedInstructions, "Be brief.")
        draft.toggle("b")
        draft.toggle("a")
        XCTAssertEqual(draft.scope.projects, .selected(["b"]))
        draft.access = .all
        XCTAssertEqual(draft.scope.projects, .all)
        draft.instructions = "   "
        XCTAssertNil(draft.savedInstructions)
    }

    func testProblemsAndCounter() {
        var draft = BotSettingsDraft(existingCount: 0, host: nil)
        XCTAssertEqual(draft.problem, "Give the workspace a name.")
        draft.name = String(repeating: "n", count: 41)
        XCTAssertEqual(draft.problem, "Name must be 40 characters or fewer.")
        draft.name = "Atlas"
        XCTAssertNil(draft.problem)
        XCTAssertNil(draft.instructionsCounter)
        draft.instructions = String(repeating: "i", count: 8_000)
        XCTAssertEqual(draft.instructionsCounter, "\(8_000.formatted()) / \(8_192.formatted())")
        draft.instructions = String(repeating: "i", count: 8_193)
        XCTAssertNotNil(draft.problem)
    }

    func testAccessSummaryCountsTheSessionsTheScopeReads() {
        func chat(_ id: String, space: String?, archived: Bool = false, hidden: Bool = false) -> Chat {
            Chat(
                id: id, deviceId: "d", title: id, archived: archived, cwd: nil, branch: nil, checkoutId: nil,
                config: nil, lastMessagePreview: nil, lastMessageAt: nil, createdAt: 0, spaceId: space,
                lastSeenAt: nil, bot: hidden ? BotChatMarker(botId: "b") : nil)
        }
        let chats = [
            chat("1", space: "a"), chat("2", space: "a", archived: true), chat("3", space: "b"),
            chat("4", space: "a", hidden: true), chat("5", space: nil),
        ]
        let spaces = [
            Space(id: "a", deviceId: "d", path: "/a", gitDetected: true, createdAt: 0),
            Space(id: "b", deviceId: "d", path: "/b", gitDetected: true, createdAt: 0),
        ]
        var draft = BotSettingsDraft(existingCount: 0, host: nil)
        XCTAssertEqual(draft.accessSummary(chats: chats, spaces: spaces), "No access to any sessions yet.")
        draft.toggle("a")
        XCTAssertEqual(draft.accessSummary(chats: chats, spaces: spaces), "Access to 1 session in 1 project.")
        draft.includeArchived = true
        draft.capabilities = BotCapabilities()
        XCTAssertEqual(
            draft.accessSummary(chats: chats, spaces: spaces), "Access to 2 sessions in 1 project. Read-only.")
        draft.access = .all
        draft.excludeSessions = ["3"]
        draft.includeSessions = ["5"]
        XCTAssertEqual(
            draft.accessSummary(chats: chats, spaces: spaces), "Access to 3 sessions in 2 projects. Read-only.")
    }

    func testHostsAreDesktopEnginesOnlineFirst() {
        let devices = [
            DeviceRow(id: "phone", name: "iPhone", platform: "ios"),
            DeviceRow(id: "vps", name: "Server", platform: "linux"),
            DeviceRow(id: "mac", name: "MacBook", platform: "macos"),
            DeviceRow(id: "old", name: "Attic Mac", platform: "macos"),
        ]
        let hosts = BotSettingsDraft.hosts(devices) { $0 != "old" }
        XCTAssertEqual(hosts.map(\.id), ["mac", "vps", "old"])
        XCTAssertEqual(hosts.map(\.online), [true, true, false])
        XCTAssertEqual(hosts.first?.laptop, true)
    }
}

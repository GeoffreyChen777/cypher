import XCTest
@testable import Cypher

/// The desktop sidebar view menu's rules (state/tests.rs
/// `sidebar_view_sorts_and_filters_cards`, `sidebar_time_filter_keeps_recent_live_and_pinned`).
final class SessionListingTests: XCTestCase {
    private let now: Int64 = 1_000_000_000_000
    private let hour: Int64 = 3_600_000

    /// `active` / `created`: hours before `now`.
    private func chat(
        _ id: String, title: String? = nil, space: String? = nil, active: Int64?, created: Int64
    ) -> Chat {
        Chat(
            id: id, deviceId: "dev", title: title, archived: false, cwd: nil, branch: nil,
            checkoutId: nil, config: nil, lastMessagePreview: nil,
            lastMessageAt: active.map { now - $0 * hour }, createdAt: now - created * hour,
            spaceId: space, lastSeenAt: nil)
    }

    private func space(_ id: String, name: String, device: String, created: Int64) -> Space {
        Space(
            id: id, deviceId: device, path: "/p/\(id)", name: name, gitDetected: false,
            gitCheckedAt: nil, checkoutId: nil, createdAt: now - created * hour)
    }

    private func ids(_ chats: [Chat]) -> [String] { chats.map(\.id) }

    func testSessionsSortWithTiesKeepingActivityOrder() {
        // Activity order: a, b, c.
        let chats = [
            chat("a", title: "beta", active: 1, created: 5),
            chat("b", title: "Alpha", active: 2, created: 9),
            chat("c", title: "beta", active: 3, created: 1),
        ]
        let idle: (Chat) -> ChatIndicator = { _ in .idle }
        func order(_ sort: SessionSort, reversed: Bool = false) -> [String] {
            ids(SessionListOptions(sort: sort, reversed: reversed).arrange(chats, now: now, indicator: idle))
        }
        XCTAssertEqual(order(.activity), ["a", "b", "c"])
        XCTAssertEqual(order(.activity, reversed: true), ["c", "b", "a"])
        XCTAssertEqual(order(.name), ["b", "a", "c"])
        XCTAssertEqual(order(.name, reversed: true), ["c", "a", "b"])
        XCTAssertEqual(order(.date), ["c", "a", "b"])
        // Device leaves sessions in activity order, either direction.
        XCTAssertEqual(order(.device), ["a", "b", "c"])
        XCTAssertEqual(order(.device, reversed: true), ["a", "b", "c"])
    }

    func testTimeFilterKeepsRecentAndLiveSessions() {
        let chats = [
            chat("recent", active: 1, created: 50),
            chat("never-messaged", active: nil, created: 2),
            chat("old-running", active: 40, created: 50),
            chat("old-asking", active: 40, created: 50),
            chat("old", active: 40, created: 50),
        ]
        let options = SessionListOptions(activeWithinMins: 3 * 60)
        let kept = options.arrange(chats, now: now) { chat in
            switch chat.id {
            case "old-running": .working
            case "old-asking": .awaitingInput
            default: .completed
            }
        }
        XCTAssertEqual(ids(kept), ["recent", "never-messaged", "old-running", "old-asking"])
    }

    func testProjectsOrderAndFilter() {
        let names = ["mac": "MacBook", "vps": "hetzner"]
        let spaces = [
            space("s1", name: "zeta", device: "mac", created: 100),
            space("s2", name: "alpha", device: "vps", created: 50),
            space("s3", name: "Mid", device: "mac", created: 20),
            space("empty-new", name: "new", device: "vps", created: 1),
            space("empty-old", name: "dusty", device: "mac", created: 200),
        ]
        // Activity order: s2's newest session, then s1's, then s3's.
        let active = [
            chat("c2", space: "s2", active: 1, created: 60),
            chat("c1", space: "s1", active: 5, created: 60),
            chat("c3", space: "s3", active: 30, created: 20),
        ]
        func order(_ options: SessionListOptions) -> [String] {
            options.arrange(
                spaces: spaces, activeChats: active, now: now,
                indicator: { _ in .idle }, deviceName: { names[$0] ?? $0 }
            ).map(\.id)
        }
        // Empty projects follow, by name.
        XCTAssertEqual(order(SessionListOptions()), ["s2", "s1", "s3", "empty-old", "empty-new"])
        XCTAssertEqual(order(SessionListOptions(sort: .name)), ["s2", "empty-old", "s3", "empty-new", "s1"])
        // hetzner < macbook; ties keep activity order.
        XCTAssertEqual(order(SessionListOptions(sort: .device)), ["s2", "empty-new", "s1", "s3", "empty-old"])
        XCTAssertEqual(order(SessionListOptions(sort: .date)), ["empty-new", "s3", "s2", "s1", "empty-old"])
        XCTAssertEqual(
            order(SessionListOptions(sort: .date, reversed: true)), ["empty-old", "s1", "s2", "s3", "empty-new"])
        // A 12h window: s3 (quiet 30h) and the old empty project go; the
        // project added an hour ago stays.
        XCTAssertEqual(order(SessionListOptions(activeWithinMins: 12 * 60)), ["s2", "s1", "empty-new"])
    }

    /// The `-demo-history` fixture exercises what the view menu promises.
    @MainActor
    func testDemoHistoryNarrowsUnderTheTimeFilter() {
        let model = AppModel()
        let demo = DemoDataset.standard()
        demo.addSessionHistory()
        model.demo = demo
        let all = model.listedSpaces(options: SessionListOptions()).map(\.id)
        XCTAssertTrue(all.contains("space-archive-tools"))
        let week = SessionListOptions(activeWithinMins: 7 * 24 * 60)
        let spaces = model.listedSpaces(options: week).map(\.id)
        XCTAssertFalse(spaces.contains("space-archive-tools"))
        XCTAssertTrue(spaces.contains("space-fresh-idea"))
        XCTAssertEqual(
            ids(model.listedChats(in: "space-research", options: week)),
            ["hist-zotero", "hist-arxiv", "hist-citations", "hist-waiting"])
        XCTAssertEqual(ids(model.listed(model.quickChats, options: week)), ["hist-quick-regex", "hist-quick-units"])
        // Created long ago but busy today: first by activity, last by date.
        let cypher = model.listedChats(in: "space-cypher", options: SessionListOptions(sort: .date))
        XCTAssertEqual(cypher.last?.id, "hist-refactor")
    }

    func testHomeHeadsTheListByDayWithLiveSessionsFirst() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // Noon UTC, so "hours ago" lands on predictable days.
        let noon = Date(timeIntervalSince1970: TimeInterval(now / 1000 / 86_400 * 86_400 + 43_200))
        let at = Int64(noon.timeIntervalSince1970 * 1000)
        func row(_ id: String, _ hoursAgo: Int64, created: Int64? = nil) -> Chat {
            var row = self.chat(id, active: nil, created: 0)
            row.lastMessageAt = at - hoursAgo * hour
            row.createdAt = at - (created ?? hoursAgo) * hour
            return row
        }
        let chats = [
            row("live", 30), row("today", 2), row("yesterday", 20, created: 1),
            row("week", 72), row("month", 20 * 24), row("old", 90 * 24),
        ]
        let indicator: (Chat) -> ChatIndicator = { $0.id == "live" ? .working : .idle }
        func headings(_ options: SessionListOptions) -> [String: [String]] {
            let arranged = options.arrange(chats, now: at, indicator: indicator)
            let sections = options.sections(
                arranged, now: noon, calendar: calendar, indicator: indicator, deviceName: { $0 })
            return Dictionary(uniqueKeysWithValues: sections.map { ($0.title, $0.chats.map(\.id)) })
        }
        let activity = SessionListOptions()
        let arranged = activity.arrange(chats, now: at, indicator: indicator)
        XCTAssertEqual(
            activity.sections(arranged, now: noon, calendar: calendar, indicator: indicator, deviceName: { $0 })
                .map(\.title),
            ["Now", "Today", "Yesterday", "Previous 7 Days", "Previous 30 Days", "Earlier"])
        XCTAssertEqual(
            headings(activity),
            [
                "Now": ["live"], "Today": ["today"], "Yesterday": ["yesterday"],
                "Previous 7 Days": ["week"], "Previous 30 Days": ["month"], "Earlier": ["old"],
            ])
        // Oldest first: no "Now"; the live session sits in its own day.
        XCTAssertNil(headings(SessionListOptions(reversed: true))["Now"])
        // By creation: "yesterday" was created an hour ago.
        XCTAssertEqual(headings(SessionListOptions(sort: .date))["Today"], ["yesterday", "today"])
        XCTAssertEqual(headings(SessionListOptions(sort: .name)).keys.sorted(), [""])
    }

    func testHomeRunsByDeviceInNameOrder() {
        var mac = chat("mac", active: 1, created: 1)
        mac.deviceId = "m"
        var vps = chat("vps", active: 2, created: 2)
        vps.deviceId = "v"
        let names = ["m": "MacBook", "v": "hetzner"]
        for reversed in [false, true] {
            let options = SessionListOptions(sort: .device, reversed: reversed)
            let titles = options.sections(
                [mac, vps], now: Date(), indicator: { _ in .idle }, deviceName: { names[$0]! }
            ).map(\.title)
            XCTAssertEqual(titles, reversed ? ["MacBook", "hetzner"] : ["hetzner", "MacBook"])
        }
        let onMac = SessionListOptions(device: "m").arrange([mac, vps], now: now) { _ in .idle }
        XCTAssertEqual(ids(onMac), ["mac"])
    }

    func testSearchMatchesEveryWordAnywhere() {
        var row = chat("c", title: "Zotero export cleanup", active: 1, created: 1)
        row.lastMessagePreview = "Fixed the BibTeX keys"
        func finds(_ query: String) -> Bool {
            SessionSearch.matches(row, query: query, project: "research-notes", device: "hetzner")
        }
        XCTAssertTrue(finds(""))
        XCTAssertTrue(finds("zotero"))
        XCTAssertTrue(finds("bibtex research"), "words may match different fields")
        XCTAssertTrue(finds("HETZNER"))
        XCTAssertFalse(finds("zotero mac"), "every word must match")
    }

    func testScopesSplitProjectsQuickChatsAndTheRest() {
        let live: Set<String> = ["s1"]
        let inProject = chat("p", space: "s1", active: 1, created: 1)
        let orphan = chat("o", space: "gone", active: 1, created: 1)
        var quick = chat("q", active: 1, created: 1)
        quick.cwd = "/Users/dev/.cypher/cypher-scratch/q"
        let loose = chat("l", active: 1, created: 1)
        func members(_ scope: SessionScope) -> [String] {
            ids([inProject, orphan, quick, loose].filter { scope.contains($0, liveSpaceIds: live) })
        }
        XCTAssertEqual(members(.all), ["p", "o", "q", "l"])
        XCTAssertEqual(members(.project("s1")), ["p"])
        XCTAssertEqual(members(.quickChats), ["q"])
        XCTAssertEqual(members(.noProject), ["o", "l"])
    }

    func testDirectionAndFormatting() {
        XCTAssertTrue(SessionListOptions().descending)
        XCTAssertFalse(SessionListOptions(sort: .name).descending)
        XCTAssertTrue(SessionListOptions(sort: .name, reversed: true).descending)
        XCTAssertTrue(SessionListOptions().isDefault)
        XCTAssertFalse(SessionListOptions(activeWithinMins: 60).isDefault)
        XCTAssertEqual(SessionListOptions.format(nil), "All")
        XCTAssertEqual(SessionListOptions.format(90), "1h 30m")
        XCTAssertEqual(SessionListOptions.format(12 * 60), "12h")
        XCTAssertEqual(SessionListOptions.format(3 * 24 * 60), "3d")
        XCTAssertEqual(SessionListOptions.format(24 * 60 + 6 * 60), "1d 6h")
    }
}

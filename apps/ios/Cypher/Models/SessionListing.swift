// The session list's view options — a port of the desktop sidebar's view
// menu (crates/ui/src/prefs.rs `SidebarSort`, prefs/sidebar_time.rs, and
// state/queries.rs `sidebar_groups_with`): a sort with its direction, a
// "last active" window and a device filter — plus what the phone's
// session-first Home adds on top: a project scope, search, and the list's
// time headings.

import Foundation

/// How projects (and the sessions inside them) are ordered.
enum SessionSort: String, CaseIterable, Identifiable {
    /// Newest activity first (the default).
    case activity
    /// Project display name, A→Z; sessions by title.
    case name
    /// Owning device name, A→Z; sessions keep their activity order.
    case device
    /// Creation date, newest first.
    case date

    var id: String { rawValue }

    var label: String {
        switch self {
        case .activity: "New activity"
        case .name: "Name"
        case .device: "Device"
        case .date: "Date created"
        }
    }

    /// Time sorts lead with the newest; name sorts read A→Z.
    var naturalDescending: Bool {
        self == .activity || self == .date
    }
}

struct SessionListOptions: Equatable {
    var sort: SessionSort = .activity
    /// Flip the sort's natural direction.
    var reversed = false
    /// Only sessions active within this many minutes (live ones stay);
    /// nil keeps them all.
    var activeWithinMins: Int?
    /// Only sessions (and projects) on this device; nil keeps every device.
    var device: String?

    /// The desktop slider's stops (minutes); past the last one is "All".
    static let stops: [Int] = [
        60, 2 * 60, 3 * 60, 6 * 60, 12 * 60,
        day, 2 * day, 3 * day, 7 * day, 14 * day, 30 * day,
    ]
    private static let day = 24 * 60

    /// Whether the list currently reads newest/Z first.
    var descending: Bool { sort.naturalDescending != reversed }

    var isDefault: Bool { self == SessionListOptions() }

    /// Compact display: `45m`, `12h`, `1h 30m`, `3d`, `1d 6h`; nil is "All".
    static func format(_ mins: Int?) -> String {
        guard let mins, mins > 0 else { return "All" }
        let parts = [(mins / day, "d"), (mins % day / 60, "h"), (mins % 60, "m")]
            .filter { $0.0 > 0 }
            .map { "\($0.0)\($0.1)" }
        return parts.joined(separator: " ")
    }

    /// The oldest activity the window keeps (ms), or nil without one.
    func cutoff(now: Int64) -> Int64? {
        guard let mins = activeWithinMins, mins > 0 else { return nil }
        return now - Int64(mins) * 60_000
    }

    /// Whether the time filter keeps a session: recent ones, and live ones
    /// (running or waiting on an answer) however quiet.
    func keeps(_ chat: Chat, cutoff: Int64?, indicator: ChatIndicator) -> Bool {
        if let device, chat.deviceId != device { return false }
        guard let cutoff else { return true }
        return indicator == .working || indicator == .awaitingInput
            || (chat.lastMessageAt ?? chat.createdAt) >= cutoff
    }

    /// One list's sessions (given in `sortActive` order) under the time
    /// filter and the sort. Sorts are stable, so ties keep the activity
    /// order; Device leaves sessions in activity order either way.
    func arrange(_ chats: [Chat], now: Int64, indicator: (Chat) -> ChatIndicator) -> [Chat] {
        let cutoff = cutoff(now: now)
        var chats = chats.filter { keeps($0, cutoff: cutoff, indicator: indicator($0)) }
        switch sort {
        case .activity, .device: break
        case .name:
            chats = stableSorted(chats) { ($0.title ?? "").lowercased() < ($1.title ?? "").lowercased() }
        case .date:
            chats = stableSorted(chats) { $0.createdAt > $1.createdAt }
        }
        if reversed && sort != .device { chats.reverse() }
        return chats
    }

    /// The project cards in order. Activity orders projects by their newest
    /// session (`activeChats`, all live-project sessions in `sortActive`
    /// order), then the empty ones by name / device / path / id. The time
    /// filter drops a project with no session left, unless it was added
    /// within the window (so a new project doesn't vanish).
    func arrange(
        spaces: [Space], activeChats: [Chat], now: Int64,
        indicator: (Chat) -> ChatIndicator, deviceName: (String) -> String
    ) -> [Space] {
        var rank: [String: Int] = [:]
        for chat in activeChats {
            if let id = chat.spaceId, rank[id] == nil { rank[id] = rank.count }
        }
        let busy = spaces.filter { rank[$0.id] != nil }.sorted { rank[$0.id]! < rank[$1.id]! }
        let empty = spaces.filter { rank[$0.id] == nil }.sorted { a, b in
            let (na, nb) = (a.displayName.lowercased(), b.displayName.lowercased())
            if na != nb { return na < nb }
            let (da, db) = (deviceName(a.deviceId), deviceName(b.deviceId))
            if da != db { return da < db }
            if a.path != b.path { return a.path < b.path }
            return a.id < b.id
        }
        var ordered = busy + empty
        if let device { ordered = ordered.filter { $0.deviceId == device } }
        if let cutoff = cutoff(now: now) {
            let kept = Set(
                activeChats
                    .filter { keeps($0, cutoff: cutoff, indicator: indicator($0)) }
                    .compactMap(\.spaceId))
            ordered = ordered.filter { kept.contains($0.id) || $0.createdAt >= cutoff }
        }
        switch sort {
        case .activity: break
        case .name:
            ordered = stableSorted(ordered) { a, b in
                let (na, nb) = (a.displayName.lowercased(), b.displayName.lowercased())
                if na != nb { return na < nb }
                return deviceName(a.deviceId).lowercased() < deviceName(b.deviceId).lowercased()
            }
        case .device:
            ordered = stableSorted(ordered) {
                deviceName($0.deviceId).lowercased() < deviceName($1.deviceId).lowercased()
            }
        case .date:
            ordered = stableSorted(ordered) { $0.createdAt > $1.createdAt }
        }
        if reversed { ordered.reverse() }
        return ordered
    }
}

/// Swift's `sorted` isn't stable; ties here must keep the incoming order.
private func stableSorted<T>(_ items: [T], by less: (T, T) -> Bool) -> [T] {
    items.enumerated().sorted { a, b in
        if less(a.element, b.element) { return true }
        if less(b.element, a.element) { return false }
        return a.offset < b.offset
    }.map(\.element)
}

// MARK: - Session-first Home

/// Which sessions Home lists: the project chips.
enum SessionScope: Hashable {
    case all
    case project(String)
    /// Project-less sessions in a host's scratch folder.
    case quickChats
    /// Other sessions outside any live project.
    case noProject

    func contains(_ chat: Chat, liveSpaceIds: Set<String>) -> Bool {
        switch self {
        case .all: true
        case .project(let id): chat.spaceId == id
        case .quickChats: chat.isScratch
        case .noProject: !chat.isScratch && !(chat.spaceId.map(liveSpaceIds.contains) ?? false)
        }
    }
}

enum SessionSearch {
    /// Every word of `query` appears in the title, the last message, the
    /// project or the device (case- and diacritic-insensitive). An empty
    /// query matches everything.
    static func matches(_ chat: Chat, query: String, project: String?, device: String) -> Bool {
        let words = query.split(whereSeparator: \.isWhitespace)
        guard !words.isEmpty else { return true }
        let haystack = [chat.displayTitle, chat.lastMessagePreview ?? "", project ?? "", device]
        return words.allSatisfy { word in
            haystack.contains { $0.localizedStandardContains(word) }
        }
    }
}

/// A run of Home's list under one heading.
struct SessionSection: Identifiable, Equatable {
    /// The heading; empty for a list without headings (Name sort).
    var title: String
    var chats: [Chat]
    var id: String { title }
}

extension SessionListOptions {
    /// Headings for an arranged list, keeping its order. By activity:
    /// "Now" (running or waiting on an answer) when newest come first, then
    /// Today, Yesterday, Previous 7 Days, Previous 30 Days and Earlier. By
    /// date, the same days over creation. By device, one run per device in
    /// name order (activity within). By name, one run without a heading.
    func sections(
        _ chats: [Chat], now: Date, calendar: Calendar = .current,
        indicator: (Chat) -> ChatIndicator, deviceName: (String) -> String
    ) -> [SessionSection] {
        guard !chats.isEmpty else { return [] }
        switch sort {
        case .name:
            return [SessionSection(title: "", chats: chats)]
        case .device:
            let byDevice = stableSorted(chats) { a, b in
                let (da, db) = (deviceName(a.deviceId).lowercased(), deviceName(b.deviceId).lowercased())
                return reversed ? da > db : da < db
            }
            return grouped(byDevice) { deviceName($0.deviceId) }
        case .activity, .date:
            let today = calendar.startOfDay(for: now)
            func day(_ chat: Chat) -> String {
                let ms = sort == .activity ? chat.lastMessageAt ?? chat.createdAt : chat.createdAt
                let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
                let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: today).day ?? 0
                switch days {
                case ...0: return "Today"
                case 1: return "Yesterday"
                case 2...7: return "Previous 7 Days"
                case 8...30: return "Previous 30 Days"
                default: return "Earlier"
                }
            }
            guard sort == .activity, !reversed else { return grouped(chats, by: day) }
            let live = chats.filter { [.working, .awaitingInput].contains(indicator($0)) }
            let liveIds = Set(live.map(\.id))
            let rest = grouped(chats.filter { !liveIds.contains($0.id) }, by: day)
            return (live.isEmpty ? [] : [SessionSection(title: "Now", chats: live)]) + rest
        }
    }

    /// Chats under their heading; headings in the order of their first chat.
    private func grouped(_ chats: [Chat], by title: (Chat) -> String) -> [SessionSection] {
        var sections: [SessionSection] = []
        for chat in chats {
            let heading = title(chat)
            if let ix = sections.firstIndex(where: { $0.title == heading }) {
                sections[ix].chats.append(chat)
            } else {
                sections.append(SessionSection(title: heading, chats: [chat]))
            }
        }
        return sections
    }
}

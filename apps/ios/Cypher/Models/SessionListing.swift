// The session list's view options — a port of the desktop sidebar's view
// menu (crates/ui/src/prefs.rs `SidebarSort`, prefs/sidebar_time.rs, and
// state/queries.rs `sidebar_groups_with`): a sort with its direction, and a
// "last active" window. The device filter is Home's device tabs.

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

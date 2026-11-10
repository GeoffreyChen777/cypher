// The Connections pages' words and rules (crates/ui/src/shell/bots/
// network.rs): a connection's sub-line, an invite's lines, who asked whom,
// the invite code's shape, and what a connection may read, narrowed from
// the workspace's own scope and never wider.

import Foundation

/// Questions a list shows.
let recentExchangesShown = 8

/// bot_wire.rs `split_invite_code`: `<room>.<secret>`, each 1–128 ASCII
/// letters, digits, `-` or `_`, around exactly one dot; surrounding
/// whitespace is ignored.
func splitInviteCode(_ code: String) -> (room: String, secret: String)? {
    let parts = code.trimmingCharacters(in: .whitespacesAndNewlines)
        .split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false)
    guard parts.count == 2 else { return nil }
    func idOK(_ part: Substring) -> Bool {
        !part.isEmpty && part.utf8.count <= 128
            && part.unicodeScalars.allSatisfy { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_") }
    }
    guard idOK(parts[0]), idOK(parts[1]) else { return nil }
    return (String(parts[0]), String(parts[1]))
}

extension BotConnection {
    /// "Review replies · 1h ago", "Auto replies · no questions yet",
    /// "Revoking…" (network.rs `connection_sub`).
    func subLine(now: Int64 = nowMs()) -> String {
        if revokeRequestedAt != nil && revokedAt == nil { return "Revoking…" }
        guard let last = lastExchangeAt else { return "\(replyMode.label) · no questions yet" }
        return "\(replyMode.label) · \(BotTime.ago(last, now: now))"
    }

    /// The peer's workspace and owner: "Ledger (Dana Whitfield)".
    var peerLabel: String {
        peer.ownerName.isEmpty ? peer.name : "\(peer.name) (\(peer.ownerName))"
    }
}

extension BotInvite {
    /// "Invite code · made 3h ago".
    func title(now: Int64 = nowMs()) -> String {
        "Invite code · made \(BotTime.ago(createdAt, now: now))"
    }

    /// "Expires in 5 days · 0 of 1 used", or "Revoking…".
    func detail(now: Int64 = nowMs(), calendar: Calendar = .current) -> String {
        if revokeRequestedAt != nil { return "Revoking…" }
        return "Expires \(inviteExpiry(expiresAt, now: now, calendar: calendar)) · \(uses) of \(maxUses) used"
    }
}

/// When an invite stops working: "today 18:30", "tomorrow", "in 6 days",
/// or the date (network.rs `expiry_words`).
func inviteExpiry(_ at: Int64, now: Int64 = nowMs(), calendar: Calendar = .current) -> String {
    let date = Date(timeIntervalSince1970: TimeInterval(at) / 1000)
    let today = calendar.startOfDay(for: Date(timeIntervalSince1970: TimeInterval(now) / 1000))
    let days = calendar.dateComponents([.day], from: today, to: calendar.startOfDay(for: date)).day ?? 0
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.calendar = calendar
    formatter.timeZone = calendar.timeZone
    switch days {
    case ...0:
        formatter.dateFormat = "HH:mm"
        return "today \(formatter.string(from: date))"
    case 1: return "tomorrow"
    case 2...13: return "in \(days) days"
    default:
        formatter.dateFormat = "MMM d"
        return formatter.string(from: date)
    }
}

/// The note under a fresh code: "Shown only this once. Works once, expires
/// in 7 days."
func issuedInviteNote(_ invite: InviteCreated, now: Int64 = nowMs()) -> String {
    let uses = invite.maxUses <= 1 ? "Works once" : "Works \(invite.maxUses) times"
    return "Shown only this once. \(uses), expires \(inviteExpiry(invite.expiresAt, now: now))."
}

extension BotExchange {
    /// Who asked whom: "Ledger asked Atlas", "Atlas asked a former
    /// connection" (network.rs `exchange_row`).
    func who(peer: PeerBot?, bot: String) -> String {
        switch (direction, peer) {
        case (.incoming, let peer?): "\(peer.name) asked \(bot)"
        case (.outgoing, let peer?): "\(bot) asked \(peer.name)"
        case (.incoming, nil): "A former connection asked \(bot)"
        case (.outgoing, nil): "\(bot) asked a former connection"
        }
    }

    /// The state word's tone in a list: amber only while it waits on the
    /// owner, faint once it ended without an answer.
    var stateTone: BotTone {
        if awaitingReview { return .waiting }
        return [.declined, .expired].contains(state) ? .muted : .active
    }
}

/// What a connection's questions may read, as its page edits it: the
/// workspace's projects or some of them, and whether message text is read.
/// Never wider than the workspace's own scope.
struct ConnectionReach: Equatable {
    /// Every project the workspace sees (no narrowing of projects).
    var sameProjects: Bool
    /// The chosen projects when narrowed: always within `granted`.
    var spaceIds: [String]
    /// Message text, only when the workspace itself reads it.
    var transcripts: Bool

    /// The connection's stored scope read against the workspace's: nil (or
    /// a copy of the workspace's projects) is the same projects; anything
    /// outside `granted` (the workspace's projects, in order) drops out.
    init(scope: BotScope?, bot: BotScope, granted: [String]) {
        transcripts = bot.transcripts && (scope?.transcripts ?? true)
        guard let scope, scope.projects != bot.projects || scope.includeSessions != bot.includeSessions else {
            sameProjects = true
            spaceIds = granted
            return
        }
        sameProjects = false
        switch scope.projects {
        case .noProjects: spaceIds = []
        case .all: spaceIds = granted
        case .selected(let ids): spaceIds = granted.filter(ids.contains)
        }
    }

    init(sameProjects: Bool, spaceIds: [String], transcripts: Bool) {
        self.sameProjects = sameProjects
        self.spaceIds = spaceIds
        self.transcripts = transcripts
    }

    /// The scope to write: nil when it reads exactly what the workspace
    /// reads; else the workspace's scope narrowed (network.rs `narrowed`),
    /// keeping its archived and excluded sessions as they are.
    func scope(bot: BotScope, granted: [String]) -> BotScope? {
        let transcripts = bot.transcripts && self.transcripts
        if sameProjects && transcripts == bot.transcripts { return nil }
        var scope = bot
        scope.transcripts = transcripts
        if !sameProjects {
            scope.projects = .selected(granted.filter(spaceIds.contains))
            scope.includeSessions = []
        }
        return scope
    }

    /// One project flipped in a narrowed scope.
    func toggling(_ spaceId: String, granted: [String]) -> ConnectionReach {
        var next = self
        if let ix = next.spaceIds.firstIndex(of: spaceId) {
            next.spaceIds.remove(at: ix)
        } else {
            next.spaceIds = granted.filter { next.spaceIds.contains($0) || $0 == spaceId }
        }
        return next
    }
}

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
            && part.unicodeScalars.allSatisfy {
                $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_")
            }
    }
    guard idOK(parts[0]), idOK(parts[1]) else { return nil }
    return (String(parts[0]), String(parts[1]))
}

/// What a connection is doing, the most pressing thing first: what its row
/// shows as its mark and its page says in its head (network.rs `Standing`).
enum ConnectionStanding: Equatable {
    /// Being disconnected on both sides.
    case leaving
    /// An answer waits for the owner's review.
    case review
    /// The workspace is answering one of their questions.
    case answering
    /// One of the workspace's questions waits for their answer.
    case asking
    /// Replies are off: their questions are declined.
    case off
    case idle

    /// `exchanges`: this connection's own.
    init(_ connection: BotConnection, exchanges: [BotExchange]) {
        if connection.revokeRequestedAt != nil {
            self = .leaving
        } else if exchanges.contains(where: \.awaitingReview) {
            self = .review
        } else if exchanges.contains(where: { $0.direction == .incoming && $0.inFlight }) {
            self = .answering
        } else if exchanges.contains(where: { $0.direction == .outgoing && $0.inFlight }) {
            self = .asking
        } else {
            self = connection.replyMode == .off ? .off : .idle
        }
    }

    /// Its place on the board: what waits on the owner, what is on its
    /// way, the rest.
    var rank: Int {
        switch self {
        case .review: 0
        case .answering, .asking: 1
        default: 2
        }
    }
}

/// A connected workspace as its row draws it.
struct BoardConnectionRow: Identifiable, Equatable {
    var connection: BotConnection
    var standing: ConnectionStanding
    /// A question of its is in flight and its ring still goes round.
    var moving: Bool

    var id: String { connection.id }
}

extension PeerBot {
    /// Who a connection is: the person, and their workspace's name beside
    /// it; the workspace alone when its owner is unnamed (network.rs
    /// `peer_names`).
    var names: (who: String, theirs: String?) {
        let owner = singleLine(ownerName)
        return owner.isEmpty ? (singleLine(name), nil) : (owner, singleLine(name))
    }

    /// The first word of the person's name, for a short status ("Waiting
    /// for Dana").
    var firstName: String {
        names.who.split(separator: " ").first.map(String.init) ?? names.who
    }
}

extension BotConnection {
    /// The peer's workspace and owner: "Ledger (Dana Whitfield)".
    var peerLabel: String {
        peer.ownerName.isEmpty ? peer.name : "\(peer.name) (\(peer.ownerName))"
    }

    /// Its row's one quiet note: when the last question was, or that
    /// replies are off.
    func note(_ standing: ConnectionStanding, now: Int64 = nowMs()) -> String? {
        switch standing {
        case .leaving: "leaving"
        case .off: "off"
        default: lastExchangeAt.map { BotTime.ago($0, now: now) }
        }
    }

    /// How it stands, in a word or two for its page's head.
    func status(_ standing: ConnectionStanding) -> BotStatusLabel {
        if !isLive { return BotStatusLabel("Disconnected", .muted) }
        return switch standing {
        case .leaving: BotStatusLabel("Disconnecting…", .muted)
        case .review: BotStatusLabel("Waiting on you", .waiting)
        case .answering: BotStatusLabel("Answering", .active)
        case .asking: BotStatusLabel("Waiting for \(peer.firstName)", .active)
        case .off: BotStatusLabel("Replies off", .muted)
        case .idle: BotStatusLabel("Connected", .active)
        }
    }
}

/// What a connection's questions can read, after "can read": everything
/// the workspace reads, or the projects it is narrowed to ("billing,
/// web-api", "billing, web-api +2", "no projects"; network.rs
/// `reads_words`). `names` are the narrowed projects', `all` how many the
/// workspace itself reads.
func connectionReads(_ reach: ConnectionReach, names: [String], all: Int, bot: String) -> String {
    if reach.sameProjects || (!names.isEmpty && names.count == all) { return "everything \(bot) reads" }
    switch names.count {
    case 0: return "no projects"
    case 1: return names[0]
    case 2: return "\(names[0]), \(names[1])"
    default: return "\(names[0]), \(names[1]) +\(names.count - 2)"
    }
}

/// How long an open invite has left, for a row's note: "5d left", "3h
/// left", "soon" (network.rs `left_words`).
func inviteLeft(_ expiresAt: Int64, now: Int64 = nowMs()) -> String {
    let minutes = max(expiresAt - now, 0) / 60_000
    switch minutes {
    case ..<60: return "soon"
    case ..<1440: return "\(minutes / 60)h left"
    default: return "\(minutes / 1440)d left"
    }
}

extension BotInvite {
    /// "Made 3h ago".
    func title(now: Int64 = nowMs()) -> String {
        "Made \(BotTime.ago(createdAt, now: now))"
    }

    /// How long it has left ("5d left"), with its uses only when it works
    /// more than once, or "revoking…".
    func detail(now: Int64 = nowMs()) -> String {
        if revokeRequestedAt != nil { return "revoking…" }
        let left = inviteLeft(expiresAt, now: now)
        return maxUses > 1 ? "\(uses) of \(maxUses) used · \(left)" : left
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

    /// Still on its way to an answer (network.rs `in_flight`).
    var inFlight: Bool {
        switch state {
        case .queued, .delivered, .working: true
        // Reviewed here; its host has not sent or declined it yet.
        case .awaitingReview: review != nil
        default: false
        }
    }

    /// How long a question that waits on the other side keeps its ring
    /// moving.
    static let freshMs: Int64 = 60_000

    /// Whether a question in flight shows as moving (network.rs `moving`):
    /// its ring goes round while this workspace writes its answer, and for
    /// the first minute of a wait on the other side (their answer, the
    /// sending of a reviewed one). That wait can last days, so after a
    /// minute the ring holds still until the question settles.
    func moving(now: Int64 = nowMs()) -> Bool {
        guard inFlight else { return false }
        if let review { return now - review.at < Self.freshMs }
        return direction == .incoming || now - askedAt < Self.freshMs
    }

    /// Its row's note once it settled: its age, with how it ended when
    /// that was without an answer.
    func settledNote(now: Int64 = nowMs()) -> String {
        let asked = BotTime.ago(askedAt, now: now)
        return switch state {
        case .declined: "declined · \(asked)"
        case .expired: "expired · \(asked)"
        default: asked
        }
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

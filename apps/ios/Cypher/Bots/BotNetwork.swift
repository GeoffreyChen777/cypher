// A workspace's Connections ("network" in code, bot_records.rs §11): mutual
// links to other users' workspaces, the invites that make them, and the
// questions between them. Plus the `CreateBotInvite` reply (bot_wire.rs) and
// the allowlist a connected owner's photo must pass.

import Foundation

/// Another user's workspace, as its owner published it.
struct PeerBot: Hashable, Sendable {
    var roomId: String
    var name: String
    var ownerName = ""
    /// The owner's profile photo; only a URL `peerAvatarURL` accepts is ever
    /// shown.
    var ownerAvatarUrl: String?
}

extension PeerBot: Codable {
    private enum CodingKeys: String, CodingKey { case roomId, name, ownerName, ownerAvatarUrl }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        roomId = try c.decode(String.self, forKey: .roomId)
        name = try c.decode(String.self, forKey: .name)
        ownerName = try c.decode(.ownerName, or: "")
        ownerAvatarUrl = try c.decodeIfPresent(String.self, forKey: .ownerAvatarUrl)
    }
}

enum ReplyMode: String, Codable, CaseIterable, Hashable, Sendable {
    /// Answers go back at once.
    case auto
    /// Answers wait as drafts until the owner sends them.
    case review
    /// Questions are declined.
    case off
}

/// A mutual link between this workspace and another user's (registry kind
/// `botConnections`).
struct BotConnection: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var peer: PeerBot
    var createdAt: Int64
    /// Owner-written.
    var replyMode: ReplyMode = .auto
    /// Owner-written: narrows what this peer's questions may read (never
    /// wider than the workspace's own scope). nil = the workspace's scope.
    var scope: BotScope?
    /// Owner-written: revoke on both sides.
    var revokeRequestedAt: Int64?
    // Host-written:
    var revokedAt: Int64?
    var threadChatId: String?
    var lastExchangeAt: Int64?

    var isLive: Bool { revokedAt == nil }
}

extension BotConnection {
    private enum CodingKeys: String, CodingKey {
        case id, botId, peer, createdAt, replyMode, scope, revokeRequestedAt, revokedAt, threadChatId
        case lastExchangeAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        peer = try c.decode(PeerBot.self, forKey: .peer)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        replyMode = try c.decode(.replyMode, or: .auto)
        scope = try c.decodeIfPresent(BotScope.self, forKey: .scope)
        revokeRequestedAt = try c.decodeIfPresent(Int64.self, forKey: .revokeRequestedAt)
        revokedAt = try c.decodeIfPresent(Int64.self, forKey: .revokedAt)
        threadChatId = try c.decodeIfPresent(String.self, forKey: .threadChatId)
        lastExchangeAt = try c.decodeIfPresent(Int64.self, forKey: .lastExchangeAt)
    }
}

/// An invite the owner created (registry kind `botInvites`); the code itself
/// is shown once and never stored.
struct BotInvite: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var createdAt: Int64
    var expiresAt: Int64
    var uses = 0
    var maxUses: Int
    /// Owner-written: revoke before it is used.
    var revokeRequestedAt: Int64?
    var revokedAt: Int64?

    func isOpen(nowMs: Int64) -> Bool {
        revokedAt == nil && uses < maxUses && nowMs < expiresAt
    }
}

extension BotInvite {
    private enum CodingKeys: String, CodingKey {
        case id, botId, createdAt, expiresAt, uses, maxUses, revokeRequestedAt, revokedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        createdAt = try c.decode(Int64.self, forKey: .createdAt)
        expiresAt = try c.decode(Int64.self, forKey: .expiresAt)
        uses = try c.decode(.uses, or: 0)
        maxUses = try c.decode(Int.self, forKey: .maxUses)
        revokeRequestedAt = try c.decodeIfPresent(Int64.self, forKey: .revokeRequestedAt)
        revokedAt = try c.decodeIfPresent(Int64.self, forKey: .revokedAt)
    }
}

enum ExchangeDirection: String, Codable, Hashable, Sendable {
    /// A peer asked this workspace.
    case incoming
    /// This workspace asked a peer.
    case outgoing
}

enum ExchangeState: String, Codable, Hashable, Sendable {
    case queued, delivered, working, awaitingReview, answered, declined, expired
}

/// Send (with the edited `text` when the owner changed it) or decline.
struct ReviewDecision: Codable, Hashable, Sendable {
    var send: Bool
    var text: String?
    var at: Int64
}

/// One question between two workspaces and its answer (registry kind
/// `botExchanges`).
struct BotExchange: Codable, Hashable, Identifiable, Sendable {
    var id: String
    var botId: String
    var connectionId: String
    var direction: ExchangeDirection
    var question: String
    var answer: String?
    var state: ExchangeState = .queued
    var askedAt: Int64
    var answeredAt: Int64?
    /// Owner-written, for an incoming question in Review mode.
    var review: ReviewDecision?

    var awaitingReview: Bool {
        direction == .incoming && state == .awaitingReview && review == nil
    }
}

extension BotExchange {
    private enum CodingKeys: String, CodingKey {
        case id, botId, connectionId, direction, question, answer, state, askedAt, answeredAt, review
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        botId = try c.decode(String.self, forKey: .botId)
        connectionId = try c.decode(String.self, forKey: .connectionId)
        direction = try c.decode(ExchangeDirection.self, forKey: .direction)
        question = try c.decode(String.self, forKey: .question)
        answer = try c.decodeIfPresent(String.self, forKey: .answer)
        state = try c.decode(.state, or: .queued)
        askedAt = try c.decode(Int64.self, forKey: .askedAt)
        answeredAt = try c.decodeIfPresent(Int64.self, forKey: .answeredAt)
        review = try c.decodeIfPresent(ReviewDecision.self, forKey: .review)
    }
}

/// `CreateBotInvite`'s reply (bot_wire.rs `InviteCreated`): the code is shown
/// once, here, and never stored.
struct InviteCreated: Hashable, Sendable {
    var inviteId: String
    /// `<botRoomId>.<secret>`.
    var code: String
    var createdAt: Int64
    var expiresAt: Int64
    var maxUses: Int
}

extension InviteCreated: Codable {
    private enum CodingKeys: String, CodingKey { case inviteId, code, createdAt, expiresAt, maxUses }

    /// The design names only `{inviteId, code, expiresAt}`; the rest default.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        inviteId = try c.decode(String.self, forKey: .inviteId)
        code = try c.decode(String.self, forKey: .code)
        createdAt = try c.decode(.createdAt, or: 0)
        expiresAt = try c.decode(Int64.self, forKey: .expiresAt)
        maxUses = try c.decode(.maxUses, or: 1)
    }
}

/// Hosts a connected owner's photo may come from: the identity providers'
/// avatar CDNs (bot_wire.rs `AVATAR_HOSTS`). Another person supplies the
/// URL and this phone loads it, so anything else (a tracking pixel on their
/// own server) is dropped and the letter tile shows instead.
private let avatarHosts = [
    "avatars.githubusercontent.com", "googleusercontent.com", "gravatar.com", "workoscdn.com",
]

/// `url` when it may be shown as a connected owner's photo: `https`, no
/// credentials or port, bounded, plain characters, on one of the avatar
/// hosts or a subdomain of one (bot_wire.rs `peer_avatar_url`).
func peerAvatarURL(_ url: String) -> URL? {
    guard !url.isEmpty, url.utf8.count <= 2048 else { return nil }
    if url.unicodeScalars.contains(where: {
        CharacterSet.whitespacesAndNewlines.contains($0) || CharacterSet.controlCharacters.contains($0)
    }) || url.contains("\\") {
        return nil
    }
    guard url.hasPrefix("https://") else { return nil }
    let rest = url.dropFirst("https://".count)
    let end = rest.firstIndex { $0 == "/" || $0 == "?" || $0 == "#" } ?? rest.endIndex
    let host = rest[..<end].lowercased()
    guard !host.isEmpty, !host.contains("@"), !host.contains(":") else { return nil }
    guard avatarHosts.contains(where: { host == $0 || host.hasSuffix(".\($0)") }) else { return nil }
    return URL(string: url)
}

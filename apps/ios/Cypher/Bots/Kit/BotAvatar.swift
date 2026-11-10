// A workspace's face, and a connected owner's: a tinted square with the
// workspace's glyph (bot_style.rs `bot_avatar`), and a round photo or letter
// tile for another person.

import SwiftUI

/// A `size`-pt avatar square: the glyph in the workspace's tint on a faint
/// wash of it (neutral without a colour).
struct BotAvatar: View {
    let bot: Bot
    var size: CGFloat = 28

    var body: some View {
        BotAvatarTile(name: bot.name, icon: bot.icon, color: bot.color, size: size)
    }
}

/// `BotAvatar` from loose keys: the settings form's live preview.
struct BotAvatarTile: View {
    let name: String
    let icon: String?
    let color: String?
    var size: CGFloat = 28

    var body: some View {
        let tint = BotStyle.color(color)
        RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
            .fill(tint.map { $0.opacity(0.18) } ?? Theme.text.opacity(0.08))
            .frame(width: size, height: size)
            .overlay {
                BotGlyph(name: name, icon: icon, color: color, size: size * 0.62)
            }
            .accessibilityHidden(true)
    }
}

/// The workspace's glyph alone, in its tint, no square (bot_style.rs
/// `bot_glyph`): for rows whose surface already sets it apart.
struct BotGlyph: View {
    let name: String
    let icon: String?
    let color: String?
    var size: CGFloat = 18

    init(name: String, icon: String?, color: String?, size: CGFloat = 18) {
        self.name = name
        self.icon = icon
        self.color = color
        self.size = size
    }

    init(bot: Bot, size: CGFloat = 18) {
        self.init(name: bot.name, icon: bot.icon, color: bot.color, size: size)
    }

    var body: some View {
        let tint = BotStyle.color(color) ?? Theme.text
        switch BotStyle.glyph(icon) {
        case .face(let face):
            BotFaceView(face: face, size: size, color: tint)
        case .symbol(let symbol):
            Image(systemName: symbol)
                .font(.system(size: size * 0.78, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: size, height: size)
        case .initial:
            Text(BotStyle.initial(name))
                .font(Theme.sans(size * 0.8, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: size, height: size)
        }
    }
}

/// A connected workspace's owner: their photo when it comes from an
/// identity provider's avatar host (`peerAvatarURL`), else their initial on
/// a neutral circle. A photo that fails to load shows the initial too.
struct PeerAvatar: View {
    let peer: PeerBot
    var size: CGFloat = 28

    var body: some View {
        Group {
            if let url = peer.ownerAvatarUrl.flatMap(peerAvatarURL) {
                AsyncImage(url: url) { phase in
                    if case .success(let image) = phase {
                        image.resizable().scaledToFill()
                    } else {
                        letter
                    }
                }
            } else {
                letter
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityHidden(true)
    }

    private var letter: some View {
        Circle()
            .fill(Theme.text.opacity(0.08))
            .overlay {
                Text(BotStyle.initial(peer.ownerName.isEmpty ? peer.name : peer.ownerName))
                    .font(Theme.sans(size * 0.45, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
            }
    }
}

#Preview("Avatars") {
    VStack(alignment: .leading, spacing: 16) {
        HStack(spacing: 12) {
            BotAvatarTile(name: "Atlas", icon: "bot-visor", color: "indigo", size: 44)
            BotAvatarTile(name: "Scout", icon: "bot-round", color: "teal", size: 44)
            BotAvatarTile(name: "Ledger", icon: "dollar", color: "amber", size: 44)
            BotAvatarTile(name: "Billing", icon: nil, color: nil, size: 44)
            BotAvatarTile(name: "Ops", icon: "unknown-key", color: "pink", size: 44)
        }
        HStack(spacing: 12) {
            PeerAvatar(peer: PeerBot(roomId: "r", name: "Ledger", ownerName: "Dana Whitfield"), size: 36)
            PeerAvatar(
                peer: PeerBot(
                    roomId: "r", name: "Harbor", ownerName: "Sam Ortiz",
                    ownerAvatarUrl: "https://evil.example/pixel.png"), size: 36)
        }
    }
    .padding()
    .background(Theme.surface)
}

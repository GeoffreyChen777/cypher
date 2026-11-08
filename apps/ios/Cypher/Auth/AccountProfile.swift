// The signed-in account as the toolbar shows it: name, email and the
// WorkOS (GitHub) profile picture. Saved on this device at sign-in and
// refreshed from every token refresh — the edge returns the user with it —
// so an account signed in before avatars still gets one, without signing in
// again. The avatar falls back to the initial-letter circle, like the
// desktop sidebar's (crates/ui/src/shell/overlays.rs `user_avatar`).

import SwiftUI

struct AccountProfile: Codable, Equatable {
    var email: String?
    var name: String?
    /// Only ever a safe URL (`safeAvatarURL`).
    var avatarURL: URL?

    init(email: String?, name: String?, avatarURL: URL?) {
        self.email = email
        self.name = name
        self.avatarURL = avatarURL
    }

    init(_ user: AuthUser) {
        let name = [user.firstName, user.lastName]
            .compactMap { $0?.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        self.init(
            email: user.email, name: name.isEmpty ? nil : name,
            avatarURL: Self.safeAvatarURL(user.profilePictureUrl))
    }

    /// The fallback's letter: the name's, else the email's.
    var initial: String {
        let source = name ?? email ?? ""
        return source.first.map { String($0).uppercased() } ?? "?"
    }

    /// An avatar URL safe to fetch, as the desktop's `safe_avatar_url`:
    /// HTTPS, a named host, no credentials, no whitespace or control
    /// characters, at most 2048 characters. Anything else shows the initial.
    static func safeAvatarURL(_ raw: String?) -> URL? {
        guard let raw, raw.count <= 2048,
            !raw.contains(where: { $0.isWhitespace || $0.asciiValue.map { $0 < 0x20 } == true }),
            let url = URL(string: raw), url.scheme?.lowercased() == "https",
            let host = url.host(percentEncoded: false), !host.isEmpty,
            url.user == nil, url.password == nil,
            // `https:///a.png` must not pass as host `a.png`.
            raw.dropFirst("https:".count).hasPrefix("//"), !raw.dropFirst("https://".count).hasPrefix("/")
        else { return nil }
        return url
    }

    // MARK: Storage (this device only)

    private static let key = "accountProfile"

    static func load(from defaults: UserDefaults = .standard) -> AccountProfile? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(AccountProfile.self, from: $0) }
    }

    func save(to defaults: UserDefaults = .standard) {
        if let data = try? JSONEncoder().encode(self) { defaults.set(data, forKey: Self.key) }
    }

    static func clear(from defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key)
    }
}

/// The account's picture in a circle, else its initial on a filled circle
/// (also while the picture loads, and when it fails).
struct AccountAvatar: View {
    let profile: AccountProfile
    var size: CGFloat = 30

    var body: some View {
        Group {
            if let url = profile.avatarURL {
                AsyncImage(url: url) { phase in
                    if case .success(let image) = phase {
                        image.renderingMode(.original).resizable().scaledToFill()
                    } else {
                        initialCircle
                    }
                }
            } else {
                initialCircle
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityHidden(true)
    }

    private var initialCircle: some View {
        Circle()
            .fill(Theme.text)
            .overlay {
                Text(profile.initial)
                    .font(Theme.sans(size * 0.46, weight: .semibold))
                    .foregroundStyle(Theme.bg)
            }
    }
}

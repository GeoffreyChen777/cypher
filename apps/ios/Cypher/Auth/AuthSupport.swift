// Pure auth helpers: RFC 7636 PKCE, sign-in org routing, and workspace-name
// validation. Foundation-only so the unit tests can pin them without a UI
// target — the iOS side of the PKCE flow the edge already enforces
// (`PKCE_VERIFIER_RE` + `codeVerifier` in edge/src/auth-routes.ts).

import CryptoKit
import Foundation
import Security

/// RFC 7636 PKCE (Proof Key for Code Exchange). Every authorize attempt draws
/// a fresh verifier, publishes its S256 challenge up front, and presents the
/// verifier exactly once at the exchange (the engine's auth.rs does the same:
/// a verifier bound to the pending `state`, consumed with it).
enum PKCE {
    /// RFC 7636 §4.1 verifier: 43–128 characters of the unreserved URL-safe
    /// alphabet. 32 CSPRNG bytes encode to 43 base64url chars (no padding) —
    /// in range and free of any character needing URL escaping.
    static func newVerifier() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        precondition(status == errSecSuccess, "the OS CSPRNG must be available")
        return base64URLEncode(Data(bytes))
    }

    /// RFC 7636 §4.2 S256 challenge: `base64url(sha256(verifier))` without
    /// padding — the value sent as `code_challenge` on the authorize URL.
    static func s256Challenge(for verifier: String) -> String {
        base64URLEncode(Data(SHA256.hash(data: Data(verifier.utf8))))
    }

    /// Well-formed per the edge's `PKCE_VERIFIER_RE`
    /// (`^[A-Za-z0-9\-._~]{43,128}$`).
    static func isValidVerifier(_ verifier: String) -> Bool {
        (43...128).contains(verifier.count)
            && verifier.allSatisfy { c in
                c.isASCII && (c.isLetter || c.isNumber || "-._~".contains(c))
            }
    }

    private static func base64URLEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// How a freshly-exchanged sign-in proceeds from the user's active
/// memberships — the engine's one-org auto-select rule, kept pure so the
/// picker/onboarding branches are testable without the UI target.
enum OrgSelection: Equatable {
    enum Route: Equatable {
        /// Exactly one membership — skip the picker and connect straight away.
        case autoSelect(AuthOrg)
        /// Multiple memberships — show the org picker.
        case pick([AuthOrg])
        /// No memberships — first-user onboarding (create a workspace).
        case createOrg
    }

    static func route(for orgs: [AuthOrg]) -> Route {
        if let only = orgs.first, orgs.count == 1 {
            return .autoSelect(only)
        }
        if orgs.isEmpty {
            return .createOrg
        }
        return .pick(orgs)
    }
}

/// Local workspace-name validation for first-user onboarding — the same
/// trim-then-1-80 rule the edge enforces on POST /auth/orgs.
enum OrgNameValidator {
    /// The trimmed name when it is a valid 1–80 char workspace name, else nil
    /// (the caller shows the error inline).
    static func normalize(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed.count <= 80 else { return nil }
        return trimmed
    }
}

// Dev bundle edge endpoint and fixed identity (CYPHER_DEVELOPMENT builds only).

import Foundation

/// The Dev bundle's Edge and identity, mirroring the desktop development
/// profile (apps/cypher/src/main.rs): a local `wrangler dev` by default, or a
/// staging endpoint named by `CYPHER_DEV_EDGE_URL`.
enum DevelopmentProfile {
    static var enabled: Bool {
        #if CYPHER_DEVELOPMENT
            return true
        #else
            return false
        #endif
    }
    #if CYPHER_DEVELOPMENT
        /// `cd edge && npm run dev`. The simulator shares the Mac's loopback.
        static let defaultEdge = URL(string: "http://127.0.0.1:27640")!
        static let edge = resolveEdge(ProcessInfo.processInfo.environment["CYPHER_DEV_EDGE_URL"])
        static let user = "dev-user"
        static let org = "dev-org"

        static func resolveEdge(_ override: String?) -> URL {
            guard let override, !override.isEmpty, let url = URL(string: override), edgeIsSafe(url) else {
                return defaultEdge
            }
            return url
        }

        /// https anywhere but the production Edge; plain http only on loopback.
        static func edgeIsSafe(_ url: URL) -> Bool {
            guard let host = url.host?.lowercased() else { return false }
            switch url.scheme?.lowercased() {
            case "https": return host != "edge.letscypher.app"
            case "http": return isLoopback(url)
            default: return false
            }
        }

        static func isLoopback(_ url: URL) -> Bool {
            ["localhost", "127.0.0.1", "::1"].contains(url.host?.lowercased() ?? "")
        }

        /// A remote development Edge takes that deployment's 64-hex shared secret.
        static func validToken(_ token: String) -> Bool {
            token.utf8.count == 64 && token.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
        }

        /// The bearer for `edge`. A loopback Edge runs `AUTH_MODE=dev`, where the
        /// bearer is the identity and only `user@org` carries the org claim the
        /// registry routes check; it needs no secret.
        static func bearer(edge: URL = edge, secret: String?) -> String? {
            if isLoopback(edge) { return "\(user)@\(org)" }
            guard let secret, validToken(secret) else { return nil }
            return secret
        }
    #endif
}

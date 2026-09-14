import Foundation

enum DevelopmentProfile {
    static var enabled: Bool {
        #if CYPHER_DEVELOPMENT
        return true
        #else
        return false
        #endif
    }
    #if CYPHER_DEVELOPMENT
    static let edge = URL(string: "https://cypher-edge-development.geoffreychen777.workers.dev")!
    static let user = "dev-user"
    static let org = "dev-org"
    static func validToken(_ token: String) -> Bool {
        token.utf8.count == 64 && token.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    #endif
}

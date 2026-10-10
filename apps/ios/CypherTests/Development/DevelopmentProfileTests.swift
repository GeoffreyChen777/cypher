import XCTest
@testable import Cypher

final class DevelopmentProfileTests: XCTestCase {
    func testBundleAndCallbackAreIsolated() {
        #if CYPHER_DEVELOPMENT
        XCTAssertEqual(Bundle.main.bundleIdentifier, "ai.mvp-lab.cypher.ios.dev")
        XCTAssertEqual(Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String, "Cypher Dev")
        #else
        XCTAssertEqual(Bundle.main.bundleIdentifier, "ai.mvp-lab.cypher.ios")
        #endif
    }

    func testRemoteDevelopmentBearerIsBuildGated() async {
        let staging = URL(string: "https://edge-dev.example.com")!
        let token = String(repeating: "a", count: 64)
        let config = AppConfig(edgeURL: staging, mode: .dev, userId: "dev-user", orgId: "dev-org",
                               deviceId: "ios-test", deviceName: "Test", devBearer: token)
        let bearer = await config.currentToken()
        #if CYPHER_DEVELOPMENT
        // Only the profile's own Edge (or loopback) gets a development bearer.
        XCTAssertTrue(DevelopmentProfile.enabled)
        XCTAssertNil(bearer)
        XCTAssertEqual(DevelopmentProfile.bearer(edge: staging, secret: token), token)
        XCTAssertNil(DevelopmentProfile.bearer(edge: staging, secret: nil))
        XCTAssertNil(DevelopmentProfile.bearer(edge: staging, secret: "dev-user@dev-org"))
        #else
        XCTAssertFalse(DevelopmentProfile.enabled)
        XCTAssertNil(bearer)
        #endif
    }

    #if CYPHER_DEVELOPMENT
    func testTheDefaultEdgeIsALocalWranglerDev() async {
        XCTAssertEqual(DevelopmentProfile.defaultEdge.absoluteString, "http://127.0.0.1:27640")
        // A loopback Edge runs AUTH_MODE=dev: the bearer is the identity,
        // with the org claim, and no secret is needed.
        let bearer = DevelopmentProfile.bearer(edge: DevelopmentProfile.defaultEdge, secret: nil)
        XCTAssertEqual(bearer, "dev-user@dev-org")
        let config = AppConfig(edgeURL: DevelopmentProfile.defaultEdge, mode: .dev,
                               userId: DevelopmentProfile.user, orgId: DevelopmentProfile.org,
                               deviceId: "ios-test", deviceName: "Test", devBearer: bearer)
        let current = await config.currentToken()
        XCTAssertEqual(current, "dev-user@dev-org")
    }

    func testEdgeOverridesAreFenced() {
        let resolve = DevelopmentProfile.resolveEdge
        XCTAssertEqual(resolve(nil), DevelopmentProfile.defaultEdge)
        XCTAssertEqual(resolve(""), DevelopmentProfile.defaultEdge)
        XCTAssertEqual(resolve("https://edge-dev.example.com").absoluteString, "https://edge-dev.example.com")
        XCTAssertEqual(resolve("http://localhost:8787").absoluteString, "http://localhost:8787")
        // Never production, never plain http off loopback.
        XCTAssertEqual(resolve("https://edge.letscypher.app"), DevelopmentProfile.defaultEdge)
        XCTAssertEqual(resolve("https://EDGE.letscypher.app/"), DevelopmentProfile.defaultEdge)
        XCTAssertEqual(resolve("http://edge-dev.example.com"), DevelopmentProfile.defaultEdge)
        XCTAssertEqual(resolve("ftp://127.0.0.1"), DevelopmentProfile.defaultEdge)
        XCTAssertTrue(DevelopmentProfile.validToken(String(repeating: "a", count: 64)))
        XCTAssertFalse(DevelopmentProfile.validToken("dev-user@dev-org"))
    }
    #endif
}

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
    func testCloudDevelopmentBearerIsBuildGated() async {
        let url = URL(string: "https://cypher-edge-development.geoffreychen777.workers.dev")!
        let token = String(repeating: "a", count: 64)
        let config = AppConfig(edgeURL: url, mode: .dev, userId: "dev-user", orgId: "dev-org",
                               deviceId: "ios-test", deviceName: "Test", devBearer: token)
        let bearer = await config.currentToken()
        #if CYPHER_DEVELOPMENT
        XCTAssertTrue(DevelopmentProfile.enabled)
        XCTAssertEqual(bearer, token)
        XCTAssertTrue(DevelopmentProfile.validToken(token))
        XCTAssertFalse(DevelopmentProfile.validToken("dev-user@dev-org"))
        XCTAssertEqual(config.userId, "dev-user")
        XCTAssertFalse(config.userId.contains(token))
        #else
        XCTAssertFalse(DevelopmentProfile.enabled)
        XCTAssertNil(bearer)
        #endif
    }
}

import XCTest
@testable import Cypher

final class ReleasePackagingTests: XCTestCase {
    func testRequiredReasonManifestIsInAppBundle() throws {
        let url = try XCTUnwrap(Bundle.main.url(forResource: "PrivacyInfo", withExtension: "xcprivacy"))
        let plist = try XCTUnwrap(PropertyListSerialization.propertyList(
            from: Data(contentsOf: url), format: nil) as? [String: Any])
        let entries = try XCTUnwrap(plist["NSPrivacyAccessedAPITypes"] as? [[String: Any]])
        let reasons = Dictionary(uniqueKeysWithValues: entries.compactMap { entry -> (String, [String])? in
            guard let category = entry["NSPrivacyAccessedAPIType"] as? String,
                  let codes = entry["NSPrivacyAccessedAPITypeReasons"] as? [String] else { return nil }
            return (category, codes)
        })
        XCTAssertEqual(reasons["NSPrivacyAccessedAPICategoryUserDefaults"], ["CA92.1"])
        XCTAssertEqual(reasons["NSPrivacyAccessedAPICategoryFileTimestamp"], ["C617.1"])
    }

    func testDistributionOptionsNeverUploadOrRewriteVersionImplicitly() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let plist = try XCTUnwrap(PropertyListSerialization.propertyList(
            from: Data(contentsOf: root.appendingPathComponent("ExportOptions-TestFlight.plist")),
            format: nil) as? [String: Any])
        XCTAssertEqual(plist["method"] as? String, "app-store-connect")
        XCTAssertEqual(plist["destination"] as? String, "export")
        XCTAssertEqual(plist["manageAppVersionAndBuildNumber"] as? Bool, false)
        XCTAssertEqual(plist["teamID"] as? String, "999875MHT4")
    }
}

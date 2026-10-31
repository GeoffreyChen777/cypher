import XCTest
@testable import Cypher

/// Patches apply as the desktop applies them (crates/ui/src/markdown/artifact/patch.rs):
/// both run protocol/vectors/artifact-patch-v1.json.
final class ArtifactPatchTests: XCTestCase {
    func testSharedVectors() throws {
        let root = try TestSupport.repoRoot()
        let data = try Data(contentsOf: root.appendingPathComponent("protocol/vectors/artifact-patch-v1.json"))
        let file = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let vectors = try XCTUnwrap(file["apply"] as? [[String: Any]])
        XCTAssertFalse(vectors.isEmpty)
        for v in vectors {
            let name = try XCTUnwrap(v["name"] as? String)
            let applied = ArtifactPatch.apply(
                base: try XCTUnwrap(v["base"] as? String), patch: try XCTUnwrap(v["patch"] as? String))
            XCTAssertEqual(applied.code, v["code"] as? String, name)
            XCTAssertEqual(applied.applied, v["applied"] as? Int, name)
            XCTAssertEqual(applied.total, v["total"] as? Int, name)
        }
    }

    func testOnlyCompleteHunksCount() {
        let done = "<<<<<<< SEARCH\na\n=======\nb\n>>>>>>> REPLACE\n"
        XCTAssertEqual(ArtifactPatch.hunks(done).count, 1)
        XCTAssertEqual(ArtifactPatch.hunks(done + "<<<<<<< SEARCH\nc\n=======\nd").count, 1)
    }
}

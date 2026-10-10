import XCTest
@testable import Cypher

/// The combined model + thinking picker: what level a newly picked model
/// runs at, and the demo catalog that exercises it offline.
@MainActor
final class ModelPickerTests: XCTestCase {
    private func model(_ id: String, _ levels: [String]) -> ModelInfo {
        ModelInfo(id: id, label: id, description: nil, reasoningLevels: levels)
    }

    func testANewModelKeepsTheLevelItTakesElseItsDefault() {
        let fourLevels = model("a/x", ["low", "medium", "high", "xhigh"])
        XCTAssertEqual(ModelPickerSheet.level(keeping: "low", on: fourLevels), "low")
        let threeLevels = model("a/y", ["low", "medium", "high"])
        XCTAssertEqual(ModelPickerSheet.level(keeping: "xhigh", on: threeLevels), "high",
                       "a level the model doesn't take falls back to its default")
        XCTAssertEqual(ModelPickerSheet.level(keeping: nil, on: fourLevels), "xhigh")
        XCTAssertNil(ModelPickerSheet.level(keeping: "high", on: model("a/z", [])),
                     "a model without levels runs without one")
    }

    func testEveryLevelHasAHint() {
        for level in ["minimal", "low", "medium", "high", "xhigh", "max"] {
            XCTAssertNotNil(HarnessCatalog.reasoningHint(level), level)
        }
        XCTAssertNil(HarnessCatalog.reasoningHint("custom"))
        XCTAssertEqual(HarnessCatalog.reasoningLabel("xhigh"), "X-High")
    }

    func testTheDemoCatalogShowsEveryPickerState() {
        let groups = HarnessCatalog.providerGroups(HarnessCatalog.demoModels)
        XCTAssertEqual(groups.map(\.name), ["Demo", "Claude", "ChatGPT"])
        XCTAssertEqual(HarnessCatalog.demoModels.first?.id, "demo/pi", "demo chats keep their model")
        XCTAssertTrue(HarnessCatalog.demoModels.contains { $0.reasoningLevels.isEmpty })
        XCTAssertNotNil(HarnessCatalog.providerBadgeHarness(groups[1].id))
    }
}

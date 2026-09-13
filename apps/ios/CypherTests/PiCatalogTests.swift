import XCTest
@testable import Cypher

final class PiCatalogTests: XCTestCase {
    func testOnlyPiIsOffered() throws {
        XCTAssertEqual(HarnessCatalog.harnesses.map(\.id), ["pi"])
        let descriptors = try JSONDecoder().decode([PiHarnessDescriptor].self, from: Data("""
        [
          {"id":"claude-code","installed":true,"enabled":true},
          {"id":"codex","installed":true,"enabled":true},
          {"id":"pi","installed":false,"enabled":true},
          {"id":"pi","installed":true,"enabled":false},
          {"id":"pi"},
          {"id":"pi","installed":true}
        ]
        """.utf8))
        XCTAssertEqual(descriptors.map(\.available), [false, false, false, false, false, true])
    }

    func testReasoningNeverInventsAnUnsupportedLevel() {
        for levels in [[], ["minimal"], ["off", "low"], ["high"], ["high", "xhigh"]] {
            let model = ModelInfo(id: "m", label: "m", description: nil, reasoningLevels: levels)
            let selected = HarnessCatalog.defaultReasoning(for: model)
            XCTAssertTrue(selected.map { levels.contains($0) } ?? levels.isEmpty)
        }
    }

    @MainActor
    func testEmptyAndFailedCatalogsNeverFallBack() async {
        let catalog = RemotePiCatalog()
        await catalog.load(deviceId: "a") { _ in [] }
        XCTAssertTrue(catalog.models(for: "a").isEmpty)
        XCTAssertEqual(catalog.error, .noModels)
        await catalog.load(deviceId: "a") { _ in throw PiCatalogError.runtimeUnavailable }
        XCTAssertEqual(catalog.error, .runtimeUnavailable)
        await catalog.load(deviceId: "a") { _ in throw RelayError.timeout }
        XCTAssertEqual(catalog.error, .unavailable)
        XCTAssertFalse(catalog.loading)
        XCTAssertTrue(catalog.models.isEmpty)
    }

    @MainActor
    func testRetryAndTargetIsolation() async {
        let catalog = RemotePiCatalog()
        await catalog.load(deviceId: "a") { _ in throw RelayError.hostOffline }
        await catalog.load(deviceId: "a") { target in
            XCTAssertEqual(target, "a")
            return HarnessCatalog.demoModels
        }
        XCTAssertNil(catalog.error)
        XCTAssertEqual(catalog.models(for: "a"), HarnessCatalog.demoModels)
        XCTAssertTrue(catalog.models(for: "b").isEmpty)
        await catalog.load(deviceId: "b") { _ in [] }
        XCTAssertTrue(catalog.models(for: "a").isEmpty)
    }

    @MainActor
    func testLateReplyCannotOverwriteNewDevice() async {
        let catalog = RemotePiCatalog()
        var reply: CheckedContinuation<[ModelInfo], Never>?
        let old = Task { @MainActor in
            await catalog.load(deviceId: "a") { _ in
                await withCheckedContinuation { reply = $0 }
            }
        }
        while reply == nil { await Task.yield() }
        await catalog.load(deviceId: "b") { _ in [] }
        reply?.resume(returning: HarnessCatalog.demoModels)
        await old.value
        XCTAssertEqual(catalog.deviceId, "b")
        XCTAssertEqual(catalog.error, .noModels)
        XCTAssertTrue(catalog.models.isEmpty)
    }

    @MainActor
    func testCancelledReplyDoesNotPublishModels() async {
        let catalog = RemotePiCatalog()
        var reply: CheckedContinuation<[ModelInfo], Never>?
        let task = Task { @MainActor in
            await catalog.load(deviceId: "a") { _ in
                await withCheckedContinuation { reply = $0 }
            }
        }
        while reply == nil { await Task.yield() }
        task.cancel()
        reply?.resume(returning: HarnessCatalog.demoModels)
        await task.value
        XCTAssertTrue(catalog.models.isEmpty)
        await catalog.load(deviceId: "a") { _ in HarnessCatalog.demoModels }
        XCTAssertFalse(catalog.loading)
        XCTAssertEqual(catalog.models.count, 1)
    }
}

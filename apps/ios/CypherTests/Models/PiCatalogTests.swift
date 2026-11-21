import XCTest
@testable import Cypher

private struct ModelInfoProbe: Decodable {
    let id: String
}

final class PiCatalogTests: XCTestCase {
    func testOnlyPiIsOffered() throws {
        XCTAssertEqual(HarnessCatalog.harnesses.map(\.id), ["pi"])
        let descriptors = try JSONDecoder().decode(
            [PiHarnessDescriptor].self,
            from: Data(
                """
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

    /// "Couldn't load models" covers a timeout, a refused link and the
    /// device's own error alike; the reason kept beside it tells them apart.
    @MainActor
    func testAFailedLoadKeepsWhatTheLinkOrTheDeviceSaid() async {
        let catalog = RemotePiCatalog()
        await catalog.load(deviceId: "a") { _ in throw RelayError.timeout }
        XCTAssertEqual(catalog.reason, "The device didn't respond")
        await catalog.load(deviceId: "a") { _ in throw RelayError.rpc("model discovery timed out") }
        XCTAssertEqual(catalog.error, .unavailable)
        XCTAssertEqual(catalog.reason, "model discovery timed out")
        XCTAssertEqual(catalog.failureText, "\(PiCatalogError.unavailable.message) (model discovery timed out)")
        await catalog.load(deviceId: "a") { _ in
            try JSONDecoder().decode([ModelInfoProbe].self, from: Data(#"[{"id":1}]"#.utf8)).map { _ in
                ModelInfo(id: "m", label: "m", description: nil, reasoningLevels: [])
            }
        }
        XCTAssertEqual(catalog.reason, "The device sent a model list this version of the app can't read.")

        // A failure the catalog names itself needs no second line, and a
        // later load starts clean.
        await catalog.load(deviceId: "a") { _ in throw PiCatalogError.runtimeUnavailable }
        XCTAssertNil(catalog.reason)
        XCTAssertEqual(catalog.failureText, PiCatalogError.runtimeUnavailable.message)
        await catalog.load(deviceId: "a") { _ in HarnessCatalog.demoModels }
        XCTAssertNil(catalog.reason)
        XCTAssertNil(catalog.failureText)
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
        XCTAssertEqual(catalog.models, HarnessCatalog.demoModels)
    }

    func testProvidersGroupInCatalogOrderWithDesktopNamesAndContextOnlySublines() {
        let model = { (id: String, desc: String?) in
            ModelInfo(id: id, label: id, description: desc, reasoningLevels: [])
        }
        let catalog = [
            model("mvp-lab/grok-4.6", "mvp-lab · 128k context"),
            model("claude-bridge/claude-opus-5", "claude-bridge · 1M context"),
            model("mvp-lab/kimi", "mvp-lab"),
            model("openai-codex/gpt-5.5", "openai-codex · 400k context"),
            model("unknown/x", nil),
            model("bare", "bare"),
        ]
        let groups = HarnessCatalog.providerGroups(catalog)
        XCTAssertEqual(groups.map(\.id), ["mvp-lab", "claude-bridge", "openai-codex", "other"])
        XCTAssertEqual(groups.map(\.name), ["mvp-lab", "Claude", "ChatGPT", "Other"])
        XCTAssertEqual(groups[0].models.map(\.id), ["mvp-lab/grok-4.6", "mvp-lab/kimi"])
        XCTAssertEqual(groups[3].models.count, 2, "unknown and bare ids both file under Other")
        XCTAssertEqual(HarnessCatalog.contextLabel(catalog[0]), "128k context")
        XCTAssertNil(HarnessCatalog.contextLabel(catalog[2]), "a provider-only subline repeats the group")
        XCTAssertNil(HarnessCatalog.contextLabel(catalog[4]))
        XCTAssertEqual(HarnessCatalog.providerBadgeHarness("claude-bridge"), "claude-code")
        XCTAssertEqual(HarnessCatalog.providerBadgeHarness("openai"), "codex")
        XCTAssertNil(HarnessCatalog.providerBadgeHarness("mvp-lab"))
    }
}

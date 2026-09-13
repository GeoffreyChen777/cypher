import XCTest
@testable import Cypher

final class SessionConnectionTests: XCTestCase {
    func testTransientReconnectAndCatalogLoadingHaveNoExplanationText() {
        let reconnect = SessionConnectionPhase.resolve(
            transportReady: false, needsCatalog: true, catalogMatches: true,
            catalogLoading: false, catalogError: .unavailable, modelAvailable: false)
        XCTAssertEqual(reconnect, .connecting, "Don't stack stale catalog failures on top of reconnecting")
        XCTAssertNil(reconnect.summary(elapsed: 0))
        XCTAssertNil(reconnect.summary(elapsed: 14.9))
        XCTAssertEqual(reconnect.summary(elapsed: 15), "Connection timed out")
        let loading = SessionConnectionPhase.resolve(
            transportReady: true, needsCatalog: true, catalogMatches: true,
            catalogLoading: true, catalogError: nil, modelAvailable: false)
        XCTAssertEqual(loading, .connecting)
        XCTAssertNil(loading.summary(elapsed: 1))
    }

    func testActualCatalogFailuresAreConciseAndKeepActionableDetails() {
        for error in [PiCatalogError.unavailable, .runtimeUnavailable, .noModels] {
            let phase = SessionConnectionPhase.resolve(
                transportReady: true, needsCatalog: true, catalogMatches: true,
                catalogLoading: false, catalogError: error, modelAvailable: false)
            XCTAssertEqual(phase, .catalogFailure(error))
            XCTAssertNotNil(phase.summary(elapsed: 0))
            XCTAssertEqual(phase.detail, error.message)
        }
    }

    func testReadyReadOnlyAndQuestionPanelDoNotWaitForAnUnusedCatalog() {
        let ready = SessionConnectionPhase.resolve(
            transportReady: true, needsCatalog: true, catalogMatches: true,
            catalogLoading: false, catalogError: nil, modelAvailable: true)
        XCTAssertEqual(ready, .ready)
        XCTAssertNil(ready.summary(elapsed: 100))
        XCTAssertEqual(SessionConnectionPhase.resolve(
            transportReady: true, needsCatalog: false, catalogMatches: false,
            catalogLoading: false, catalogError: nil, modelAvailable: false), .ready)
    }

    func testMissingSelectionIsNotMistakenForAConnectionFailureOrSilentlyReplaced() {
        XCTAssertEqual(SessionConnectionPhase.resolve(
            transportReady: true, needsCatalog: true, catalogMatches: true,
            catalogLoading: false, catalogError: nil, modelAvailable: false), .missingModel)
        XCTAssertEqual(SessionConnectionPhase.resolve(
            transportReady: true, needsCatalog: true, catalogMatches: false,
            catalogLoading: false, catalogError: .noModels, modelAvailable: false), .connecting)
    }
}

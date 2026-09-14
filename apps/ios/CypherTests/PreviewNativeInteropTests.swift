#if CYPHER_DEVELOPMENT
import XCTest
@testable import Cypher

@MainActor
final class PreviewNativeInteropTests: XCTestCase {
    func testNativeSessionStoreThroughLocalWorkerd() async throws {
        // Only scripts/test-preview-native.sh creates this opt-in fixture file.
        let control = URL(fileURLWithPath: "/tmp/cypher-preview-native.json")
        guard FileManager.default.fileExists(atPath: control.path) else { throw XCTSkip("local native fixture not running") }
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: control)) as? [String: String])
        let edge = try XCTUnwrap(URL(string: fixture["edge"]!))
        guard edge.scheme == "http", edge.host == "127.0.0.1" else { XCTFail("local fixture only"); return }
        let chatId = fixture["chatId"]!, expected = fixture["expected"]!
        let config = AppConfig(edgeURL: edge, mode: .dev, userId: "dev-user", orgId: "dev-org",
                               deviceId: "ios-preview-" + UUID().uuidString, deviceName: "Preview test",
                               devBearer: "local-preview-viewer", developmentPreview: true)
        let store = SessionStore(chatId: chatId, config: config)
        store.updateRoomGen(2); store.start(); defer { store.stop() }
        func wait(_ predicate: () -> Bool) async -> Bool {
            let deadline = Date().addingTimeInterval(45)
            while Date() < deadline { if predicate() { return true }; try? await Task.sleep(nanoseconds: 10_000_000) }
            return false
        }
        let seeded = await wait { store.entries.contains { $0.id == "seed" } }
        XCTAssertTrue(seeded)
        let chat = Chat(id: chatId, deviceId: fixture["hostDeviceId"]!, title: "Preview fixture", archived: false,
                        cwd: fixture["cwd"], branch: nil, checkoutId: nil, config: nil, lastMessagePreview: nil,
                        lastMessageAt: nil, createdAt: 0, spaceId: nil, lastSeenAt: nil, roomGen: 2)
        XCTAssertTrue(store.sendRun(prompt: "ios-preview-" + UUID().uuidString, chat: chat))
        var sawPreview = false
        var sawPreviewAheadOfDoc = false
        let finished = await wait {
            sawPreview = sawPreview || store.entries.flatMap(\.parts).contains {
                if case .text(_, let t) = $0 { return t.contains("实时预览") }; return false
            }
            let durableIDs = Set((SessionStore.decodeEntries(from: store.doc) ?? []).map(\.id))
            for entry in store.entries where !durableIDs.contains(entry.id) && entry.role == .assistant {
                sawPreviewAheadOfDoc = true
                XCTAssertNotEqual(store.lastEntryId, entry.id, "command basedOn must never use a preview-only entry")
            }
            return store.entries.contains { e in e.id != "seed" && e.role == .assistant && e.status == .complete
                && e.parts.contains { if case .text(_, let t) = $0 { return t == expected }; return false } }
        }
        XCTAssertTrue(finished); XCTAssertTrue(sawPreview); XCTAssertTrue(sawPreviewAheadOfDoc)
        let durable = SessionStore.decodeEntries(from: store.doc) ?? []
        XCTAssertFalse(durable.flatMap(\.parts).contains { if case .text(_, let t) = $0 { return t.contains("实时预览") }; return false })
        let receipt = try JSONSerialization.data(withJSONObject: ["passed": finished && sawPreview && sawPreviewAheadOfDoc, "sawPreview": sawPreview, "previewAheadOfDurable": sawPreviewAheadOfDoc])
        try receipt.write(to: control.deletingPathExtension().appendingPathExtension("ios.json"), options: .atomic)
    }
}
#endif

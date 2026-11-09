// DocDisk's chat2 snapshots and their `.honest` sidecar: a cursor is only
// taken as honest when the sidecar beside its snapshot names it.

import Foundation
import Loro
import XCTest
@testable import Cypher

final class DocDiskTests: XCTestCase {
    private var chatId = ""

    override func setUp() {
        chatId = "disk-\(UUID().uuidString.lowercased())"
    }

    override func tearDown() {
        let url = DocDisk.chat2URL(for: chatId)
        try? FileManager.default.removeItem(at: url)
        try? FileManager.default.removeItem(at: DocDisk.honestURL(for: url))
    }

    private func doc() throws -> LoroDoc {
        let doc = LoroDoc()
        try doc.getMap(id: "meta").insert(key: "title", v: "hello")
        doc.commit()
        return doc
    }

    private func load() -> (cursor: UInt64, honest: Bool)? {
        DocDisk.loadChat2(into: LoroDoc(), id: chatId)
    }

    func testAnHonestSaveLoadsHonest() throws {
        DocDisk.saveChat2(doc: try doc(), id: chatId, cursor: 42, honest: true)
        let saved = try XCTUnwrap(load())
        XCTAssertEqual(saved.cursor, 42)
        XCTAssertTrue(saved.honest)
    }

    func testADishonestSaveWithdrawsTheSidecar() throws {
        DocDisk.saveChat2(doc: try doc(), id: chatId, cursor: 42, honest: true)
        DocDisk.saveChat2(doc: try doc(), id: chatId, cursor: 43, honest: false)
        let saved = try XCTUnwrap(load())
        XCTAssertEqual(saved.cursor, 43)
        XCTAssertFalse(saved.honest)
        XCTAssertFalse(FileManager.default.fileExists(atPath: DocDisk.honestURL(for: DocDisk.chat2URL(for: chatId)).path))
    }

    func testASidecarNamingAnotherCursorVouchesForNothing() throws {
        DocDisk.saveChat2(doc: try doc(), id: chatId, cursor: 42, honest: true)
        // A snapshot written by a build that knew nothing of the sidecar.
        let url = DocDisk.chat2URL(for: chatId)
        var data = try Data(contentsOf: url)
        data.replaceSubrange(8..<16, with: withUnsafeBytes(of: UInt64(50).littleEndian) { Data($0) })
        try data.write(to: url)
        let saved = try XCTUnwrap(load())
        XCTAssertEqual(saved.cursor, 50)
        XCTAssertFalse(saved.honest)
    }

    func testASnapshotWithoutASidecarIsNotHonest() throws {
        DocDisk.saveChat2(doc: try doc(), id: chatId, cursor: 7)
        XCTAssertEqual(load()?.honest, false)
    }
}

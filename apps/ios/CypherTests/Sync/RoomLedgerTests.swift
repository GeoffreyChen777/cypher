// The room ledger and its heal: what a chat2 room provably holds, and the
// batches that send it what only this device still has. The scenario tests
// replay the 2026-11-28 production case with real Loro docs: a send whose
// push died with the app, then sends from later launches built on it, which
// the host parked until the lost op came back.

import Foundation
import Loro
import XCTest
@testable import Cypher

final class RoomLedgerTests: XCTestCase {
    private func span(_ start: Int32, _ end: Int32) -> CounterSpan { CounterSpan(start: start, end: end) }

    private func ledger(checkpointSeq: UInt64 = 10, frontier: [UInt64: Int32] = [1: 5], headSeq: UInt64 = 20)
        -> RoomLedger
    {
        var ledger = RoomLedger()
        ledger.rebase(checkpointSeq: checkpointSeq, frontier: frontier, headSeq: headSeq)
        return ledger
    }

    // MARK: Pure rules

    func testAFreshLedgerStandsOnTheCheckpointAndAsksForTheRowsAfterIt() {
        let ledger = ledger()
        XCTAssertEqual(ledger.basis, 10)
        XCTAssertEqual(ledger.through, 10, "rows after the checkpoint must be read once")
        XCTAssertEqual(ledger.held(), [1: 5])
    }

    func testRowsAfterTheCheckpointExtendWhatTheRoomHolds() {
        var ledger = ledger()
        ledger.note(seq: 11, spans: [1: span(5, 8)])
        ledger.note(seq: 12, spans: [2: span(0, 3)])
        XCTAssertEqual(ledger.through, 12)
        XCTAssertEqual(ledger.held(), [1: 8, 2: 3])
    }

    func testRowsTheCheckpointCoversAddNothing() {
        var ledger = ledger()
        ledger.note(seq: 9, spans: [3: span(0, 4)])
        XCTAssertEqual(ledger.held(), [1: 5])
        XCTAssertEqual(ledger.through, 10)
    }

    func testAGapHoldsThroughBackUntilItFills() {
        var ledger = ledger()
        ledger.note(seq: 12, spans: [2: span(0, 3)])
        XCTAssertEqual(ledger.through, 10)
        ledger.note(seq: 11, spans: [1: span(5, 8)])
        XCTAssertEqual(ledger.through, 12)
        XCTAssertEqual(ledger.held(), [1: 8, 2: 3])
    }

    func testOpsPastAGapInAPeersCountersAreNotClaimed() {
        var ledger = ledger()
        ledger.note(seq: 11, spans: [1: span(7, 9)])  // 5..<7 is in no row
        XCTAssertEqual(ledger.held(), [1: 5])
        ledger.note(seq: 12, spans: [1: span(5, 7)])
        XCTAssertEqual(ledger.held(), [1: 9])
    }

    func testACheckpointTakesItsRowsOutOfTheClaim() {
        // A phone's row the host parked: in the room at seq 11, so held.
        var ledger = ledger()
        ledger.note(seq: 11, spans: [9: span(0, 10)])
        ledger.note(seq: 12, spans: [1: span(5, 6)])
        XCTAssertEqual(ledger.held()[9], 10)
        // A checkpoint at 11 deleted that row, and the host never applied
        // it: its frontier lacks peer 9, so the claim goes.
        ledger.rebase(checkpointSeq: 11, frontier: [1: 5], headSeq: 12)
        XCTAssertNil(ledger.held()[9])
        XCTAssertEqual(ledger.held()[1], 6, "the row after the checkpoint still counts")
        XCTAssertEqual(ledger.through, 12, "nothing to read again")
    }

    func testACheckpointCuttingThroughARunDropsTheRun() {
        var ledger = ledger()
        ledger.note(seq: 11, spans: [1: span(5, 6)])
        ledger.note(seq: 12, spans: [1: span(6, 7)])
        ledger.rebase(checkpointSeq: 11, frontier: [1: 6], headSeq: 12)
        XCTAssertEqual(ledger.held()[1], 6, "less than the room holds (a duplicate push), never more")
    }

    func testRunsStopGrowingAtTheirRowLimit() {
        var ledger = ledger(frontier: [:])
        for ix in 0..<(RoomLedger.rowsPerRun + 1) {
            ledger.note(seq: 11 + UInt64(ix), spans: [1: span(Int32(ix), Int32(ix + 1))])
        }
        XCTAssertEqual(ledger.runs.count, 2)
        XCTAssertEqual(ledger.held()[1], Int32(RoomLedger.rowsPerRun + 1))
    }

    func testACheckpointPastWhatTheLedgerSawNeedsNoOldRows() {
        var ledger = ledger()
        ledger.rebase(checkpointSeq: 30, frontier: [1: 40], headSeq: 35)
        XCTAssertEqual(ledger.through, 30)
        XCTAssertEqual(ledger.held(), [1: 40])
    }

    func testAWipedRoomStartsTheLedgerOver() {
        var ledger = ledger()
        ledger.note(seq: 11, spans: [1: span(5, 8)])
        ledger.rebase(checkpointSeq: 0, frontier: [:], headSeq: 3)
        XCTAssertEqual(ledger.through, 0)
        XCTAssertEqual(ledger.held(), [:])
    }

    func testMissingIsWhatTheDocHasBeyondTheRoomAndTheQueue() {
        var ledger = ledger(frontier: [1: 5])
        ledger.note(seq: 11, spans: [9: span(0, 10)])
        let doc: [UInt64: Int32] = [1: 5, 9: 20, 7: 4]
        XCTAssertEqual(
            ledger.missing(doc: doc, queued: []),
            [IdSpan(peer: 7, counter: span(0, 4)), IdSpan(peer: 9, counter: span(10, 20))])
        XCTAssertEqual(
            ledger.missing(doc: doc, queued: [[9: span(10, 20)], [7: span(0, 4)]]), [],
            "queued batches go up on their own")
        XCTAssertEqual(
            ledger.missing(doc: doc, queued: [[9: span(15, 20)]]),
            [IdSpan(peer: 7, counter: span(0, 4)), IdSpan(peer: 9, counter: span(10, 20))],
            "a queued batch past a gap leaves the gap to send")
    }

    func testTheLedgerSurvivesASaveAndLoad() throws {
        var ledger = ledger()
        ledger.note(seq: 11, spans: [9: span(0, 10)])
        ledger.note(seq: 13, spans: [1: span(5, 6)])
        let id = "ledger-\(UUID().uuidString.lowercased())"
        defer {
            let url = DocDisk.chat2URL(for: id)
            try? FileManager.default.removeItem(at: DocDisk.ledgerURL(for: url))
        }
        DocDisk.saveLedger(ledger, id: id)
        XCTAssertEqual(DocDisk.loadLedger(id: id), ledger)
    }

    // MARK: The production case, with real docs

    /// A phone's doc: one launch makes a write whose push never lands, later
    /// launches (each a fresh peer, as SessionStore's `LoroDoc()` is) build
    /// sends on top of it. Returns the doc and the rows the room got.
    private struct Phone {
        let doc: LoroDoc
        let rows: [Data]
        let lostPeer: UInt64
    }

    private func command(_ doc: LoroDoc, _ text: String) throws {
        let commands = doc.getList(id: "commands")
        let entry = try commands.insertContainer(pos: commands.len(), child: LoroMap())
        try entry.insert(key: "kind", v: "run")
        try entry.insert(key: "prompt", v: text)
        doc.commit()
    }

    private func phone(host: LoroDoc) throws -> Phone {
        let first = LoroDoc()
        _ = try first.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        try command(first, "the send whose push died with the app")
        var rows: [Data] = []
        var saved = try first.export(mode: .snapshot)
        for text in ["sent from the next launch", "sent after a force-quit"] {
            let launch = LoroDoc()  // a fresh peer, the saved doc imported
            _ = try launch.importWith(bytes: saved, origin: "disk")
            let before = launch.oplogVv()
            try command(launch, text)
            rows.append(try launch.export(mode: .updates(from: before)))
            saved = try launch.export(mode: .snapshot)
        }
        let doc = LoroDoc()
        _ = try doc.importWith(bytes: saved, origin: "disk")
        return Phone(doc: doc, rows: rows, lostPeer: first.peerId())
    }

    private func prompts(_ doc: LoroDoc) -> [String] {
        let commands = doc.getList(id: "commands")
        return (0..<commands.len()).compactMap { ix in
            commands.get(index: ix)?.asLoroMap()?.get(key: "prompt")?.asValue()?.stringValue
        }
    }

    private func hostDoc() throws -> LoroDoc {
        let host = LoroDoc()
        try host.getMap(id: "meta").insert(key: "title", v: "cypher")
        host.commit()
        return host
    }

    func testTheHostParksSendsBuiltOnALostWriteUntilTheHealSendsIt() throws {
        let host = try hostDoc()
        let checkpoint = host.oplogVv().toHashmap()
        let phone = try phone(host: host)
        // The room: a checkpoint at 10, then the two later sends.
        var ledger = RoomLedger()
        ledger.rebase(checkpointSeq: 10, frontier: checkpoint, headSeq: 12)
        for (ix, row) in phone.rows.enumerated() {
            ledger.note(seq: 11 + UInt64(ix), spans: try XCTUnwrap(RoomLedger.spans(of: row)))
            let status = try host.importWith(bytes: row, origin: "remote")
            XCTAssertNotNil(status.pending, "every later send waits on the lost write")
        }
        XCTAssertEqual(prompts(host), [], "the host sees no command at all")

        let heal = RoomHeal.batches(doc: phone.doc, ledger: ledger, queued: [])
        XCTAssertEqual(heal.count, 1)
        let healed = try XCTUnwrap(RoomLedger.spans(of: heal[0]))
        XCTAssertEqual(Set(healed.keys), [phone.lostPeer], "only the lost write goes up again")

        let status = try host.importWith(bytes: heal[0], origin: "remote")
        XCTAssertNil(status.pending)
        XCTAssertEqual(
            prompts(host),
            ["the send whose push died with the app", "sent from the next launch", "sent after a force-quit"])
    }

    func testRowsAHostParkedAndACheckpointDeletedGoUpAgain() throws {
        let host = try hostDoc()
        let phone = try phone(host: host)
        // The host parked both sends, then a checkpoint covering their seqs
        // deleted them: the room now holds only the host's own ops.
        for row in phone.rows { _ = try host.importWith(bytes: row, origin: "remote") }
        var ledger = RoomLedger()
        ledger.rebase(checkpointSeq: 10, frontier: [:], headSeq: 12)
        for (ix, row) in phone.rows.enumerated() {
            ledger.note(seq: 11 + UInt64(ix), spans: try XCTUnwrap(RoomLedger.spans(of: row)))
        }
        ledger.rebase(checkpointSeq: 12, frontier: host.oplogVv().toHashmap(), headSeq: 12)

        let reader = LoroDoc()  // a device that joins after the checkpoint
        _ = try reader.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        for batch in RoomHeal.batches(doc: phone.doc, ledger: ledger, queued: []) {
            _ = try reader.importWith(bytes: batch, origin: "remote")
        }
        XCTAssertEqual(prompts(reader).count, 3, "the phone restored all three sends")
    }

    func testAHealOverThePushCapGoesUpInPiecesThatAllApply() throws {
        let host = try hostDoc()
        let phone = LoroDoc()
        _ = try phone.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        for ix in 0..<40 { try command(phone, String(repeating: "x", count: 200) + "\(ix)") }
        var ledger = RoomLedger()
        ledger.rebase(checkpointSeq: 0, frontier: host.oplogVv().toHashmap(), headSeq: 0)

        let batches = RoomHeal.batches(doc: phone, ledger: ledger, queued: [], maxBytes: 2_000)
        XCTAssertGreaterThan(batches.count, 1)
        XCTAssertTrue(batches.allSatisfy { $0.count <= 2_000 })
        for batch in batches { _ = try host.importWith(bytes: batch, origin: "remote") }
        XCTAssertEqual(prompts(host).count, 40)
    }

    func testNothingGoesUpWhenTheRoomHasEverything() throws {
        let host = try hostDoc()
        let phone = LoroDoc()
        _ = try phone.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        let before = phone.oplogVv()
        try command(phone, "acked")
        let row = try phone.export(mode: .updates(from: before))
        var ledger = RoomLedger()
        ledger.rebase(checkpointSeq: 10, frontier: host.oplogVv().toHashmap(), headSeq: 11)
        XCTAssertEqual(RoomHeal.batches(doc: phone, ledger: ledger, queued: [row]), [], "the queued push covers it")
        ledger.note(seq: 11, spans: try XCTUnwrap(RoomLedger.spans(of: row)))
        XCTAssertEqual(RoomHeal.batches(doc: phone, ledger: ledger, queued: []), [])
    }
}

// MARK: The client: rows request, heal first, acks noted

@MainActor
final class ChatRoomHealTests: XCTestCase {
    @MainActor private final class Phone {
        let doc = LoroDoc()
        var cursor: UInt64 = 0
        var ledger = RoomLedger()
    }

    private let clock = ManualRoomClock()
    private let transport = FakeWebSocketTransport()
    private var client: ChatRoomClient!
    private let phone = Phone()

    override func setUp() async throws {
        let phone = phone
        let delegate = ChatRoomClient.Delegate(
            cursor: { phone.cursor },
            cursorHonest: { true },
            containsFrontier: { _ in true },
            applyCheckpoint: { _, _ in true },
            applyRow: { bytes, seq in
                _ = try? phone.doc.importWith(bytes: bytes, origin: "remote")
                phone.cursor = max(phone.cursor, seq)
            },
            advanceCursor: { phone.cursor = max(phone.cursor, $0) },
            clampCursor: { phone.cursor = $0 },
            setCursor: { phone.cursor = $0 },
            event: { _ in },
            ledgerRebase: { state, frontier in
                phone.ledger.rebase(
                    checkpointSeq: state.checkpointSize > 0 ? state.checkpointSeq : 0,
                    frontier: state.checkpointSize > 0 ? RoomLedger.vector(frontier) : [:],
                    headSeq: state.headSeq)
                return phone.ledger.through
            },
            ledgerNote: { seq, bytes in phone.ledger.note(seq: seq, spans: RoomLedger.spans(of: bytes) ?? [:]) },
            heal: { queued in RoomHeal.batches(doc: phone.doc, ledger: phone.ledger, queued: queued) })
        client = ChatRoomClient(
            chatId: "chat", device: "phone",
            urlProvider: { URL(string: "wss://edge.test/chat2/chat/ws") },
            checkpointRequest: { nil }, rowsRequest: { _ in nil }, pushRequest: { _ in nil },
            delegate: delegate, transport: transport, clock: clock)
    }

    override func tearDown() async throws {
        await client.stop()
    }

    private func frames(_ socket: FakeWebSocket, _ kind: UInt8) -> [ChatWireFrame] {
        socket.sentData.compactMap(ChatWire.decode).filter { $0.kind == kind }
    }

    private func state(checkpointSeq: UInt64, headSeq: UInt64, frontier: Data) -> Data {
        ChatWire.encode(
            ChatFrameType.state,
            header: [
                "headSeq": headSeq, "seqFloor": checkpointSeq, "checkpointSeq": checkpointSeq,
                "checkpointSize": 100, "rowCount": 0, "rowBytes": 0,
            ],
            payload: frontier)
    }

    private func socket(_ index: Int) async -> FakeWebSocket {
        await eventually("socket \(index) never opened") { transport.sockets.count > index }
        let socket = transport.sockets[index]
        await eventually("no hello") { !self.frames(socket, ChatFrameType.hello).isEmpty }
        return socket
    }

    func testAJoinReadsTheRowsTheLedgerNeedsAndSendsTheLostWriteFirst() async throws {
        // The host's doc and the phone's: the phone holds a write the room lacks.
        let host = LoroDoc()
        try host.getMap(id: "meta").insert(key: "title", v: "cypher")
        host.commit()
        let frontier = host.oplogVv().encode()
        _ = try phone.doc.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        try phone.doc.getMap(id: "commands").insert(key: "lost", v: "never pushed")
        phone.doc.commit()
        // The host's rows since the checkpoint; the phone's saved cursor is
        // already past most of them.
        var hostRows: [UInt64: Data] = [:]
        for seq in UInt64(11)...16 {
            let before = host.oplogVv()
            try host.getMap(id: "meta").insert(key: "status", v: "row \(seq)")
            host.commit()
            hostRows[seq] = try host.export(mode: .updates(from: before))
            if seq <= 15 { _ = try phone.doc.importWith(bytes: hostRows[seq]!, origin: "remote") }
        }
        phone.cursor = 15

        await client.start()
        let first = await socket(0)
        first.deliver(.data(state(checkpointSeq: 10, headSeq: 16, frontier: frontier)))
        await eventually("no rows request") { !self.frames(first, ChatFrameType.rowsReq).isEmpty }
        let request = try XCTUnwrap(frames(first, ChatFrameType.rowsReq).first)
        XCTAssertEqual(
            (request.header["after"] as? NSNumber)?.uint64Value, 10, "the ledger reads every row since the checkpoint")
        XCTAssertEqual(request.header["excludeOwn"] as? Bool, false)

        for seq in UInt64(11)...16 {
            first.deliver(
                .data(
                    ChatWire.encode(
                        ChatFrameType.row, header: ["seq": seq, "device": "mac", "batchId": "h\(seq)"],
                        payload: hostRows[seq]!)))
        }
        first.deliver(.data(ChatWire.encode(ChatFrameType.rowsDone, header: ["headSeq": 16])))
        await eventually("the heal never went up") { !self.frames(first, ChatFrameType.push).isEmpty }
        let push = try XCTUnwrap(frames(first, ChatFrameType.push).first)
        let spans = try XCTUnwrap(RoomLedger.spans(of: push.payload))
        XCTAssertEqual(Set(spans.keys), [phone.doc.peerId()])
        XCTAssertEqual(frames(first, ChatFrameType.push).count, 1, "the host's rows are not sent back")
        let reader = LoroDoc()
        _ = try reader.importWith(bytes: try host.export(mode: .snapshot), origin: "remote")
        _ = try reader.importWith(bytes: push.payload, origin: "remote")
        XCTAssertEqual(reader.getMap(id: "commands").get(key: "lost")?.asValue()?.stringValue, "never pushed")

        // The ack puts it in the ledger: the next join sends nothing.
        let batchId = try XCTUnwrap(push.header["batchId"] as? String)
        first.deliver(.data(ChatWire.encode(ChatFrameType.ack, header: ["batchId": batchId, "seq": 17, "dup": false])))
        await eventually("the ack never reached the ledger") { self.phone.ledger.through == 17 }
        first.fail()
        await eventually("no backoff armed") { self.clock.hasSleeper(lasting: 250 * 1_000_000) }
        clock.advance(nanoseconds: 250 * 1_000_000)
        let second = await socket(1)
        second.deliver(.data(state(checkpointSeq: 10, headSeq: 17, frontier: frontier)))
        await eventually("no rows request") { !self.frames(second, ChatFrameType.rowsReq).isEmpty }
        XCTAssertEqual(
            (frames(second, ChatFrameType.rowsReq).first?.header["after"] as? NSNumber)?.uint64Value, 17)
        second.deliver(.data(ChatWire.encode(ChatFrameType.rowsDone, header: ["headSeq": 17])))
        await settle()
        XCTAssertEqual(frames(second, ChatFrameType.push).count, 0, "nothing left to send")
    }
}

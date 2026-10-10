// ChatRoomClient socket lifecycle against a fake transport and manual clock:
// hello and probe deadlines, backoff, and kick(), which unlike the registry's
// never redials over a handshake in flight.

import XCTest
@testable import Cypher

@MainActor
final class ChatRoomClientLifecycleTests: XCTestCase {
    @MainActor private final class Cursor {
        var value: UInt64 = 0
    }

    private let clock = ManualRoomClock()
    private let transport = FakeWebSocketTransport()
    private let log = EventLog()
    private var client: ChatRoomClient!

    private static let ms: UInt64 = 1_000_000
    private static let state = ChatWire.encode(ChatFrameType.state, header: [
        "headSeq": 0, "seqFloor": 0, "checkpointSeq": 0, "checkpointSize": 0, "rowCount": 0, "rowBytes": 0,
    ])
    private static let rowsDone = ChatWire.encode(ChatFrameType.rowsDone, header: [:])

    override func setUp() async throws {
        let log = log
        let cursor = Cursor()
        let delegate = ChatRoomClient.Delegate(
            cursor: { cursor.value },
            containsFrontier: { _ in true },
            applyCheckpoint: { _, _ in true },
            applyRow: { _, seq in cursor.value = seq },
            advanceCursor: { cursor.value = $0 },
            clampCursor: { cursor.value = $0 },
            setCursor: { cursor.value = $0 },
            event: { log.append($0) })
        client = ChatRoomClient(
            chatId: "chat", device: "phone",
            urlProvider: { URL(string: "wss://edge.test/chat2/chat/ws") },
            checkpointRequest: { nil }, rowsRequest: { _ in nil }, pushRequest: { _ in nil },
            delegate: delegate, transport: transport, clock: clock)
    }

    override func tearDown() async throws {
        await client.stop()
    }

    private func kinds(_ socket: FakeWebSocket) -> [UInt8] {
        socket.sentData.compactMap { ChatWire.decode($0)?.kind }
    }

    private func probes(_ socket: FakeWebSocket) -> Int {
        kinds(socket).filter { $0 == ChatFrameType.probe }.count
    }

    private func waitForHello(_ socket: FakeWebSocket) async {
        await eventually { self.kinds(socket).contains(ChatFrameType.hello) }
    }

    private func open() async -> FakeWebSocket {
        await client.start()
        await eventually { transport.sockets.count == 1 }
        let socket = transport.sockets[0]
        await waitForHello(socket)
        return socket
    }

    /// Answers the hello; the client then requests rows (the backfill).
    private func answerHello(_ socket: FakeWebSocket) async {
        socket.deliver(.data(Self.state))
        await eventually { self.kinds(socket).contains(ChatFrameType.rowsReq) }
    }

    private func join(_ socket: FakeWebSocket) async {
        let joins = log.count("connected")
        await answerHello(socket)
        socket.deliver(.data(Self.rowsDone))
        await eventually { log.count("connected") == joins + 1 }
    }

    private func advance(seconds: Int) async {
        for _ in 0..<seconds {
            clock.advance(nanoseconds: 1_000 * Self.ms)
            await settle()
        }
    }

    private func advance(upTo seconds: Int, until condition: () -> Bool) async {
        for _ in 0..<seconds where !condition() {
            await advance(seconds: 1)
        }
        XCTAssertTrue(condition(), "still waiting after \(seconds)s")
    }

    private func expectRedial(after delayMs: UInt64, sockets expected: Int) async {
        await eventually("backoff timer of \(delayMs)ms never armed") { clock.hasSleeper(lasting: delayMs * Self.ms) }
        clock.advance(nanoseconds: (delayMs - 1) * Self.ms)
        await settle()
        XCTAssertEqual(transport.sockets.count, expected - 1, "redialed before \(delayMs)ms")
        clock.advance(nanoseconds: Self.ms)
        await eventually("no redial after \(delayMs)ms") { transport.sockets.count == expected }
    }

    func testUnansweredHelloRedialsAfterItsDeadline() async {
        let socket = await open()
        await advance(seconds: 14)
        XCTAssertNil(socket.closeCode)
        await advance(upTo: 3) { socket.closeCode == .abnormalClosure }
        XCTAssertGreaterThanOrEqual(log.count("disconnected"), 1)
        await expectRedial(after: 250, sockets: 2)
    }

    func testUnansweredProbeRedialsAfterItsDeadline() async {
        let socket = await open()
        await join(socket)
        await client.kick()
        XCTAssertEqual(probes(socket), 1)
        await advance(seconds: 9)
        XCTAssertNil(socket.closeCode)
        await advance(upTo: 3) { socket.closeCode == .abnormalClosure }
    }

    func testAnsweredProbeKeepsTheSession() async {
        let socket = await open()
        await join(socket)
        await client.kick()
        socket.deliver(.data(ChatWire.encode(ChatFrameType.probeOk, header: [:])))
        await settle()
        await advance(seconds: 12)
        XCTAssertNil(socket.closeCode)
        XCTAssertEqual(transport.sockets.count, 1)
    }

    func testBackoffDoublesPerFailureAndResetsOnJoin() async {
        let first = await open()
        first.fail()
        await expectRedial(after: 250, sockets: 2)
        transport.sockets[1].fail()
        await expectRedial(after: 500, sockets: 3)
        let third = transport.sockets[2]
        await waitForHello(third)
        await join(third)
        third.fail()
        await expectRedial(after: 250, sockets: 4)
    }

    func testKickNeverRedialsOverAHandshakeInFlight() async {
        let socket = await open()
        await client.kick()
        await settle()
        XCTAssertEqual(transport.sockets.count, 1, "hello pending: its own deadline polices it")
        await answerHello(socket)
        await client.kick()
        await settle()
        XCTAssertEqual(transport.sockets.count, 1, "backfill pending: its own deadline polices it")
        XCTAssertNil(socket.closeCode)
    }

    func testKickDialsADeadSessionImmediately() async {
        let first = await open()
        first.fail()
        await eventually { clock.hasSleeper(lasting: 250 * Self.ms) }
        await client.kick()
        await eventually { transport.sockets.count == 2 }
    }

    func testKickProbesAJoinedSessionOnce() async {
        let socket = await open()
        await join(socket)
        await client.kick()
        await client.kick()
        await settle()
        XCTAssertEqual(probes(socket), 1, "an outstanding probe is already policed")
        XCTAssertEqual(transport.sockets.count, 1)
    }

    func testKickRestartsBackoff() async {
        let first = await open()
        first.fail()
        await expectRedial(after: 250, sockets: 2)
        transport.sockets[1].fail()
        await eventually { clock.hasSleeper(lasting: 500 * Self.ms) }
        await client.kick()
        await eventually { transport.sockets.count == 3 }
        transport.sockets[2].fail()
        await expectRedial(after: 250, sockets: 4)
    }
}

// RegistryClient socket lifecycle against a fake transport and manual clock:
// hello and probe deadlines, backoff, the generation guard and kick().

import XCTest
@testable import Cypher

@MainActor
final class RegistryClientLifecycleTests: XCTestCase {
    private let clock = ManualRoomClock()
    private let transport = FakeWebSocketTransport()
    private let log = EventLog()
    private var client: RegistryClient!

    private static let state = #"{"t":"state","seq":1,"full":true,"rows":[]}"#
    private static let ms: UInt64 = 1_000_000

    override func setUp() async throws {
        let log = log
        client = RegistryClient(
            device: "phone",
            urlProvider: { URL(string: "wss://edge.test/registry/org/ws") },
            rowsRequest: { _ in nil },
            pushRequest: { nil },
            delegate: .init(helloCursor: { nil }, takePushable: { [] }, resetPushable: {},
                            acknowledge: { _, _ in }, event: { log.append($0) }),
            transport: transport, clock: clock)
    }

    override func tearDown() async throws {
        await client.stop()
    }

    private func open() async -> FakeWebSocket {
        await client.start()
        await eventually { transport.last?.sentStrings.contains { $0.contains(#""t":"hello""#) } == true }
        return transport.sockets[0]
    }

    private func join(_ socket: FakeWebSocket) async {
        let joins = log.count("connected")
        socket.deliver(.string(Self.state))
        await eventually { log.count("connected") == joins + 1 }
    }

    private func advance(seconds: Int) async {
        for _ in 0..<seconds {
            clock.advance(nanoseconds: 1_000 * Self.ms)
            await settle()
        }
    }

    /// Steps time a second at a time until `condition` holds.
    private func advance(upTo seconds: Int, until condition: () -> Bool) async {
        for _ in 0..<seconds where !condition() {
            await advance(seconds: 1)
        }
        XCTAssertTrue(condition(), "still waiting after \(seconds)s")
    }

    /// Moves time past a reconnect delay once its timer is armed.
    private func expectRedial(after delayMs: UInt64, sockets expected: Int) async {
        await eventually("backoff timer of \(delayMs)ms never armed") { clock.hasSleeper(lasting: delayMs * Self.ms) }
        clock.advance(nanoseconds: (delayMs - 1) * Self.ms)
        await settle()
        XCTAssertEqual(transport.sockets.count, expected - 1, "redialed before \(delayMs)ms")
        clock.advance(nanoseconds: Self.ms)
        await eventually("no redial after \(delayMs)ms") { transport.sockets.count == expected }
    }

    private func probes(_ socket: FakeWebSocket) -> Int {
        socket.sentStrings.filter { $0.contains(#""t":"probe""#) }.count
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
        socket.deliver(.string(#"{"t":"probe-ok"}"#))
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
        await eventually { third.sentStrings.contains { $0.contains(#""t":"hello""#) } }
        await join(third)
        third.fail()
        await expectRedial(after: 250, sockets: 4)
    }

    func testKickRedialsAnUnjoinedSessionImmediately() async {
        _ = await open()
        await client.kick()
        await eventually { transport.sockets.count == 2 }
    }

    func testFramesFromASupersededSocketAreIgnored() async {
        let first = await open()
        await client.kick()
        await eventually { transport.sockets.count == 2 }
        let second = transport.sockets[1]
        first.deliver(.string(Self.state))
        await settle()
        XCTAssertEqual(log.count("connected"), 0, "a stale socket's state must not join")
        XCTAssertNil(second.closeCode)
        XCTAssertEqual(transport.sockets.count, 2)
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

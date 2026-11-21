// DeviceRelayClient link health: a link that stops answering is replaced and
// its calls are sent again, instead of every later call timing out on it.

import XCTest
@testable import Cypher

/// A socket whose pending receive outlives `cancel`, as a URLSession callback
/// can: it fails only when the test says so.
private final class LingeringSocket: WebSocketConnection, @unchecked Sendable {
    private let lock = NSLock()
    private var waiter: CheckedContinuation<URLSessionWebSocketTask.Message, Error>?
    private var _cancelled = false

    var cancelled: Bool { lock.withLock { _cancelled } }
    var receiving: Bool { lock.withLock { waiter != nil } }

    func send(_ message: URLSessionWebSocketTask.Message) async throws {}

    func receive() async throws -> URLSessionWebSocketTask.Message {
        try await withCheckedThrowingContinuation { continuation in
            lock.withLock { waiter = continuation }
        }
    }

    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        lock.withLock { _cancelled = true }
    }

    /// The receive error arrives, late.
    func failNow() {
        let receiver = lock.withLock { () -> CheckedContinuation<URLSessionWebSocketTask.Message, Error>? in
            defer { waiter = nil }
            return waiter
        }
        receiver?.resume(throwing: FakeSocketClosed())
    }
}

/// Hands out prepared sockets first, then fakes.
private final class PreparedTransport: WebSocketTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var prepared: [any WebSocketConnection]
    private var _opened: [any WebSocketConnection] = []

    init(_ prepared: [any WebSocketConnection]) { self.prepared = prepared }

    var opened: [any WebSocketConnection] { lock.withLock { _opened } }

    func open(_ request: URLRequest) -> any WebSocketConnection {
        lock.withLock {
            let socket = prepared.isEmpty ? FakeWebSocket() : prepared.removeFirst()
            _opened.append(socket)
            return socket
        }
    }
}

@MainActor
final class DeviceRelayClientTests: XCTestCase {
    private let transport = FakeWebSocketTransport()
    private let clock = ManualRoomClock()
    private var clients: [DeviceRelayClient] = []

    override func tearDown() async throws {
        // Close the links and let every timer they armed run out.
        for client in clients { await client.close() }
        clients = []
        clock.advance(nanoseconds: 600_000_000_000)
        await settle()
    }

    private func config() -> AppConfig {
        AppConfig(
            edgeURL: URL(string: "http://localhost:1")!, mode: .dev, userId: "u", orgId: "o",
            deviceId: "phone", deviceName: "Phone", devBearer: "dev-token")
    }

    private func client(transport: (any WebSocketTransport)? = nil) -> DeviceRelayClient {
        let client = DeviceRelayClient(
            deviceId: "mac", config: config(), transport: transport ?? self.transport, clock: clock)
        clients.append(client)
        return client
    }

    /// The RPC frames a socket was sent, as JSON objects.
    private func requests(_ socket: FakeWebSocket) -> [[String: Any]] {
        socket.sentData.compactMap { frame in
            guard let (header, payload) = DeviceRelayClient.decodeFrame(frame), header.k == DeviceRelayClient.rpcKind
            else { return nil }
            return try? JSONSerialization.jsonObject(with: payload) as? [String: Any]
        }
    }

    private func methods(_ socket: FakeWebSocket) -> [String] {
        requests(socket).compactMap { $0["method"] as? String }
    }

    /// The host answers the socket's request number `index`.
    private func answer(_ socket: FakeWebSocket, request index: Int, ok: Any) throws {
        let id = try XCTUnwrap(requests(socket)[index]["id"])
        let payload = try JSONSerialization.data(withJSONObject: ["id": id, "ok": ok])
        socket.deliver(.data(DeviceRelayClient.encodeFrame(header: DeviceRelayClient.rpcHeader, payload: payload)))
    }

    /// The host answers every probe it has not answered yet.
    private func answerProbes(_ socket: FakeWebSocket, answered: inout Set<Int>) throws {
        for (index, request) in requests(socket).enumerated()
        where request["method"] as? String == DeviceRelayClient.probeMethod && answered.insert(index).inserted {
            try answer(socket, request: index, ok: [Any]())
        }
    }

    private func refuse(_ socket: FakeWebSocket, reason: String) {
        let payload = Data([DeviceRelayClient.refused]) + Data(reason.utf8)
        socket.deliver(.data(DeviceRelayClient.encodeFrame(header: DeviceRelayClient.noiseHeader, payload: payload)))
    }

    private func relayError(_ socket: FakeWebSocket, code: String) {
        socket.deliver(
            .data(
                DeviceRelayClient.encodeFrame(
                    header: #"{"s":"","k":" relay"}"#, payload: Data(#"{"error":"\#(code)"}"#.utf8))))
    }

    /// Lets the pause before a call is sent again run out.
    private func passRetryPause() async {
        let pause = DeviceRelayClient.Timing().retrySpacingNs
        await eventually("the call was not queued to be sent again") { clock.hasSleeper(lasting: pause) }
        clock.advance(nanoseconds: pause)
    }

    func testACallIsSentAgainOnAFreshLinkWhenTheDeviceStopsAnswering() async throws {
        let client = client()
        let call = Task { try await client.call(method: "ListModels", params: ["harness": "pi"]) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }
        let first = try XCTUnwrap(transport.last)

        // Three quiet seconds with the call waiting: is the device there?
        for _ in 0..<3 { await tick(clock) }
        await eventually("no probe was sent") { first.sentData.count == 2 }
        XCTAssertEqual(methods(first), ["ListModels", DeviceRelayClient.probeMethod])
        XCTAssertNil(first.closeCode, "a probe in flight is not a verdict")

        // Four more without a word: the link is dead, whatever the socket says.
        for _ in 0..<4 { await tick(clock, unless: { first.closeCode != nil }) }
        await eventually("the silent link was kept") { first.closeCode != nil }

        await passRetryPause()
        await eventually("the call was not sent again") {
            self.transport.sockets.count == 2 && self.transport.last?.sentData.count == 1
        }
        let second = try XCTUnwrap(transport.last)
        XCTAssertEqual(methods(second), ["ListModels"])
        try answer(second, request: 0, ok: ["model-1"])
        let models = try await call.value
        XCTAssertEqual(models, ["model-1"])
    }

    func testAnAnsweredProbeKeepsTheLinkOfASlowCall() async throws {
        let client = client()
        let call = Task { try await client.call(method: "ForkSession", params: [:], timeoutSeconds: 90) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }
        let socket = try XCTUnwrap(transport.last)

        // Half a minute of a host that answers probes and nothing else.
        var answered = Set<Int>()
        for _ in 0..<30 {
            await tick(clock)
            try answerProbes(socket, answered: &answered)
            await settle()
        }
        XCTAssertFalse(answered.isEmpty, "the quiet link was never probed")
        XCTAssertNil(socket.closeCode, "a link whose device answers is kept")
        XCTAssertEqual(transport.sockets.count, 1)

        try answer(socket, request: 0, ok: ["forked"])
        let reply = try await call.value
        XCTAssertEqual(reply, ["forked"])
    }

    func testACallTheDeviceNeverAnswersStillTimesOutOnALiveLink() async throws {
        let client = client()
        let call = Task { try await client.call(method: "ListModels", params: [:]) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }
        let socket = try XCTUnwrap(transport.last)

        var answered = Set<Int>()
        var failure: Error?
        let outcome = Task { () -> Error? in
            do {
                _ = try await call.value
                return nil
            } catch { return error }
        }
        for _ in 0..<10 {
            await tick(clock)
            try answerProbes(socket, answered: &answered)
            await settle()
        }
        failure = await outcome.value
        guard case .timeout? = failure as? RelayError else {
            return XCTFail("expected a timeout, got \(String(describing: failure))")
        }
        XCTAssertNil(socket.closeCode, "a slow call is not a dead link")
        XCTAssertEqual(transport.sockets.count, 1)
    }

    func testASessionTheDeviceNoLongerHoldsIsDialedAgain() async throws {
        let client = client()
        let call = Task { try await client.call(method: "ListModels", params: [:]) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }
        let first = try XCTUnwrap(transport.last)

        // The host re-joined the relay and holds no session for this link.
        refuse(first, reason: "secure_channel_failed")
        await eventually("the lost session's link was kept") { first.closeCode != nil }

        await passRetryPause()
        await eventually("the call was not sent again") {
            self.transport.sockets.count == 2 && self.transport.last?.sentData.count == 1
        }
        try answer(try XCTUnwrap(transport.last), request: 0, ok: ["model-1"])
        let models = try await call.value
        XCTAssertEqual(models, ["model-1"])
    }

    func testARefusalByPolicyIsReportedNotRetried() async throws {
        let client = client()
        let call = Task { try await client.call(method: "ListModels", params: [:]) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }

        refuse(try XCTUnwrap(transport.last), reason: "untrusted_device")
        do {
            _ = try await call.value
            XCTFail("a refused device answered")
        } catch RelayError.notPaired(let message) {
            XCTAssertTrue(message.contains("isn't paired"), message)
        }
        await settle()
        XCTAssertEqual(transport.sockets.count, 1, "dialing again changes nothing about the host's policy")
    }

    func testTheNextCallDialsAgainAfterTheRelaySaysTheHostWentAway() async throws {
        let client = client()
        let one = Task { try await client.call(method: "ListHarnesses", params: [:]) as [String] }
        await eventually("the call never left") { self.transport.last?.sentData.count == 1 }
        let first = try XCTUnwrap(transport.last)
        try answer(first, request: 0, ok: [Any]())
        _ = try await one.value

        // Idle link; the host's socket is replaced and the relay says so.
        relayError(first, code: "host_closed")
        await eventually("the ended link was kept") { first.closeCode != nil }

        let two = Task { try await client.call(method: "ListModels", params: [:]) as [String] }
        await eventually("the call did not dial a fresh link") {
            self.transport.sockets.count == 2 && self.transport.last?.sentData.count == 1
        }
        try answer(try XCTUnwrap(transport.last), request: 0, ok: ["model-1"])
        let models = try await two.value
        XCTAssertEqual(models, ["model-1"])
    }

    func testALateFailureOfAnEndedLinkLeavesItsSuccessorAlone() async throws {
        let old = LingeringSocket()
        let transport = PreparedTransport([old])
        let client = client(transport: transport)
        let call = Task { try await client.call(method: "ListModels", params: [:]) as [String] }
        await eventually("the first link never opened") { old.receiving }

        // The link ends under the call, which is sent again on a new one.
        await client.close()
        XCTAssertTrue(old.cancelled)
        await passRetryPause()
        await eventually("the call was not sent again") {
            (transport.opened.last as? FakeWebSocket)?.sentData.count == 1
        }
        let current = try XCTUnwrap(transport.opened.last as? FakeWebSocket)

        // The first socket's receive error arrives only now.
        old.failNow()
        await settle()
        XCTAssertNil(current.closeCode, "an ended link's failure tore down the link that replaced it")
        XCTAssertEqual(transport.opened.count, 2)

        try answer(current, request: 0, ok: ["model-1"])
        let models = try await call.value
        XCTAssertEqual(models, ["model-1"])
    }
}

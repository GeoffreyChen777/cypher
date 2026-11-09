// ChatRoomClient catch-up over a slow link: the catch-up deadline follows
// progress rather than total time, an honest cursor skips the amnesty
// refetch, and a checkpoint download outlives the socket that asked for it —
// a redial joins it, and a cut-off one resumes by Range.

import XCTest
@testable import Cypher

/// Serves `https://edge.test/…/checkpoint` from a script. A reply can stop
/// partway (`hold`) until the test finishes it, or drop the connection.
final class CheckpointStub: URLProtocol, @unchecked Sendable {
    enum Reply {
        /// Status, body, and what follows the body: finish, drop, or wait for `finish()`.
        case body(status: Int, data: Data, end: End)
        /// The connection fails before any response.
        case fail
    }

    enum End { case finish, drop, hold }

    private static let lock = NSLock()
    nonisolated(unsafe) private static var replies: [Reply] = []
    nonisolated(unsafe) private static var _ranges: [String?] = []
    nonisolated(unsafe) private static var held: CheckpointStub?

    /// The Range header of every request, in order (nil: a fresh download).
    static var ranges: [String?] { lock.withLock { _ranges } }

    static func reset(_ script: [Reply]) {
        lock.withLock {
            replies = script
            _ranges = []
            held = nil
        }
    }

    /// Whether a reply is waiting on `send`/`finish`.
    static var holding: Bool { lock.withLock { held != nil } }

    /// More of the held reply's body.
    static func send(_ data: Data) {
        guard let stub = lock.withLock({ held }) else { return }
        stub.onLoadingThread { $0.client?.urlProtocol($0, didLoad: data) }
    }

    /// Sends the last of the held reply's body and finishes it.
    static func finish(_ rest: Data = Data()) {
        guard let stub = lock.withLock({ () -> CheckpointStub? in
            defer { held = nil }
            return held
        }) else { return }
        stub.onLoadingThread {
            $0.client?.urlProtocol($0, didLoad: rest)
            $0.client?.urlProtocolDidFinishLoading($0)
        }
    }

    /// The held reply's connection drops.
    static func drop() {
        guard let stub = lock.withLock({ () -> CheckpointStub? in
            defer { held = nil }
            return held
        }) else { return }
        stub.onLoadingThread { $0.client?.urlProtocol($0, didFailWithError: URLError(.networkConnectionLost)) }
    }

    /// URLProtocol's client is called on the thread that started loading.
    private var thread: Thread?
    private final class Work: NSObject {
        let run: (CheckpointStub) -> Void
        init(_ run: @escaping (CheckpointStub) -> Void) { self.run = run }
    }

    private func onLoadingThread(_ run: @escaping (CheckpointStub) -> Void) {
        guard let thread else { return }
        perform(#selector(runWork(_:)), on: thread, with: Work(run), waitUntilDone: false)
    }

    @objc private func runWork(_ work: Work) {
        work.run(self)
    }

    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "edge.test" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        thread = Thread.current
        let reply = Self.lock.withLock { () -> Reply in
            Self._ranges.append(request.value(forHTTPHeaderField: "Range"))
            return Self.replies.isEmpty ? .fail : Self.replies.removeFirst()
        }
        guard case .body(let status, let data, let end) = reply else {
            client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost))
            return
        }
        let response = HTTPURLResponse(
            url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
            // Typed as the Edge types it, and past URLSession's sniffing,
            // which would hold a short body's first 512 bytes back.
            headerFields: [
                "x-chat2-checkpoint-seq": "10", "content-type": "application/octet-stream",
                "x-content-type-options": "nosniff",
            ])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        switch end {
        case .finish: client?.urlProtocolDidFinishLoading(self)
        case .drop: client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost))
        case .hold: Self.lock.withLock { Self.held = self }
        }
    }

    override func stopLoading() {}
}

@MainActor
final class ChatRoomCatchUpTests: XCTestCase {
    @MainActor private final class Doc {
        var cursor: UInt64 = 0
        var honest = false
        var contains = false
        var checkpoints: [Data] = []
    }

    private let clock = ManualRoomClock()
    private let transport = FakeWebSocketTransport()
    private let log = EventLog()
    private let doc = Doc()
    private var client: ChatRoomClient!

    private static let ms: UInt64 = 1_000_000
    private static let checkpoint = Data((0..<10_000).map { UInt8($0 % 251) })

    override func setUp() async throws {
        URLProtocol.registerClass(CheckpointStub.self)
        CheckpointStub.reset([])
        let doc = doc
        let log = log
        let delegate = ChatRoomClient.Delegate(
            cursor: { doc.cursor },
            cursorHonest: { doc.honest },
            containsFrontier: { _ in doc.contains },
            applyCheckpoint: { bytes, seq in
                doc.checkpoints.append(bytes)
                doc.contains = true
                doc.cursor = max(doc.cursor, seq)
                return true
            },
            applyRow: { _, seq in doc.cursor = max(doc.cursor, seq) },
            advanceCursor: { doc.cursor = $0 },
            clampCursor: { doc.cursor = $0 },
            setCursor: { doc.cursor = $0 },
            event: { log.append($0) })
        client = ChatRoomClient(
            chatId: "chat", device: "phone",
            urlProvider: { URL(string: "wss://edge.test/chat2/chat/ws") },
            checkpointRequest: { URLRequest(url: URL(string: "https://edge.test/chat2/chat/checkpoint")!) },
            rowsRequest: { _ in nil }, pushRequest: { _ in nil },
            delegate: delegate, transport: transport, clock: clock)
    }

    override func tearDown() async throws {
        await client.stop()
        CheckpointStub.finish()
        URLProtocol.unregisterClass(CheckpointStub.self)
    }

    private func frames(_ socket: FakeWebSocket, _ kind: UInt8) -> [ChatWireFrame] {
        socket.sentData.compactMap(ChatWire.decode).filter { $0.kind == kind }
    }

    private func state(checkpointSeq: UInt64 = 10, checkpointSize: Int = 10_000, headSeq: UInt64 = 60) -> Data {
        ChatWire.encode(
            ChatFrameType.state,
            header: [
                "headSeq": headSeq, "seqFloor": checkpointSeq, "checkpointSeq": checkpointSeq,
                "checkpointSize": checkpointSize, "rowCount": 0, "rowBytes": 0,
            ],
            payload: Data([1]))
    }

    private func row(_ seq: UInt64) -> Data {
        ChatWire.encode(ChatFrameType.row, header: ["seq": seq, "device": "mac", "batchId": "b\(seq)"])
    }

    private func socket(_ index: Int) async -> FakeWebSocket {
        await eventually("socket \(index) never opened") { transport.sockets.count > index }
        let socket = transport.sockets[index]
        await eventually("no hello on socket \(index)") { !self.frames(socket, ChatFrameType.hello).isEmpty }
        return socket
    }

    private func rowsReqAfter(_ socket: FakeWebSocket) async -> UInt64? {
        await eventually("no rows request") { !self.frames(socket, ChatFrameType.rowsReq).isEmpty }
        return (frames(socket, ChatFrameType.rowsReq).last?.header["after"] as? NSNumber)?.uint64Value
    }

    func testABackfillThatKeepsDrainingOutlivesTheDeadline() async {
        doc.contains = true
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state(checkpointSize: 0)))
        _ = await rowsReqAfter(first)
        // 150 seconds of rows, one every 10: past 120s in total, never 120s quiet.
        for seq in 1...15 {
            first.deliver(.data(row(UInt64(seq))))
            await settle()
            for _ in 0..<10 { await tick(clock) }
        }
        XCTAssertNil(first.closeCode, "a backfill still landing rows was redialed")
        XCTAssertEqual(transport.sockets.count, 1)
        XCTAssertTrue(log.events.contains("catchingUp(true)"))
        first.deliver(.data(ChatWire.encode(ChatFrameType.rowsDone, header: [:])))
        await eventually { self.log.count("connected") == 1 }
        XCTAssertEqual(log.events.suffix(2), ["catchingUp(false)", "connected"])
    }

    func testABackfillWithNoProgressRedials() async {
        doc.contains = true
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state(checkpointSize: 0)))
        _ = await rowsReqAfter(first)
        // Pongs keep the socket's lease; nothing moves the catch-up along.
        for second in 1...119 {
            if second % 10 == 0 { first.deliver(.string("pong")) }
            await tick(clock)
        }
        XCTAssertNil(first.closeCode)
        for _ in 0..<3 where first.closeCode == nil { await tick(clock) { first.closeCode != nil } }
        await eventually { first.closeCode == .abnormalClosure }
        await eventually { self.log.events.contains("catchingUp(false)") }
    }

    func testAnHonestCursorSkipsTheAmnestyRefetch() async {
        doc.cursor = 50
        doc.honest = true
        doc.contains = true
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state()))
        let after = await rowsReqAfter(first)
        XCTAssertEqual(after, 50, "only the rows after the honest cursor")
    }

    func testAnUnvouchedCursorIsClampedToTheCheckpoint() async {
        doc.cursor = 50
        doc.contains = true
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state()))
        let after = await rowsReqAfter(first)
        XCTAssertEqual(after, 10, "every row since the checkpoint, once")
    }

    func testASlowCheckpointDownloadKeepsItsSocket() async {
        let bytes = Self.checkpoint
        CheckpointStub.reset([.body(status: 200, data: bytes.prefix(1000), end: .hold)])
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state()))
        await eventually("no checkpoint request") { CheckpointStub.holding }
        // 500 bytes every 10 seconds: 180 seconds for the rest, and the
        // socket's own frames unread all the while.
        for chunk in 0..<18 {
            for _ in 0..<10 { await tick(clock) }
            let start = 1000 + chunk * 500
            CheckpointStub.send(bytes.subdata(in: start..<(start + 500)))
            await settle()
        }
        XCTAssertNil(first.closeCode, "a download still moving was cut off")
        CheckpointStub.finish()
        let after = await rowsReqAfter(first)
        XCTAssertEqual(after, 10)
        XCTAssertEqual(transport.sockets.count, 1)
        XCTAssertEqual(doc.checkpoints, [bytes])
    }

    func testARedialJoinsTheCheckpointDownloadInFlight() async {
        let bytes = Self.checkpoint
        CheckpointStub.reset([.body(status: 200, data: bytes.prefix(4000), end: .hold)])
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state()))
        await eventually("no checkpoint request") { CheckpointStub.holding }
        // The download stalls: the socket's lease runs out and it redials.
        for _ in 0..<60 where first.closeCode == nil {
            await tick(clock) { first.closeCode != nil }
        }
        await eventually { first.closeCode == .abnormalClosure }
        await eventually { clock.hasSleeper(lasting: 250 * Self.ms) }
        clock.advance(nanoseconds: 250 * Self.ms)
        let second = await socket(1)
        second.deliver(.data(state()))
        await settle()
        CheckpointStub.finish(bytes.subdata(in: 4000..<bytes.count))
        let after = await rowsReqAfter(second)
        XCTAssertEqual(after, 10)
        XCTAssertEqual(CheckpointStub.ranges, [nil], "one download, not one per socket")
        XCTAssertEqual(doc.checkpoints, [bytes], "imported once, whole")
        XCTAssertTrue(frames(first, ChatFrameType.rowsReq).isEmpty)
    }

    func testACutOffCheckpointDownloadResumesWhereItStopped() async {
        let bytes = Self.checkpoint
        // The first load gets 4000 bytes and then no answer at all: its
        // attempts run out and the session redials.
        CheckpointStub.reset([
            .body(status: 200, data: bytes.prefix(4000), end: .hold), .fail, .fail, .fail,
            .body(status: 206, data: bytes.subdata(in: 4000..<bytes.count), end: .finish),
        ])
        await client.start()
        let first = await socket(0)
        first.deliver(.data(state()))
        await eventually("no checkpoint request") { CheckpointStub.holding }
        try? await Task.sleep(for: .milliseconds(100))
        CheckpointStub.drop()
        await eventually("the first load never gave up") { first.closeCode == .abnormalClosure }
        XCTAssertTrue(doc.checkpoints.isEmpty)
        await eventually { clock.hasSleeper(lasting: 250 * Self.ms) }
        clock.advance(nanoseconds: 250 * Self.ms)
        let second = await socket(1)
        second.deliver(.data(state()))
        let after = await rowsReqAfter(second)
        XCTAssertEqual(after, 10)
        XCTAssertEqual(CheckpointStub.ranges, [nil, "bytes=4000-", "bytes=4000-", "bytes=4000-", "bytes=4000-"])
        XCTAssertEqual(doc.checkpoints, [bytes], "resumed, not restarted")
    }
}

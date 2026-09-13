// Direct v3 WS client. Experimental until SessionStore/migration integration.
import Foundation

@MainActor
final class Sync3Client {
    struct Tuning {
        var deadline: Duration = .seconds(15)
        var probeInterval: Duration = .seconds(60)
        var pingInterval: Duration = .seconds(20)
        var tickInterval: Duration = .seconds(1)
        var retryBase: Double = 0.5
        var retryCap: Double = 60
    }
    struct Status: Equatable {
        var phase: String = "offline"
        var generation: UInt64 = 0
        var cursor: Int64 = 0
        var repairs: UInt64 = 0
        var error: String?
    }
    private enum Signal: Sendable {
        case message(String), wake, tick, disconnected, protocolFailure(String)
    }
    private final class WakeBox {
        var stream: AsyncStream<Signal>.Continuation?
        func send() { _ = stream?.yield(.wake) }
    }
    let journal: Sync3Journal
    private(set) var status = Status()
    var onChange: (() -> Void)?
    private let wakeBox = WakeBox()
    private var task: Task<Void, Never>?

    /// URL request resolves credentials before each connection, not once at
    /// construction. It must point at the account-scoped v3 endpoint.
    init(journal: Sync3Journal, request: @escaping @MainActor () async throws -> URLRequest,
         repair: (@MainActor ([String: JSONValue]) async throws -> [String: JSONValue])? = nil,
         tuning: Tuning = Tuning()) {
        precondition(tuning.deadline > .zero && tuning.probeInterval > .zero &&
                     tuning.pingInterval > .zero && tuning.tickInterval > .zero &&
                     tuning.retryBase > 0 && tuning.retryCap >= tuning.retryBase)
        self.journal = journal
        let wake = self.wakeBox
        task = Task { [weak self, journal, wake] in
            var generation: UInt64 = 0
            var repairs: UInt64 = 0
            var retry = tuning.retryBase
            let publish: (String, String?) -> Void = { [weak self] phase, error in
                self?.status = Status(phase: phase, generation: generation,
                                     cursor: (try? journal.cursor) ?? 0, repairs: repairs, error: error)
                self?.onChange?()
            }
            while !Task.isCancelled {
                generation += 1
                publish("connecting", nil)
                let started = ContinuousClock.now
                do {
                    let authRequest = try await request()
                    try Task.checkCancellation()
                    try await Self.session(journal: journal, wake: wake, request: authRequest, tuning: tuning, publish: publish)
                } catch is CancellationError { break }
                catch {
                    if Task.isCancelled { break }
                    let code: String
                    if case Sync3Error.protocolError(let problem) = error { code = problem }
                    else { code = "transport_unavailable" }
                    publish("suspect", code)
                    // Never auto-reset state or regenerate commands on a
                    // semantic conflict. Keep the durable queue for recovery.
                    if !["transport_unavailable", "business_timeout", "reauth_required", "not_initialized"].contains(code) { return }
                    if let repair, !Task.isCancelled {
                        repairs += 1; publish("repairing", nil)
                        do {
                            try await Self.repairBounded(journal: journal, exchange: repair, deadline: tuning.deadline * 2)
                        }
                        catch is CancellationError { break }
                        catch {
                            if case Sync3Error.protocolError(let problem) = error,
                               !["transport_unavailable", "business_timeout", "reauth_required", "not_initialized"].contains(problem) {
                                publish("suspect", problem); return
                            }
                        }
                    }
                }
                if started.duration(to: .now) > tuning.probeInterval { retry = tuning.retryBase }
                try? await Task.sleep(for: .milliseconds(Int((retry + Double.random(in: 0...0.25)) * 1000)))
                retry = min(retry * 2, tuning.retryCap)
            }
            publish("offline", nil)
        }
    }
    deinit { task?.cancel() }
    func enqueue(_ operation: Sync3Operation) throws {
        try journal.enqueue(operation)
        wakeBox.send()
    }

    /// Wake the transport after a caller durably changes the journal.
    func wake() {
        wakeBox.send()
    }
    func stop() async {
        task?.cancel()
        await task?.value
        task = nil
    }

    private static func session(
        journal: Sync3Journal, wake: WakeBox, request: URLRequest, tuning: Tuning,
        publish: (String, String?) -> Void
    ) async throws {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 15
        let session = URLSession(configuration: config, delegate: V3NoRedirect(), delegateQueue: nil)
        let socket = session.webSocketTask(with: request)
        socket.maximumMessageSize = Sync3Wire.maxFrameBytes
        let (signals, continuation) = AsyncStream<Signal>.makeStream(bufferingPolicy: .bufferingOldest(64))
        wake.stream = continuation
        socket.resume()
        let reader = Task {
            do {
                while !Task.isCancelled {
                    let message = try await socket.receive()
                    guard case .string(let text) = message else {
                        _ = continuation.yield(.protocolFailure("expected_json")); break
                    }
                    // Bounded buffers never silently discard reliable frames:
                    // on overflow, end this connection and resume from disk.
                    if case .dropped = continuation.yield(.message(text)) { break }
                }
            } catch {}
            _ = continuation.yield(.disconnected)
            continuation.finish()
        }
        let ticker = Task {
            while !Task.isCancelled {
                do { try await Task.sleep(for: tuning.tickInterval) } catch { break }
                if case .dropped = continuation.yield(.tick) { continuation.finish(); break }
            }
        }
        defer {
            wake.stream = nil
            continuation.finish(); reader.cancel(); ticker.cancel()
            socket.cancel(with: .goingAway, reason: nil)
            session.invalidateAndCancel()
        }
        try await send(socket, journal.hello(), deadline: tuning.deadline)
        var handshake: ContinuousClock.Instant? = .now.advanced(by: tuning.deadline)
        var pull: (through: Int64, deadline: ContinuousClock.Instant)?
        var push: (ids: [String], deadline: ContinuousClock.Instant)?
        var probeDeadline: ContinuousClock.Instant?
        var nextProbe = ContinuousClock.now.advanced(by: tuning.probeInterval)
        var nextPing = ContinuousClock.now.advanced(by: tuning.pingInterval)
        var ready = false, head: Int64 = 0
        for await signal in signals {
            try Task.checkCancellation()
            switch signal {
            case .disconnected: try Sync3Wire.fail("transport_unavailable")
            case .protocolFailure(let code): try Sync3Wire.fail(code)
            case .wake: break
            case .tick:
                let now = ContinuousClock.now
                if let deadline = [handshake, pull?.deadline, push?.deadline, probeDeadline].compactMap({ $0 }).min(), now >= deadline {
                    try Sync3Wire.fail("business_timeout")
                }
                if now >= nextPing {
                    try await timedSend(socket, text: "ping", deadline: tuning.deadline)
                    nextPing = now.advanced(by: tuning.pingInterval)
                }
                if ready, now >= nextProbe {
                    try await send(socket, ["type": .string("probe"), "version": .int(3)], deadline: tuning.deadline)
                    if probeDeadline == nil { probeDeadline = now.advanced(by: tuning.deadline) }
                    nextProbe = now.advanced(by: tuning.probeInterval)
                }
            case .message(let text):
                if text == "pong" { continue } // Not business progress.
                let frame = try Sync3Wire.decode(Data(text.utf8))
                switch frame["type"]?.stringValue {
                case "state":
                    head = max(head, try journal.acceptState(frame))
                    ready = true; handshake = nil; probeDeadline = nil
                case "ack":
                    guard let expected = push?.ids, case .array(let receipts) = frame["receipts"] else {
                        try Sync3Wire.fail("unexpected_ack")
                    }
                    guard expected == receipts.compactMap({ $0.objectValue?["id"]?.stringValue }) else {
                        try Sync3Wire.fail("receipt_conflict")
                    }
                    try journal.acknowledge(frame)
                    for receipt in receipts {
                        head = max(head, try Sync3Wire.integer(receipt.objectValue?["seq"]))
                    }
                    push = nil
                case "page":
                    guard frame["through"] == pull.map({ .int($0.through) }) else { try Sync3Wire.fail("unexpected_page") }
                    try journal.applyPage(frame); pull = nil
                case "error":
                    try Sync3Wire.shape(frame, ["type", "version", "code"])
                    guard frame["version"] == .int(3) else { try Sync3Wire.fail("upgrade_required") }
                    try Sync3Wire.fail(Sync3Wire.string(frame["code"]))
                default: try Sync3Wire.fail("invalid_frame")
                }
            }
            if ready {
                let cursor = try journal.cursor
                if cursor < head, pull == nil {
                    try await send(socket, ["type": .string("pull"), "version": .int(3),
                                            "epoch": .int(try journal.epoch), "after": .int(cursor), "through": .int(head)],
                                   deadline: tuning.deadline)
                    pull = (head, .now.advanced(by: tuning.deadline))
                    publish("catchingUp", nil)
                }
                if cursor == head, pull == nil {
                    publish("live", nil)
                    if push == nil {
                        let operations = try journal.pending()
                        if !operations.isEmpty {
                            let values = try operations.map { try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode($0)) }
                            try await send(socket, ["type": .string("push"), "version": .int(3), "operations": .array(values)],
                                           deadline: tuning.deadline)
                            push = (operations.map(\.id), .now.advanced(by: tuning.deadline))
                        }
                    }
                }
            }
        }
        try Task.checkCancellation()
        try Sync3Wire.fail("transport_unavailable")
    }
    private static func send(_ socket: URLSessionWebSocketTask, _ frame: [String: JSONValue], deadline: Duration) async throws {
        let data = try Sync3Wire.encode(frame)
        try await timedSend(socket, text: String(decoding: data, as: UTF8.self), deadline: deadline)
    }
    private static func timedSend(_ socket: URLSessionWebSocketTask, text: String, deadline: Duration) async throws {
        try await withTaskCancellationHandler {
            try await withThrowingTaskGroup(of: Void.self) { group in
                group.addTask { try await socket.send(.string(text)) }
                group.addTask {
                    try await Task.sleep(for: deadline)
                    socket.cancel(with: .goingAway, reason: nil)
                    throw Sync3Error.protocolError("business_timeout")
                }
                defer { group.cancelAll() }
                _ = try await group.next()
            }
        } onCancel: {
            // Cancelling the wrapper must also unblock Foundation's send.
            socket.cancel(with: .goingAway, reason: nil)
        }
    }
    private static func repairBounded(
        journal: Sync3Journal,
        exchange: @escaping @MainActor ([String: JSONValue]) async throws -> [String: JSONValue],
        deadline: Duration
    ) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { @MainActor in try await repairOnce(journal: journal, exchange: exchange) }
            group.addTask {
                try await Task.sleep(for: deadline)
                throw Sync3Error.protocolError("business_timeout")
            }
            defer { group.cancelAll() }
            _ = try await group.next()
        }
    }
    private static func repairOnce(
        journal: Sync3Journal,
        exchange: @MainActor ([String: JSONValue]) async throws -> [String: JSONValue]
    ) async throws {
        // Exchange implementations must bound each HTTP call and resolve a
        // fresh credential. This coordinator is the only retry owner.
        let state = try await exchange(journal.hello())
        try Task.checkCancellation()
        var head = try journal.acceptState(checkReply(state))
        for _ in 0..<7 {
            try Task.checkCancellation()
            let cursor = try journal.cursor
            if cursor < head {
                let page = try await exchange(["type": .string("pull"), "version": .int(3),
                                               "epoch": .int(journal.epoch), "after": .int(cursor), "through": .int(head)])
                try Task.checkCancellation()
                try journal.applyPage(checkReply(page))
            } else {
                let operations = try journal.pending()
                if operations.isEmpty { return }
                let values = try operations.map { try JSONDecoder().decode(JSONValue.self, from: Sync3Wire.encode($0)) }
                let ack = try await exchange(["type": .string("push"), "version": .int(3), "operations": .array(values)])
                try Task.checkCancellation()
                _ = try checkReply(ack)
                guard case .array(let receipts) = ack["receipts"],
                      receipts.compactMap({ $0.objectValue?["id"]?.stringValue }) == operations.map(\.id) else {
                    try Sync3Wire.fail("receipt_conflict")
                }
                try journal.acknowledge(ack)
                for receipt in receipts { head = max(head, try Sync3Wire.integer(receipt.objectValue?["seq"])) }
            }
        }
    }
    private static func checkReply(_ frame: [String: JSONValue]) throws -> [String: JSONValue] {
        if frame["type"] == .string("error") {
            try Sync3Wire.shape(frame, ["type", "version", "code"])
            guard frame["version"] == .int(3) else { try Sync3Wire.fail("upgrade_required") }
            try Sync3Wire.fail(Sync3Wire.string(frame["code"]))
        }
        return frame
    }
}

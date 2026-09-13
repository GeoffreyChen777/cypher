import Foundation

enum Workspace3Event {
    case metadata([(String, String)])
    case frame(UInt64, [String: JSONValue])
    case disconnected(UInt64)
}

@MainActor
final class Workspace3Client {
    struct Status: Equatable {
        var connected = false
        var caughtUp = false
        var generation: UInt64 = 0
        var error: String?
    }
    private enum Signal { case message(String), wake, tick, disconnected, failure(String) }
    private struct Outbound {
        let generation: UInt64
        let text: String
        let deadline: ContinuousClock.Instant
        let receipt: CheckedContinuation<Void, Error>
    }
    private final class Desired {
        var topics: [String] = []
        var presence: [String: JSONValue] = [:]
        var version: UInt64 = 0
        var probe = false
        var reconnect = false
        var control: [Outbound] = []
        var continuation: AsyncStream<Signal>.Continuation?
        func wake() {
            if case .dropped = continuation?.yield(.wake) { continuation?.finish() }
        }
        func failControls() {
            let pending = control; control.removeAll()
            for frame in pending { frame.receipt.resume(throwing: Sync3Error.protocolError("delivery_unknown")) }
        }
    }
    let journal: Workspace3Journal
    private(set) var status = Status()
    var onChange: (() -> Void)?
    private let desired = Desired()
    private var task: Task<Void, Never>?
    private var retired = false
    private let active: () -> Bool

    init(journal: Workspace3Journal, request: @escaping @MainActor () async throws -> URLRequest,
         active: @escaping @MainActor () -> Bool = { true },
         event: @escaping @MainActor (Workspace3Event) -> Void) {
        self.journal = journal
        self.active = active
        let desired = self.desired
        task = Task { [weak self, journal, desired] in
            var generation: UInt64 = 0, retry = 0.25
            let publish: (Bool, Bool, String?) -> Void = { [weak self] connected, caughtUp, error in
                self?.status = Status(connected: connected, caughtUp: caughtUp, generation: generation, error: error)
                self?.onChange?()
            }
            while !Task.isCancelled {
                guard active() else { break }
                generation += 1; publish(false, false, nil)
                do {
                    let auth = try await request()
                    try Task.checkCancellation()
                    guard active() else { throw CancellationError() }
                    try Self.checkRequest(auth, scope: journal.scope)
                    try await Self.session(journal: journal, desired: desired, request: auth,
                                           generation: generation, active: active, event: event, publish: publish)
                } catch {
                    desired.failControls()
                    if Task.isCancelled { break }
                    let code: String
                    if case Sync3Error.protocolError(let problem) = error { code = problem }
                    else { code = "transport_unavailable" }
                    publish(false, false, code); event(.disconnected(generation))
                    guard ["transport_unavailable", "business_timeout", "reauth_required"].contains(code) else { return }
                }
                try? await Task.sleep(for: .milliseconds(Int((retry + Double.random(in: 0...0.25)) * 1000)))
                retry = min(retry * 2, 30)
            }
            desired.failControls(); publish(false, false, nil); event(.disconnected(generation))
        }
    }
    deinit { task?.cancel() }
    func stop() async {
        retire()
        await task?.value; task = nil
    }
    func retire() { retired = true; task?.cancel(); desired.continuation?.finish() }
    func nudge() { if !retired { desired.wake() } }
    func probe() { if !retired { desired.probe = true; desired.wake() } }
    func reconnect() { if !retired { desired.reconnect = true; desired.wake() } }
    func watch(_ chats: [String]) throws {
        guard !retired, chats.count <= 8, chats.allSatisfy(Workspace3Wire.id) else { try Workspace3Wire.fail("invalid_topics") }
        desired.topics = Array(Set(chats)).sorted(); desired.version += 1; desired.wake()
    }
    func presence(_ state: [String: JSONValue]) throws {
        try Workspace3Wire.value(.object(state))
        guard !retired, try Workspace3Wire.data(state).count <= 64 * 1024 else { try Workspace3Wire.fail("invalid_presence") }
        desired.presence = state; desired.version += 1; desired.wake()
    }
    func send(generation: UInt64, frame: [String: JSONValue]) async throws {
        guard !retired, active(), status.connected, status.generation == generation else { try Workspace3Wire.fail("connection_retired") }
        guard let type = frame["type"]?.stringValue, ["call", "input", "inputAck", "reply", "ack", "cancel"].contains(type),
              desired.control.count < 32 else { try Workspace3Wire.fail("control_backpressure") }
        var frame = frame; frame["version"] = .int(3)
        try Workspace3Wire.value(.object(frame))
        let data = try Workspace3Wire.data(frame)
        guard data.count <= Workspace3Wire.frameBytes else { try Workspace3Wire.fail("frame_too_large") }
        try await withCheckedThrowingContinuation { receipt in
            desired.control.append(Outbound(generation: generation, text: String(decoding: data, as: UTF8.self),
                                             deadline: .now.advanced(by: .seconds(30)), receipt: receipt))
            desired.wake()
        }
    }
    private static func checkRequest(_ request: URLRequest, scope: Workspace3Scope) throws {
        guard let endpoint = URL(string: scope.endpoint),
              var expected = URLComponents(url: endpoint.appending(path: "workspace3/\(scope.org)/ws"), resolvingAgainstBaseURL: false),
              let actual = request.url.flatMap({ URLComponents(url: $0, resolvingAgainstBaseURL: false) }) else {
            try Workspace3Wire.fail("invalid_endpoint")
        }
        expected.scheme = expected.scheme == "http" ? "ws" : "wss"
        guard actual == expected, actual.query == nil, actual.user == nil, actual.password == nil,
              actual.scheme == "wss" || (actual.scheme == "ws" && ["localhost", "127.0.0.1", "::1"].contains(actual.host)),
              request.value(forHTTPHeaderField: "Authorization")?.hasPrefix("Bearer ") == true else {
            try Workspace3Wire.fail("workspace_endpoint_mismatch")
        }
    }
    private static func session(journal: Workspace3Journal, desired: Desired, request: URLRequest,
                                generation: UInt64, active: @escaping () -> Bool, event: (Workspace3Event) -> Void,
                                publish: (Bool, Bool, String?) -> Void) async throws {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 30
        let session = URLSession(configuration: config, delegate: V3NoRedirect(), delegateQueue: nil), socket = session.webSocketTask(with: request)
        socket.maximumMessageSize = Workspace3Wire.frameBytes
        let (signals, continuation) = AsyncStream<Signal>.makeStream(bufferingPolicy: .bufferingOldest(32))
        desired.continuation = continuation
        socket.resume()
        let reader = Task {
            do {
                while !Task.isCancelled {
                    guard case .string(let text) = try await socket.receive() else {
                        _ = continuation.yield(.failure("expected_json")); continuation.finish(); return
                    }
                    if case .dropped = continuation.yield(.message(text)) { break }
                }
            } catch {}
            _ = continuation.yield(.disconnected); continuation.finish()
        }
        let ticker = Task {
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(1)) } catch { break }
                if case .dropped = continuation.yield(.tick) { continuation.finish(); break }
            }
        }
        func close() {
            desired.continuation = nil; continuation.finish(); reader.cancel(); ticker.cancel()
            socket.cancel(with: .goingAway, reason: nil); session.invalidateAndCancel()
        }
        do {
            try await drive(journal: journal, desired: desired, socket: socket, signals: signals,
                            generation: generation, active: active, event: event, publish: publish)
            close(); await reader.value; await ticker.value
        } catch {
            close(); await reader.value; await ticker.value; throw error
        }
    }
    private static func drive(journal: Workspace3Journal, desired: Desired, socket: URLSessionWebSocketTask,
                              signals: AsyncStream<Signal>, generation: UInt64, active: () -> Bool, event: (Workspace3Event) -> Void,
                              publish: (Bool, Bool, String?) -> Void) async throws {
        let scope = journal.scope
        let helloAfter = try journal.cursor
        try await send(socket, ["type": .string("hello"), "user": .string(scope.user), "org": .string(scope.org),
                                "actor": .string(scope.actor), "role": .string("viewer"), "after": .int(Int64(helloAfter))])
        var handshake: ContinuousClock.Instant? = .now.advanced(by: .seconds(30))
        var pull: (UInt64, ContinuousClock.Instant)?, push: (Workspace3Pending, ContinuousClock.Instant)?
        var probe: ContinuousClock.Instant?, head: UInt64 = 0, ready = false
        var applied: UInt64?, business = ContinuousClock.now
        var nextPresence = ContinuousClock.now, nextPing = ContinuousClock.now.advanced(by: .seconds(20))
        func page(_ frame: [String: JSONValue], after: UInt64) throws {
            let p = Workspace3Page(through: UInt64(try Sync3Wire.integer(frame["through"])),
                                   next: UInt64(try Sync3Wire.integer(frame["next"])),
                                   done: frame["done"]?.boolValue == true, rows: try Workspace3Wire.rows(frame["rows"]))
            try journal.applyPage(after: after, page: p)
            event(.metadata(p.rows.map { ($0.kind, $0.id) }))
            head = max(head, p.through)
        }
        for await signal in signals {
            try Task.checkCancellation()
            guard active() else { throw CancellationError() }
            if desired.reconnect { desired.reconnect = false; try Workspace3Wire.fail("transport_unavailable") }
            switch signal {
            case .disconnected: try Workspace3Wire.fail("transport_unavailable")
            case .failure(let code): try Workspace3Wire.fail(code)
            case .wake: break
            case .tick:
                let now = ContinuousClock.now
                if [handshake, pull?.1, push?.1, probe].compactMap({ $0 }).contains(where: { now >= $0 }) {
                    try Workspace3Wire.fail("business_timeout")
                }
                if now >= nextPing {
                    try await sendText(socket, "ping"); nextPing = now.advanced(by: .seconds(20))
                }
            case .message(let text):
                if text == "pong" { continue }
                let frame = try Workspace3Wire.decode(text)
                let type = frame["type"]?.stringValue
                if type != "welcome" && type != "error" && !ready { try Workspace3Wire.fail("hello_required") }
                switch type {
                case "welcome":
                    guard !ready, frame["user"] == .string(scope.user), frame["org"] == .string(scope.org) else {
                        try Workspace3Wire.fail("account_mismatch")
                    }
                    try page(frame, after: helloAfter); ready = true; handshake = nil
                case "page":
                    guard let expected = pull?.0 else { try Workspace3Wire.fail("unexpected_page") }
                    try page(frame, after: expected); pull = nil
                case "pushed":
                    guard let expected = push?.0, frame["id"] == .string(expected.id), frame["requestHash"] == .string(expected.hash) else {
                        try Workspace3Wire.fail("workspace_ack_mismatch")
                    }
                    let rows = try Workspace3Wire.rows(frame["rows"]), through = UInt64(try Sync3Wire.integer(frame["through"]))
                    try journal.acknowledge(id: expected.id, hash: expected.hash, through: through, rows: rows)
                    head = max(head, through); push = nil; event(.metadata(rows.map { ($0.kind, $0.id) }))
                case "changed": head = max(head, UInt64(try Sync3Wire.integer(frame["through"])))
                case "probeOk":
                    guard probe != nil, frame["id"] == .string("liveness") else { try Workspace3Wire.fail("unexpected_probe") }
                    head = max(head, UInt64(try Sync3Wire.integer(frame["through"]))); probe = nil
                case "error":
                    if push.map({ frame["id"] == .string($0.0.id) }) == true || (frame["id"] == nil && frame["token"] == nil) {
                        try Workspace3Wire.fail(frame["code"]?.stringValue ?? "protocol_error")
                    }
                    event(.frame(generation, frame))
                default: event(.frame(generation, frame))
                }
                business = .now
            }
            if ready {
                let now = ContinuousClock.now
                if applied != desired.version {
                    try await send(socket, ["type": .string("watch"), "chats": .array(desired.topics.map(JSONValue.string))])
                    try await send(socket, ["type": .string("presence"), "state": .object(desired.presence)])
                    applied = desired.version; nextPresence = now.advanced(by: .seconds(15))
                } else if now >= nextPresence {
                    try await send(socket, ["type": .string("presence"), "state": .object(desired.presence)])
                    nextPresence = now.advanced(by: .seconds(15))
                }
                if probe == nil && (desired.probe || business.duration(to: now) >= .seconds(900)) {
                    desired.probe = false; probe = now.advanced(by: .seconds(30))
                    try await send(socket, ["type": .string("probe"), "id": .string("liveness")])
                }
                let cursor = try journal.cursor
                if pull == nil && cursor < head {
                    pull = (cursor, now.advanced(by: .seconds(30)))
                    try await send(socket, ["type": .string("page"), "after": .int(Int64(cursor))])
                }
                let caughtUp = pull == nil && cursor >= head
                if caughtUp && push == nil, let pending = try journal.pending() {
                    push = (pending, now.advanced(by: .seconds(30)))
                    try await sendText(socket, pending.request)
                }
                publish(true, caughtUp, nil)
                while !desired.control.isEmpty {
                    guard active() else { throw CancellationError() }
                    let frame = desired.control.removeFirst()
                    guard frame.generation == generation else {
                        frame.receipt.resume(throwing: Sync3Error.protocolError("connection_retired")); continue
                    }
                    guard ContinuousClock.now < frame.deadline else {
                        frame.receipt.resume(throwing: Sync3Error.protocolError("control_backpressure")); continue
                    }
                    do { try await sendText(socket, frame.text); frame.receipt.resume() }
                    catch { frame.receipt.resume(throwing: error); throw error }
                }
            }
        }
        try Task.checkCancellation()
        try Workspace3Wire.fail("transport_unavailable")
    }
    private static func send(_ socket: URLSessionWebSocketTask, _ value: [String: JSONValue]) async throws {
        var value = value; value["version"] = .int(3)
        try await sendText(socket, String(decoding: Workspace3Wire.data(value), as: UTF8.self))
    }
    private static func sendText(_ socket: URLSessionWebSocketTask, _ text: String) async throws {
        try await withTaskCancellationHandler {
            try await withThrowingTaskGroup(of: Void.self) { group in
                group.addTask { try await socket.send(.string(text)) }
                group.addTask {
                    try await Task.sleep(for: .seconds(30)); socket.cancel(with: .goingAway, reason: nil)
                    try Workspace3Wire.fail("business_timeout")
                }
                defer { group.cancelAll() }; _ = try await group.next()
            }
        } onCancel: { socket.cancel(with: .goingAway, reason: nil) }
    }
}

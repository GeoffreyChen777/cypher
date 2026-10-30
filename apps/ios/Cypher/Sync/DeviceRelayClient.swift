// Device-room relay RPC client — dials a device's room on the edge as a
// `client` peer and speaks ControlRpc to the HOST engine over a virtual
// socket (crates/rpc/src/device_room.rs + apps/edge/src/device/device-room.ts).
//
// Streaming calls (`subscribe`) get `{id, item}` frames until `{id, done}`
// or `{id, err}`; dropping the stream sends `{id, cancel: true}`.
//
// Frame codec (binary WS messages): uleb128(headerLen) ‖ headerJSON ‖ payload.
// Header key order MUST be {"s","k","to","from"} (byte parity with both
// implementations; shared vectors in protocol/vectors/device-frames-v1.json,
// protocol/README.md); clients never set `to`/`from` — the DO stamps `from`.
// RPC payloads are ndjson ControlRpc frames: {id, method, params} out,
// {id, ok|err|item|done} back. Relay control frames (kind " relay" — leading
// space is part of the constant) signal host_offline/host_closed.
//
// Device trust (docs/design/device-trust.md): when the host is in this
// phone's trust group, the link first runs a Noise IK handshake (kind
// "noise") and then every RPC frame travels sealed (kind "rpcx"), mirroring
// crates/rpc/src/device_room/links.rs. Sealed frames must leave in the order
// they were sealed, so every frame goes through one outbox.

import CryptoKit
import Foundation

enum RelayError: LocalizedError {
    case notConnected
    case hostOffline
    case rpc(String)
    case timeout
    /// Device trust refused the link (this phone or the host isn't paired).
    case notPaired(String)

    var errorDescription: String? {
        switch self {
        case .notConnected: return "Not connected to the device"
        case .hostOffline: return "The device is offline"
        case .rpc(let message): return message
        case .timeout: return "The device didn't respond"
        case .notPaired(let message): return message
        }
    }
}

/// A reply whose content doesn't matter — only that the host accepted the
/// call (any `ok` payload, including null).
struct IgnoredReply: Decodable {
    init(from decoder: Decoder) throws {}
}

actor DeviceRelayClient {
    static let rpcKind = "rpc"
    static let relayKind = " relay"  // leading space is intentional
    static let noiseKind = "noise"
    static let secureKind = "rpcx"
    static let rpcHeader = #"{"s":"rpc","k":"rpc"}"#
    static let secureHeader = #"{"s":"rpc","k":"rpcx"}"#
    static let noiseHeader = #"{"s":"noise","k":"noise"}"#
    /// Handshake payload types (crates/rpc/src/device_room/trust.rs).
    static let hello: UInt8 = 1
    static let reply: UInt8 = 2
    static let refused: UInt8 = 3

    private let deviceId: String
    private let config: AppConfig

    private var socket: URLSessionWebSocketTask?
    private var receiveTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?
    private var nextId: UInt64 = 1
    private var pending: [UInt64: CheckedContinuation<Result<Data, RelayError>, Never>] = [:]
    /// Open streaming calls. An id belongs to a unary waiter or a stream,
    /// never both.
    private var streams: [UInt64: AsyncThrowingStream<Data, Error>.Continuation] = [:]
    private var connected = false
    /// One connect at a time: a second caller waits for the first instead of
    /// opening a parallel socket mid-handshake.
    private var connecting: Task<Void, Error>?
    /// The Noise session when the host is a paired device.
    private var session: NoiseSession?
    /// Every outgoing frame, in sealing order; one task sends them.
    private var outbox: AsyncStream<Data>.Continuation?
    private var senderTask: Task<Void, Never>?

    init(deviceId: String, config: AppConfig) {
        self.deviceId = deviceId
        self.config = config
    }

    // MARK: Lifecycle

    private func connect() async throws {
        if connected, socket != nil {
            // A plaintext link opened before the host was paired is replaced
            // by a sealed one as soon as the host's key is known.
            guard session == nil, let trust = config.trust else { return }
            let deviceId = self.deviceId
            guard await MainActor.run(body: { trust.hostKey(for: deviceId) != nil }) else { return }
            teardown(error: .notConnected)
        }
        if let connecting { return try await connecting.value }
        let attempt = Task { try await self.open() }
        connecting = attempt
        defer { connecting = nil }
        try await attempt.value
    }

    private func open() async throws {
        guard let token = await config.currentToken() else { throw RelayError.notConnected }
        // The host's key, when this phone and the host are both in the group.
        var secure: (local: Curve25519.KeyAgreement.PrivateKey, host: Curve25519.KeyAgreement.PublicKey)?
        var requirePaired = false
        if let trust = config.trust {
            let deviceId = self.deviceId
            let found = await MainActor.run {
                (trust.hostKey(for: deviceId), trust.localKey, trust.requirePaired)
            }
            if let host = found.0 { secure = (found.1, host) }
            requirePaired = found.2
        }
        if secure == nil && requirePaired {
            throw RelayError.notPaired(
                "This device isn't paired with your iPhone, and your devices only accept paired devices. Pair the iPhone under Account › Paired devices."
            )
        }
        var components = URLComponents(
            url: config.edgeURL.appending(path: "device/\(deviceId)/ws"),
            resolvingAgainstBaseURL: false)!
        components.scheme = components.scheme == "http" ? "ws" : "wss"
        components.queryItems = [
            URLQueryItem(name: "role", value: "client"),
            // A reconnect is a new relay peer. Reusing a connId can briefly
            // leave two tagged sockets in the hibernating DO and route the
            // host's response to the stale predecessor.
            URLQueryItem(name: "connId", value: UUID().uuidString.lowercased()),
            URLQueryItem(name: "token", value: token),
        ]
        let task = URLSession.shared.webSocketTask(with: URLRequest(url: components.url!).liftingBearer())
        // Checkout patches are capped at 3 MiB before JSON escaping. Keep a
        // finite transport ceiling, but don't reject normal diffs at 1 MiB.
        task.maximumMessageSize = 8 * 1024 * 1024
        task.resume()
        if let secure {
            do {
                session = try await handshake(task, local: secure.local, host: secure.host)
            } catch {
                task.cancel(with: .goingAway, reason: nil)
                throw error
            }
        }
        socket = task
        connected = true

        let (frames, outbox) = AsyncStream<Data>.makeStream()
        self.outbox = outbox
        senderTask = Task { [weak self] in
            for await frame in frames {
                do {
                    try await task.send(.data(frame))
                } catch {
                    await self?.teardown(error: .notConnected)
                    return
                }
            }
        }
        receiveTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                guard let sock = await self.socket else { return }
                do {
                    let message = try await sock.receive()
                    await self.handleInbound(message)
                } catch {
                    await self.teardown(error: .hostOffline)
                    return
                }
            }
        }
        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                guard let self else { return }
                await self.sendPing()
            }
        }
    }

    /// The client half of the Noise IK handshake: hello, then the host's
    /// reply or refusal. Ten seconds, like the Rust client.
    private func handshake(
        _ task: URLSessionWebSocketTask,
        local: Curve25519.KeyAgreement.PrivateKey,
        host: Curve25519.KeyAgreement.PublicKey
    ) async throws -> NoiseSession {
        var initiator = NoiseInitiator(local: local, hostDeviceId: deviceId, hostKey: host)
        let first = try initiator.first(localDeviceId: config.deviceId)
        try await task.send(.data(Self.encodeFrame(header: Self.noiseHeader, payload: Data([Self.hello]) + first)))
        let timer = Task {
            try await Task.sleep(nanoseconds: 10_000_000_000)
            task.cancel(with: .goingAway, reason: nil)
        }
        defer { timer.cancel() }
        while true {
            let message: URLSessionWebSocketTask.Message
            do {
                message = try await task.receive()
            } catch {
                // The socket failed or the timer closed it: retried like an
                // offline host.
                throw RelayError.hostOffline
            }
            guard case .data(let data) = message, let (header, payload) = Self.decodeFrame(data) else { continue }
            if header.k == Self.relayKind { throw RelayError.hostOffline }
            guard header.k == Self.noiseKind, let kind = payload.first else { continue }
            let body = Data(payload.dropFirst())
            switch kind {
            case Self.reply:
                do {
                    return try initiator.finish(reply: body)
                } catch {
                    throw RelayError.notPaired(
                        "The device's identity didn't match its pairing. Check Account › Paired devices.")
                }
            case Self.refused:
                throw RelayError.notPaired(Self.refusalMessage(String(decoding: body, as: UTF8.self)))
            default:
                throw RelayError.rpc("Malformed handshake reply")
            }
        }
    }

    /// Mirrors crates/rpc/src/device_room/trust.rs `refusal_message`.
    static func refusalMessage(_ reason: String) -> String {
        switch reason {
        case "untrusted_device":
            return
                "This device only accepts paired devices, and your iPhone isn't paired with it. Pair it under Account › Paired devices."
        default:
            return "The device refused the secure connection (\(reason))."
        }
    }

    /// Queue one ControlRpc payload, sealed when the link is secure.
    private func enqueue(_ payload: Data) throws {
        guard let outbox else { throw RelayError.notConnected }
        if var current = session {
            let fragments: [Data]
            do {
                fragments = try current.seal(payload)
            } catch {
                throw RelayError.rpc("Message too large for the secure channel")
            }
            session = current
            for fragment in fragments {
                outbox.yield(Self.encodeFrame(header: Self.secureHeader, payload: fragment))
            }
        } else {
            outbox.yield(Self.encodeFrame(header: Self.rpcHeader, payload: payload))
        }
    }

    func close() {
        teardown(error: .notConnected)
    }

    private func teardown(error: RelayError) {
        receiveTask?.cancel()
        pingTask?.cancel()
        outbox?.finish()
        outbox = nil
        senderTask?.cancel()
        senderTask = nil
        session = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        connected = false
        let waiting = pending
        pending.removeAll()
        for (_, continuation) in waiting {
            continuation.resume(returning: .failure(error))
        }
        let open = streams
        streams.removeAll()
        for (_, continuation) in open {
            continuation.finish(throwing: error)
        }
    }

    private func sendPing() async {
        try? await socket?.send(.string("ping"))
    }

    // MARK: RPC

    /// One unary ControlRpc call to the host engine. The default 10s deadline
    /// suits interactive calls (the engine itself caps folder listing at 6s);
    /// attachment uploads pass longer ones (first chunk 90s for a cold dial,
    /// commit 150s to outlast the cross-device assemble — desktop state.ts).
    ///
    /// `params` enters the actor as JSON: `[String: Any]` isn't Sendable.
    nonisolated(nonsending) func call<Response: Decodable & Sendable>(
        method: String, params: [String: Any], timeoutSeconds: UInt64 = 10
    ) async throws -> Response {
        let json = try JSONSerialization.data(withJSONObject: params)
        return try await call(method: method, paramsJSON: json, timeoutSeconds: timeoutSeconds)
    }

    private func call<Response: Decodable & Sendable>(
        method: String, paramsJSON: Data,
        timeoutSeconds: UInt64
    ) async throws -> Response {
        for attempt in 0..<3 {
            do {
                return try await callOnce(
                    method: method, paramsJSON: paramsJSON,
                    timeoutSeconds: timeoutSeconds)
            } catch let error as RelayError {
                guard attempt < 2 else { throw error }
                switch error {
                case .hostOffline, .notConnected:
                    teardown(error: error)
                    try? await Task.sleep(nanoseconds: UInt64(attempt + 1) * 250_000_000)
                case .rpc, .timeout, .notPaired:
                    throw error
                }
            }
        }
        throw RelayError.notConnected
    }

    private func callOnce<Response: Decodable & Sendable>(
        method: String,
        paramsJSON: Data,
        timeoutSeconds: UInt64
    ) async throws -> Response {
        try await connect()
        let id = nextId
        nextId += 1
        // Always send a params object — the engine's serde rejects a missing
        // field even when every param is optional (ListFolders home listing).
        let params = try JSONSerialization.jsonObject(with: paramsJSON)
        let frame: [String: Any] = ["id": id, "method": method, "params": params]
        let payload = try JSONSerialization.data(withJSONObject: frame)

        // Install the waiter before sending. URLSession's async send may yield
        // long enough for a fast host reply to reach handleInbound; registering
        // afterward loses that reply and turns a successful call into a timeout.
        let result: Result<Data, RelayError> = await withCheckedContinuation { continuation in
            pending[id] = continuation
            do {
                try enqueue(payload)
            } catch {
                failCall(id: id, error: (error as? RelayError) ?? .notConnected)
            }
            Task {
                try? await Task.sleep(nanoseconds: timeoutSeconds * 1_000_000_000)
                self.timeoutCall(id: id)
            }
        }
        switch result {
        case .failure(let error): throw error
        case .success(let ok):
            return try JSONDecoder().decode(Response.self, from: ok)
        }
    }

    /// A streaming ControlRpc call (`WatchDocMessages`, `WatchSideChatStatus`
    /// …): each `item` payload as JSON data, in order. The stream finishes on
    /// `done`, throws on `err` or a dropped link (no automatic resubscribe —
    /// the caller decides), and cancelling it tells the host to stop.
    nonisolated func subscribe(method: String, params: [String: Any]) -> AsyncThrowingStream<Data, Error> {
        let (stream, continuation) = AsyncThrowingStream<Data, Error>.makeStream()
        // As JSON into the actor: `[String: Any]` isn't Sendable.
        let paramsJSON = try? JSONSerialization.data(withJSONObject: params)
        Task { await self.openStream(method: method, paramsJSON: paramsJSON, continuation: continuation) }
        return stream
    }

    private func openStream(
        method: String, paramsJSON: Data?,
        continuation: AsyncThrowingStream<Data, Error>.Continuation
    ) async {
        do {
            try await connect()
        } catch {
            continuation.finish(throwing: error)
            return
        }
        let id = nextId
        nextId += 1
        streams[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { await self?.closeStream(id: id) }
        }
        guard let paramsJSON, let params = try? JSONSerialization.jsonObject(with: paramsJSON),
            let payload = try? JSONSerialization.data(withJSONObject: ["id": id, "method": method, "params": params])
        else {
            finishStream(id: id, error: .notConnected)
            return
        }
        do {
            try enqueue(payload)
        } catch {
            finishStream(id: id, error: (error as? RelayError) ?? .notConnected)
        }
    }

    private func finishStream(id: UInt64, error: RelayError?) {
        guard let continuation = streams.removeValue(forKey: id) else { return }
        if let error { continuation.finish(throwing: error) } else { continuation.finish() }
    }

    /// The consumer let go: stop the host's side of a still-open stream.
    private func closeStream(id: UInt64) async {
        guard streams.removeValue(forKey: id) != nil,
            let payload = try? JSONSerialization.data(withJSONObject: ["id": id, "cancel": true])
        else { return }
        try? enqueue(payload)
    }

    private func failCall(id: UInt64, error: RelayError) {
        if let continuation = pending.removeValue(forKey: id) {
            continuation.resume(returning: .failure(error))
        }
    }

    private func timeoutCall(id: UInt64) {
        failCall(id: id, error: .timeout)
    }

    // MARK: Inbound

    private func handleInbound(_ message: URLSessionWebSocketTask.Message) {
        switch message {
        case .string:
            return  // "pong"
        case .data(let data):
            guard let (header, payload) = Self.decodeFrame(data) else { return }
            switch header.k {
            case Self.rpcKind:
                // A secure link never accepts plaintext from the relay.
                guard session == nil else { return }
                handleRpcPayload(payload)
            case Self.secureKind:
                guard var current = session else { return }
                do {
                    let message = try current.open(payload)
                    session = current
                    if let message { handleRpcPayload(message) }
                } catch {
                    teardown(error: .rpc("The secure connection to the device failed"))
                }
            case Self.noiseKind:
                // The host ended the session (this phone left the group).
                if payload.first == Self.refused {
                    teardown(
                        error: .notPaired(Self.refusalMessage(String(decoding: payload.dropFirst(), as: UTF8.self))))
                }
            case Self.relayKind:
                // {"error":"host_offline"|"host_closed"|...} — link down.
                teardown(error: .hostOffline)
            default:
                return
            }
        @unknown default:
            return
        }
    }

    private func handleRpcPayload(_ payload: Data) {
        // ndjson: each line is one ServerFrame.
        guard let text = String(data: payload, encoding: .utf8) else { return }
        for line in text.split(separator: "\n") {
            guard let obj = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
                let id = (obj["id"] as? NSNumber)?.uint64Value
            else { continue }
            if let stream = streams[id] {
                if let err = obj["err"] as? String {
                    finishStream(id: id, error: .rpc(err))
                } else if obj.keys.contains("item"),
                    let item = try? JSONSerialization.data(
                        withJSONObject: obj["item"] ?? NSNull(),
                        options: .fragmentsAllowed)
                {
                    stream.yield(item)
                } else if obj["done"] != nil {
                    finishStream(id: id, error: nil)
                }
                continue
            }
            guard let continuation = pending.removeValue(forKey: id) else { continue }
            if let err = obj["err"] as? String {
                continuation.resume(returning: .failure(.rpc(err)))
            } else if obj.keys.contains("ok"),
                let okData = try? JSONSerialization.data(
                    withJSONObject: obj["ok"] ?? NSNull(),
                    options: .fragmentsAllowed)
            {
                continuation.resume(returning: .success(okData))
            } else {
                continuation.resume(returning: .failure(.rpc("unexpected reply")))
            }
        }
    }

    // MARK: Frame codec

    struct FrameHeader: Decodable {
        var s: String?
        var k: String?
        var to: String?
        var from: String?
    }

    static func encodeFrame(header: String, payload: Data) -> Data {
        let headerBytes = Data(header.utf8)
        var out = Data()
        var len = UInt64(headerBytes.count)
        repeat {
            var byte = UInt8(len & 0x7f)
            len >>= 7
            if len != 0 { byte |= 0x80 }
            out.append(byte)
        } while len != 0
        out.append(headerBytes)
        out.append(payload)
        return out
    }

    static func decodeFrame(_ data: Data) -> (FrameHeader, Data)? {
        var offset = 0
        var length: UInt64 = 0
        var shift: UInt64 = 0
        let bytes = [UInt8](data)
        while offset < bytes.count {
            let byte = bytes[offset]
            offset += 1
            length |= UInt64(byte & 0x7f) << shift
            if byte & 0x80 == 0 { break }
            shift += 7
            if shift > 28 { return nil }
        }
        guard offset + Int(length) <= bytes.count else { return nil }
        let headerData = Data(bytes[offset..<offset + Int(length)])
        guard let header = try? JSONDecoder().decode(FrameHeader.self, from: headerData) else { return nil }
        let payload = Data(bytes[(offset + Int(length))...])
        return (header, payload)
    }
}

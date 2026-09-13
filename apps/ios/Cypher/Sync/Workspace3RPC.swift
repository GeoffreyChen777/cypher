import Foundation

/// Transient, bounded unary RPC. A reconnect can never resubmit a call.
@MainActor
final class Workspace3RPC {
    private enum Signal { case frame([String: JSONValue]), cancel, timeout }
    private final class Pending {
        let owner: String
        let generation: UInt64
        let receive: AsyncStream<Signal>
        let input: AsyncStream<Signal>.Continuation
        let result: CheckedContinuation<Data, Error>
        var task: Task<Void, Never>?
        var timer: Task<Void, Never>?
        var token: String?
        var cancelled = false
        var progressDeadline: ContinuousClock.Instant? = .now.advanced(by: .seconds(30))
        init(owner: String, generation: UInt64, result: CheckedContinuation<Data, Error>) {
            self.owner = owner; self.generation = generation; self.result = result
            (receive, input) = AsyncStream.makeStream(bufferingPolicy: .bufferingOldest(8))
        }
    }
    private weak var client: Workspace3Client?
    private let active: () -> Bool
    private var calls: [String: Pending] = [:]
    private var retired = false
    private var admitted = 0

    init(client: Workspace3Client, active: @escaping () -> Bool) { self.client = client; self.active = active }
    var count: Int { calls.count }
    func event(_ event: Workspace3Event) {
        switch event {
        case .disconnected:
            for id in Array(calls.keys) { finish(id, .failure(RelayError.rpc("delivery_unknown"))) }
        case .frame(let generation, let frame):
            let id = frame["id"]?.stringValue ?? calls.first(where: { $0.value.token == frame["token"]?.stringValue && $0.value.token != nil })?.key
            guard let id, let call = calls[id], call.generation == generation else { return }
            if frame["type"] == .string("routed") { call.token = frame["token"]?.stringValue }
            if case .dropped = call.input.yield(.frame(frame)) {
                client?.reconnect()
                finish(id, .failure(RelayError.rpc("rpc_backpressure")))
            }
        default: break
        }
    }
    func retire() {
        retired = true
        for id in Array(calls.keys) { finish(id, .failure(RelayError.rpc("delivery_unknown"))) }
    }
    func cancel(owner: String) {
        for call in calls.values where call.owner == owner { call.cancelled = true; _ = call.input.yield(.cancel) }
    }
    func call(owner: String, target: String, method: String, params: JSONValue, timeout: UInt64) async throws -> Data {
        guard !retired, active(), admitted < 8 else { try Workspace3Wire.fail("rpc_capacity") }
        admitted += 1
        defer { admitted -= 1 }
        guard Workspace3Wire.id(target), let first = method.utf8.first, Workspace3Wire.asciiAlpha(first),
              method.utf8.count <= 96, method.utf8.allSatisfy({ Workspace3Wire.asciiAlpha($0) || (48...57).contains($0) }),
              (1...600).contains(timeout) else { try Workspace3Wire.fail("invalid_call") }
        let deadline = ContinuousClock.now.advanced(by: .seconds(timeout))
        guard let client else { throw RelayError.notConnected }
        while !client.status.connected {
            try Task.checkCancellation()
            guard !retired, active(), ContinuousClock.now < deadline else { throw RelayError.notConnected }
            if let error = client.status.error, !["transport_unavailable", "business_timeout", "reauth_required"].contains(error) {
                throw RelayError.rpc(error)
            }
            try await Task.sleep(for: .milliseconds(25))
        }
        guard !retired, active(), calls.count < 8 else { try Workspace3Wire.fail("rpc_capacity") }
        let encoder = try Workspace3RPCCodec.Encoder(params)
        let id = UUID().uuidString.lowercased(), generation = client.status.generation
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            return try await withCheckedThrowingContinuation { result in
                let pending = Pending(owner: owner, generation: generation, result: result)
                calls[id] = pending
                pending.timer = Task {
                    while !Task.isCancelled {
                        do { try await Task.sleep(for: .seconds(1)) } catch { return }
                        if ContinuousClock.now >= deadline || pending.progressDeadline.map({ ContinuousClock.now >= $0 }) == true {
                            _ = pending.input.yield(.timeout); return
                        }
                    }
                }
                pending.task = Task { [weak self] in
                    do {
                        let value = try await Self.run(id: id, target: target, method: method, encoder: encoder,
                                                       pending: pending, client: client, active: self?.active ?? { false })
                        self?.finish(id, .success(value))
                    } catch {
                        if let token = pending.token {
                            try? await client.send(generation: generation, frame: ["type": .string("cancel"), "token": .string(token)])
                        } else if !Task.isCancelled { client.reconnect() }
                        self?.finish(id, .failure(error))
                    }
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                guard let pending = self?.calls[id] else { return }
                pending.cancelled = true; _ = pending.input.yield(.cancel)
            }
        }
    }
    private func finish(_ id: String, _ result: Result<Data, Error>) {
        guard let pending = calls.removeValue(forKey: id) else { return }
        pending.timer?.cancel(); pending.task?.cancel(); pending.input.finish()
        pending.result.resume(with: result)
    }
    private static func run(id: String, target: String, method: String, encoder: Workspace3RPCCodec.Encoder,
                            pending: Pending, client: Workspace3Client, active: () -> Bool) async throws -> Data {
        func send(_ frame: [String: JSONValue]) async throws {
            try Task.checkCancellation()
            guard active() else { throw RelayError.notConnected }
            try await client.send(generation: pending.generation, frame: frame)
        }
        try await send(["type": .string("call"), "id": .string(id), "target": .string(target),
                        "method": .string(method), "params": .object([:]), "input": .bool(true)])
        var encoder = encoder, decoder = Workspace3RPCCodec.Decoder()
        var token: String?, sent: Int64 = 0, through: Int64 = 0, next: Int64 = 0
        var iterator = pending.receive.makeAsyncIterator()
        while true {
            if pending.cancelled && token != nil { throw RelayError.rpc("delivery_unknown") }
            if let token, sent - through < 2, let part = encoder.next() {
                try await send(["type": .string("input"), "token": .string(token), "sequence": .int(sent),
                                "done": .bool(encoder.complete), "value": part])
                sent += 1
                if sent - through >= 2 { pending.progressDeadline = .now.advanced(by: .seconds(30)) }
                continue
            }
            guard let signal = await iterator.next() else { throw RelayError.rpc("delivery_unknown") }
            try Task.checkCancellation()
            switch signal {
            case .timeout: throw RelayError.rpc("delivery_unknown")
            case .cancel:
                pending.cancelled = true
                // Before routed, keep waiting for the generation-specific
                // cancellation capability rather than abandon a live route.
            case .frame(let frame):
                switch frame["type"]?.stringValue {
                case "routed":
                    guard token == nil, let value = frame["token"]?.stringValue else { try Workspace3Wire.fail("invalid_route") }
                    token = value
                    pending.progressDeadline = nil
                case "inputCredit":
                    let ack = try Sync3Wire.integer(frame["through"])
                    guard frame["token"]?.stringValue == token, ack >= through, ack <= sent else { try Workspace3Wire.fail("invalid_credit") }
                    if ack > through { pending.progressDeadline = nil }
                    through = ack
                case "reply":
                    guard let token, try Sync3Wire.integer(frame["sequence"]) == next, let done = frame["done"]?.boolValue,
                          let part = frame["value"] else { try Workspace3Wire.fail("invalid_reply") }
                    let value = try decoder.push(part)
                    pending.progressDeadline = value == nil ? .now.advanced(by: .seconds(30)) : nil
                    guard !done || value != nil else { try Workspace3Wire.fail("truncated_rpc_reply") }
                    next += 1
                    var outcome: Result<Data, Error>?
                    if let value {
                        guard let object = value.objectValue, object["id"] == .int(0), done else { try Workspace3Wire.fail("expected_unary_reply") }
                        try Workspace3Wire.shape(object, ["id"], optional: ["ok", "err"])
                        guard (object["ok"] != nil) != (object["err"] != nil) else { try Workspace3Wire.fail("invalid_reply") }
                        if let ok = object["ok"] { outcome = .success(try Workspace3Wire.data(ok)) }
                        else { outcome = .failure(RelayError.rpc(object["err"]?.stringValue ?? "invalid_remote_error")) }
                    }
                    try await send(["type": .string("ack"), "token": .string(token), "through": .int(next)])
                    if let outcome { return try outcome.get() }
                case "error": throw RelayError.rpc(frame["code"]?.stringValue ?? "delivery_unknown")
                default: try Workspace3Wire.fail("unexpected_rpc_frame")
                }
            }
        }
    }
}

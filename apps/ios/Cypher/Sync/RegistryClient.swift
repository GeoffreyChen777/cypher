// Registry room client — a Swift port of crates/sync/src/registry.rs, the
// text-frame sibling of ChatRoomClient. JSON text frames over one WebSocket
// to /registry/{orgId}/ws:
// hello/cursor handshake, push/ack for pending op batches, merged-row
// broadcasts, presence beats, probe/redial liveness, reconnect with backoff.
//
// The client owns no row semantics: everything applies through the
// WorkspaceStore's RegistryDoc on the main actor (the Swift stand-in for the
// Rust side's doc mutex). Delivery is awaited in the receive loop, so frame
// order is preserved — the server sends the rows broadcast BEFORE the ack for
// your own push (apply rows, then retire the pending batch).
//
// Liveness discipline (shared with the desktop's registry client): the text
// "ping" elicits a runtime auto-pong that proves NOTHING about the DO, so
// room health is judged only by protocol frames — a probe
// unanswered past its deadline tears the session down for a fresh dial.

import Foundation
import os

enum RegistryEvent: Sendable {
    /// The hello's `state` answer (also a late duplicate — applying twice is
    /// harmless and simpler than special-casing).
    case state(seq: UInt64, full: Bool, gcFloor: UInt64, rows: [RegistryRow], presence: [String: Int64])
    /// Joined (or re-joined); the state above has been delivered first.
    case connected
    /// Merged full rows for every touched row (our own op may have lost LWW —
    /// the merged row is the truth to display).
    case rows(seq: UInt64, rows: [RegistryRow])
    /// Our push landed: retire the batch; `seq` advances the cursor.
    case ack(batch: String, seq: UInt64, applied: UInt64)
    /// A remote device's presence beat.
    case presence(device: String, at: Int64)
    /// The connection dropped; unacked batches become pushable again.
    case disconnected
}

actor RegistryClient {
    // Constants mirrored from crates/sync/src/registry.rs; the socket
    // timings shared with the chat client live in RoomSocketLifecycle.Timing.
    static let presenceIntervalNs: UInt64 = 15_000_000_000
    static let maxHttpPullBytes = 8 * 1024 * 1024

    /// MainActor-isolated bridge to the store's RegistryDoc.
    struct Delegate: Sendable {
        var helloCursor: @MainActor @Sendable () -> UInt64?
        var takePushable: @MainActor @Sendable () -> [RegistryPendingBatch]
        /// Clear HTTP in-flight marks after a transient request failure so
        /// the next poll can safely retry the same batch.
        var resetPushable: @MainActor @Sendable () -> Void
        var acknowledge: @MainActor @Sendable (String, UInt64) -> Void
        var event: @MainActor @Sendable (RegistryEvent) -> Void
        /// The viewport's periodic activity refresh, to ride the presence beat.
        /// That beat already goes to this same room every 15s and bills 20:1,
        /// so carrying the refresh is free; the identical report over HTTP cost
        /// a whole billable request. `nil` = nothing to refresh.
        var pendingActivity: @MainActor @Sendable () -> ActivityReport? = { nil }
    }

    private let device: String
    private let urlProvider: @Sendable () async -> URL?
    private let rowsRequest: @Sendable (UInt64?) async -> URLRequest?
    private let pushRequest: @Sendable () async -> URLRequest?
    private let delegate: Delegate
    private let life: RoomSocketLifecycle<RegistryClient>

    private struct HTTPPull: Decodable {
        let seq: UInt64
        let full: Bool
        let gcFloor: UInt64
        let rows: [RegistryRow]
        let presence: [String: Int64]
    }

    private struct PushBody: Encodable {
        let batch: String
        let ops: [RegistryOp]
    }

    private struct HTTPAck: Decodable {
        let batch: String
        let seq: UInt64
    }

    init(device: String,
         urlProvider: @escaping @Sendable () async -> URL?,
         rowsRequest: @escaping @Sendable (UInt64?) async -> URLRequest?,
         pushRequest: @escaping @Sendable () async -> URLRequest?,
         delegate: Delegate,
         transport: any WebSocketTransport = URLSessionWebSocketTransport(),
         clock: any RoomClock = SystemRoomClock()) {
        self.device = device
        self.urlProvider = urlProvider
        self.rowsRequest = rowsRequest
        self.pushRequest = pushRequest
        self.delegate = delegate
        life = RoomSocketLifecycle(transport: transport, clock: clock, hooks: .init(
            label: "registry",
            wake: { client, wake, gen in await client.lifecycle(wake, gen: gen) },
            probe: { client in await client.send(ProbeFrame()) },
            disconnected: { client in await client.delegate.event(.disconnected) },
            timers: [RegistryClient.presenceIntervalNs]))
    }

    // MARK: Lifecycle

    func start() {
        life.start(owner: self)
        connect()
    }

    /// Routes the lifecycle's wake-ups: socket frames, failures and timers.
    private func lifecycle(_ wake: RoomSocketWake, gen: Int) async {
        switch wake {
        case .message(let message): await handleInbound(message, gen: gen)
        case .failed: await life.fail(gen: gen, owner: self)
        case .ping: await life.pingTick(gen: gen, owner: self)
        case .liveness: await life.livenessTick(gen: gen, owner: self)
        case .reconnect: if gen == life.generation { connect() }
        case .timer: await presenceTick(gen: gen)
        case .poll: if life.shouldPoll { await pullSync() }
        }
    }

    /// HTTPS fallback: POST pending LWW batches, then GET the server delta.
    /// The response uses the same JSON state semantics as the WebSocket hello.
    func pullSync() async {
        guard !life.closed else { return }
        let batches = await delegate.takePushable()
        for batch in batches {
            guard var request = await pushRequest() else { break }
            guard let bytes = try? JSONEncoder().encode(PushBody(batch: batch.batch, ops: batch.ops)) else {
                break
            }
            request.httpBody = bytes
            guard let (data, response) = try? await URLSession.shared.data(for: request),
                  let http = response as? HTTPURLResponse else {
                await delegate.resetPushable()
                break
            }
            if http.statusCode != 200 {
                await delegate.resetPushable()
                break
            }
            guard let ack = try? JSONDecoder().decode(HTTPAck.self, from: data) else {
                await delegate.resetPushable()
                break
            }
            await delegate.acknowledge(ack.batch, ack.seq)
        }
        guard let request = await rowsRequest(await delegate.helloCursor()),
              let (data, response) = try? await URLSession.shared.data(for: request),
              data.count <= RegistryClient.maxHttpPullBytes,
              (response as? HTTPURLResponse)?.statusCode == 200,
              let frame = try? JSONDecoder().decode(HTTPPull.self, from: data) else { return }
        await delegate.event(.state(seq: frame.seq, full: frame.full,
                                    gcFloor: frame.gcFloor, rows: frame.rows,
                                    presence: frame.presence))
    }

    func stop() {
        life.stop()
    }

    /// Local writes were enqueued — push pending batches now.
    func nudge() async {
        guard life.joined else { return }
        await pushPending()
    }

    /// Foreground hook (the registry twin of ChatRoomClient.kick): suspension
    /// kills the socket without running any failure path. A dead or unjoined
    /// session redials NOW on fresh backoff; a joined one gets an immediate
    /// deadline-checked probe (post-suspend sockets are half-open more often
    /// than not).
    func kick() async {
        guard !life.closed else { return }
        life.resetBackoff()
        if life.socket == nil || !life.joined {
            connect()
            return
        }
        guard life.probeSentAt == nil, life.helloSentAt == nil else { return }  // already policed
        await life.sendProbe(owner: self)
    }

    /// Reconnect immediately after the store detects a sequence gap. A probe
    /// only proves liveness; it cannot repair rows that were missed.
    func redial() {
        guard !life.closed else { return }
        life.scheduleReconnect(gen: life.generation, owner: self)
    }

    private func connect() {
        guard let gen = life.beginDial() else { return }
        Task {
            guard let url = await urlProvider() else {
                // No URL = no token (refresh failed or signed out) — the most
                // confusing silent failure: everything cached renders, nothing
                // syncs. Say so and back off.
                roomLog.error("registry: no socket URL (token unavailable); backing off")
                await self.life.scheduleReconnect(gen: gen, owner: self)
                return
            }
            await self.openSocket(url: url, gen: gen)
        }
    }

    private func openSocket(url: URL, gen: Int) async {
        guard life.open(URLRequest(url: url), gen: gen, owner: self) else { return }
        // Hello with the persisted cursor (nil asks for full state). The
        // deadline is armed BEFORE the send — an unanswered hello must never
        // hang the session.
        life.armHello()
        let cursor = await delegate.helloCursor()
        await send(HelloFrame(cursor: cursor, device: device))
    }

    // MARK: Timers

    private func presenceTick(gen: Int) async {
        guard gen == life.generation, life.joined else { return }
        await send(PresenceFrame(at: nowMs(), activity: await delegate.pendingActivity()))
    }

    // MARK: Inbound

    private func handleInbound(_ message: URLSessionWebSocketTask.Message, gen: Int) async {
        guard gen == life.generation else { return }
        life.noteInbound()
        guard case .string(let text) = message else { return }
        if text == "pong" { return }  // transport lease refreshed; proves nothing
        let frame: ServerFrame
        do {
            frame = try JSONDecoder().decode(ServerFrame.self, from: Data(text.utf8))
        } catch {
            // Protocol breakdown — same as the Rust client: redial rather
            // than run blind against a server we can't parse.
            roomLog.error("registry: unparseable frame (\(String(describing: error), privacy: .public)); redialing")
            await life.fail(gen: gen, owner: self)
            return
        }
        life.noteProtocolFrame()

        switch frame {
        case .state(let seq, let full, let gcFloor, let rows, let presence):
            life.helloAnswered()
            let wasJoined = life.joined
            life.didJoin()
            await delegate.event(.state(seq: seq, full: full, gcFloor: gcFloor,
                                        rows: rows, presence: presence))
            if !wasJoined {
                roomLog.info("registry: joined (seq=\(seq), full=\(full), rows=\(rows.count))")
                await delegate.event(.connected)
            }
            // Anything pending (offline writes, reseeds) pushes now, and our
            // beat announces this device without waiting for the timer.
            await pushPending()
            await send(PresenceFrame(at: nowMs(), activity: nil))

        case .rows(let seq, let rows):
            await delegate.event(.rows(seq: seq, rows: rows))

        case .ack(let batch, let seq, let applied):
            await delegate.event(.ack(batch: batch, seq: seq, applied: applied))

        case .presence(let device, let at):
            await delegate.event(.presence(device: device, at: at))

        case .probeOk:
            break  // liveness proven; clocks already advanced above

        case .error(let code, let message):
            // Rejections are server-attributed per device; surface loudly —
            // a silent reject looks exactly like a working app.
            roomLog.error("registry: server rejected a frame: \(code, privacy: .public): \(message, privacy: .public)")
        }
    }

    // MARK: Outbound

    private func pushPending() async {
        let batches = await delegate.takePushable()
        for batch in batches {
            await send(PushFrame(batch: batch.batch, ops: batch.ops))
        }
    }

    private func send(_ frame: some Encodable) async {
        guard let socket = life.socket, let data = try? JSONEncoder().encode(frame),
              let text = String(data: data, encoding: .utf8) else { return }
        try? await socket.send(.string(text))
    }
}

// MARK: - Wire frames (JSON text; mirror edge/src/registry-room.ts)

private struct HelloFrame: Encodable {
    var t = "hello"
    var cursor: UInt64?
    var device: String

    // Explicit null for a nil cursor (synthesized Codable would omit the key;
    // the server treats both as "no cursor", but the wire doc says null).
    private enum CodingKeys: String, CodingKey { case t, cursor, device }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(t, forKey: .t)
        try c.encode(cursor, forKey: .cursor)
        try c.encode(device, forKey: .device)
    }
}

private struct PushFrame: Encodable {
    var t = "push"
    var batch: String
    var ops: [RegistryOp]
}

private struct PresenceFrame: Encodable {
    var t = "presence"
    var at: Int64
    /// Omitted entirely when there is nothing to refresh, so an ordinary beat
    /// stays the same frame it has always been.
    var activity: ActivityReport?
}

/// One activity report, used for both transports so the two cannot drift.
///
/// `chatId` is encoded explicitly as null rather than omitted: the synthesized
/// encoder would drop the key, and "no chat" must be stated, not implied.
struct ActivityReport: Encodable, Sendable, Equatable {
    var clientId: String
    var sequence: Int
    var platform = "ios"
    var foreground: Bool
    var interactionAgeMs: Int
    var chatId: String?

    enum CodingKeys: String, CodingKey {
        case clientId, sequence, platform, foreground, interactionAgeMs, chatId
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(clientId, forKey: .clientId)
        try c.encode(sequence, forKey: .sequence)
        try c.encode(platform, forKey: .platform)
        try c.encode(foreground, forKey: .foreground)
        try c.encode(interactionAgeMs, forKey: .interactionAgeMs)
        if let chatId { try c.encode(chatId, forKey: .chatId) }
        else { try c.encodeNil(forKey: .chatId) }
    }

    /// A transition — a different chat, or entering/leaving the foreground —
    /// as opposed to a refresh of a state already reported. Only a transition
    /// needs an HTTP request, because only it needs the reply.
    func isTransition(from previous: ActivityReport?) -> Bool {
        guard let previous else { return true }
        return previous.foreground != foreground || previous.chatId != chatId
    }
}

private struct ProbeFrame: Encodable {
    var t = "probe"
}

private enum ServerFrame: Decodable {
    case state(seq: UInt64, full: Bool, gcFloor: UInt64, rows: [RegistryRow], presence: [String: Int64])
    case rows(seq: UInt64, rows: [RegistryRow])
    case ack(batch: String, seq: UInt64, applied: UInt64)
    case presence(device: String, at: Int64)
    case probeOk
    case error(code: String, message: String)

    private enum Keys: String, CodingKey {
        case t, seq, full, gcFloor, rows, presence, batch, applied, device, at, code, message
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .t) {
        case "state":
            self = .state(seq: try c.decode(UInt64.self, forKey: .seq),
                          full: try c.decode(Bool.self, forKey: .full),
                          gcFloor: try c.decodeIfPresent(UInt64.self, forKey: .gcFloor) ?? 0,
                          rows: try c.decode([RegistryRow].self, forKey: .rows),
                          presence: try c.decodeIfPresent([String: Int64].self, forKey: .presence) ?? [:])
        case "rows":
            self = .rows(seq: try c.decode(UInt64.self, forKey: .seq),
                         rows: try c.decode([RegistryRow].self, forKey: .rows))
        case "ack":
            self = .ack(batch: try c.decode(String.self, forKey: .batch),
                        seq: try c.decode(UInt64.self, forKey: .seq),
                        applied: try c.decodeIfPresent(UInt64.self, forKey: .applied) ?? 0)
        case "presence":
            self = .presence(device: try c.decode(String.self, forKey: .device),
                             at: try c.decode(Int64.self, forKey: .at))
        case "probe-ok":  // the tag has a hyphen — bit the Rust side too
            self = .probeOk
        case "error":
            self = .error(code: try c.decodeIfPresent(String.self, forKey: .code) ?? "unknown",
                          message: try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        case let tag:
            throw DecodingError.dataCorrupted(.init(codingPath: [Keys.t],
                                                    debugDescription: "unknown frame: \(tag)"))
        }
    }
}

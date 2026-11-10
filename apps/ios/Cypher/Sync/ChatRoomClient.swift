// chat2 room client — a Swift port of crates/sync/src/chat_client.rs, the
// binary-frame sibling of RegistryClient. One WebSocket per chat to
// /chat2/{chatId}/ws: hello/state handshake with client-side checkpoint
// precision (the frontier payload decides fetch-vs-skip), cursor-based row
// backfill, push/ack with a pending-unacked batch queue, probe/redial
// liveness, reconnect with exponential backoff.
//
// The client owns no CRDT semantics: update bytes flow through main-actor
// delegate closures into SessionStore's doc, which persists doc content AND
// the room cursor in one file write (the C2 rule — they can never diverge).
//
// Liveness discipline is inherited from the registry client and its
// incidents: the text "ping" elicits a runtime auto-pong that proves NOTHING
// about the DO, so room health is judged only by protocol frames against
// hard deadlines.

import Foundation
import os

/// Sync must never fail silently: a send that never left the device is
/// otherwise indistinguishable from a working one. Visible in Console.app /
/// `log stream` under this subsystem.
let roomLog = Logger(subsystem: "ai.mvp-lab.cypher.ios", category: "sync")

enum ChatRoomEvent: Sendable {
    /// Joined (or re-joined) and the initial catch-up (checkpoint if needed +
    /// row backfill) has been delivered through the apply closures.
    case connected
    /// A catch-up started (true) or ended (false): its rows arrive in a
    /// burst, which the delegate need not show one by one.
    case catchingUp(Bool)
    /// The connection dropped; the client is backing off before redialing.
    case disconnected
}

actor ChatRoomClient {
    // Constants mirrored from crates/sync/src/chat_client.rs; the socket
    // timings shared with the registry client live in
    // RoomSocketLifecycle.Timing.
    /// A catch-up (checkpoint fetch + row backfill) that makes no progress
    /// for this long is redialed. Progress, not total time: a long session's
    /// checkpoint is megabytes, which a slow link can take minutes to bring,
    /// and a total deadline cut it off and started it over without end.
    static let backfillDeadlineNs: UInt64 = 120_000_000_000
    /// Re-push cadence after a `quota` rejection (server window is 60s).
    static let quotaRetryNs: UInt64 = 5_000_000_000
    /// Client-side push cap: the DO's per-row cap (1 MiB) minus frame-header
    /// headroom — the runtime closes WS messages at 1 MiB BEFORE the DO runs,
    /// so an over-cap payload would die with no error frame to retire it.
    static let maxPushBytes = 1024 * 1024 - 4096

    /// MainActor-isolated bridge to SessionStore's doc. Every apply persists
    /// doc + cursor together; `applyCheckpoint` returns false on an import
    /// failure (the session redials rather than run blind).
    struct Delegate: Sendable {
        var cursor: @MainActor @Sendable () -> UInt64
        /// The cursor covers no row whose operations the doc parked, so the
        /// one-time cursor amnesty has nothing to refetch.
        var cursorHonest: @MainActor @Sendable () -> Bool = { false }
        var containsFrontier: @MainActor @Sendable (Data) -> Bool
        var applyCheckpoint: @MainActor @Sendable (Data, UInt64) async -> Bool
        var applyRow: @MainActor @Sendable (Data, UInt64) -> Void
        var advanceCursor: @MainActor @Sendable (UInt64) -> Void
        var clampCursor: @MainActor @Sendable (UInt64) -> Void
        var setCursor: @MainActor @Sendable (UInt64) -> Void
        var event: @MainActor @Sendable (ChatRoomEvent) -> Void
        var preview: @MainActor @Sendable (Data?) -> [Data] = { _ in [] }
        var previewRetry: @MainActor @Sendable () -> Data? = { nil }
    }

    private struct PendingPush {
        var batchId: String
        var bytes: Data
        /// In flight on the CURRENT connection (cleared on disconnect so
        /// reconnects re-push; the server dedupes replays by batchId).
        var inFlight = false
    }

    private let chatId: String
    private let device: String
    private let urlProvider: @Sendable () async -> URL?
    private let checkpointRequest: @Sendable () async -> URLRequest?
    private let rowsRequest: @Sendable (UInt64) async -> URLRequest?
    private let pushRequest: @Sendable (String) async -> URLRequest?
    private let delegate: Delegate
    private let previewEnabled: Bool
    private let life: RoomSocketLifecycle<ChatRoomClient>

    private var quotaTask: Task<Void, Never>?
    private var pending: [PendingPush] = []
    /// First backfill of THIS client instance must NOT exclude own rows: the
    /// pending queue doesn't survive restarts, and a restored device's doc
    /// may be missing its own post-backup writes — they exist only on the
    /// server. Loro re-import is a no-op, so redownloading own bytes once is
    /// pure safety; same-process reconnects skip them.
    private var resumed = false
    /// One-time cursor amnesty: a persisted cursor may have advanced over
    /// Loro operations that were parked and later disappeared from export.
    private var cursorAmnestyDone = false
    /// A live row/ack jumped over the honest cursor; repair from that cursor.
    private var gapRepair = false
    private var gapRepairs = 0
    /// Set while the catch-up after the hello answer is in flight: when it
    /// last made progress (the state answer, a row, checkpoint bytes).
    private var backfillProgressAt: UInt64?
    /// An HTTPS pull is applying its frames.
    private var pulling = false
    /// What the delegate was last told about `catchingUp`.
    private var reportedCatchUp = false
    /// The one load (fetch + import) of the room's checkpoint. It belongs to
    /// the client, not to a socket: a redial while it runs joins it instead
    /// of starting the download over, and the HTTPS pull shares it instead of
    /// downloading the same megabytes alongside.
    private var checkpointLoad: (seq: UInt64, size: UInt64, task: Task<Bool, Never>)?
    /// The bytes a cut-off checkpoint download got, for the next one to
    /// resume by Range: (the checkpoint's seq stamp, bytes).
    private var checkpointPartial: (seq: String, bytes: Data)?
    private let checkpointProgress = FetchProgress()
    private var checkpointBytesSeen = 0

    init(
        chatId: String,
        device: String,
        urlProvider: @escaping @Sendable () async -> URL?,
        checkpointRequest: @escaping @Sendable () async -> URLRequest?,
        rowsRequest: @escaping @Sendable (UInt64) async -> URLRequest?,
        pushRequest: @escaping @Sendable (String) async -> URLRequest?,
        delegate: Delegate, previewEnabled: Bool = false,
        transport: any WebSocketTransport = URLSessionWebSocketTransport(),
        clock: any RoomClock = SystemRoomClock()
    ) {
        self.chatId = chatId
        self.device = device
        self.urlProvider = urlProvider
        self.checkpointRequest = checkpointRequest
        self.rowsRequest = rowsRequest
        self.pushRequest = pushRequest
        self.delegate = delegate
        self.previewEnabled = previewEnabled
        life = RoomSocketLifecycle(
            transport: transport, clock: clock,
            hooks: .init(
                label: "chat2 \(chatId)",
                wake: { client, wake, gen in await client.lifecycle(wake, gen: gen) },
                probe: { client in await client.send(ChatWire.encode(ChatFrameType.probe, header: [:])) },
                disconnected: { client in await client.delegate.event(.disconnected) }))
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
        case .failed: await onSocketError(gen: gen)
        case .ping: await life.pingTick(gen: gen, owner: self)
        case .liveness: await livenessTick(gen: gen)
        case .reconnect: if gen == life.generation { connect() }
        case .timer: break
        // A socket mid-catch-up has proven the room answers: a pull now would
        // only fetch the same rows over a link that is already the bottleneck.
        case .poll: if life.shouldPoll, backfillProgressAt == nil { await pullSync() }
        }
    }

    /// Tells the delegate whether a catch-up (the socket's or a pull's) is
    /// in flight, when that changes.
    private func reportCatchUp() async {
        let catchingUp = backfillProgressAt != nil || pulling
        guard catchingUp != reportedCatchUp else { return }
        reportedCatchUp = catchingUp
        await delegate.event(.catchingUp(catchingUp))
    }

    /// HTTPS fallback/parallel convergence path. Pushes are retried
    /// idempotently by batch id, then the framed pull is applied through the
    /// same state/row/checkpoint rules as the WebSocket path.
    func pullSync() async {
        guard !life.closed else { return }
        for push in pending {
            guard var request = await pushRequest(push.batchId) else { break }
            request.httpBody = push.bytes
            guard let (data, response) = try? await URLSession.shared.data(for: request),
                let http = response as? HTTPURLResponse
            else { break }
            if http.statusCode == 200,
                let ack = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                let seq = (ack["seq"] as? NSNumber)?.uint64Value
            {
                pending.removeAll { $0.batchId == push.batchId }
                // A Chat2 ACK is not permission to skip an interleaved row.
                let cursor = await delegate.cursor()
                if seq == cursor + 1 {
                    await delegate.advanceCursor(seq)
                }
            } else if (400...499).contains(http.statusCode) {
                // Retire only a parsed permanent edge verdict. An arbitrary
                // proxy/captive-portal response must not discard the update.
                let code =
                    (try? JSONSerialization.jsonObject(with: data))
                    .flatMap { $0 as? [String: Any] }?["error"] as? String
                if ["bad_push", "too_large", "empty"].contains(code) {
                    pending.removeAll { $0.batchId == push.batchId }
                } else {
                    break
                }
            } else {
                break
            }
        }
        let after = await delegate.cursor()
        guard let request = await rowsRequest(after),
            let (body, response) = try? await URLSession.shared.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200
        else { return }
        var frames: [ChatWireFrame] = []
        var offset = 0
        while offset + 4 <= body.count {
            let length = body[offset..<(offset + 4)].withUnsafeBytes {
                Int($0.loadUnaligned(as: UInt32.self).littleEndian)
            }
            offset += 4
            guard length > 0, offset + length <= body.count else { return }
            guard let frame = ChatWire.decode(body.subdata(in: offset..<(offset + length))) else {
                return
            }
            frames.append(frame)
            offset += length
        }
        guard offset == body.count else { return }
        guard let stateFrame = frames.first,
            stateFrame.kind == ChatFrameType.state,
            let state = ChatStateHeader(stateFrame.header)
        else { return }
        let contained: Bool
        if state.checkpointSize == 0 {
            contained = true
        } else {
            contained = await delegate.containsFrontier(stateFrame.payload)
        }
        let plan = chatPlanCatchUp(
            cursor: after, state: state,
            frontierContained: contained)
        let plannedAfter: UInt64
        switch plan {
        case .checkpointThenRows(let checkpointAfter):
            guard await loadCheckpoint(state) else { return }
            plannedAfter = checkpointAfter
        case .rowsOnly(let cursor):
            plannedAfter = cursor
        }
        if plannedAfter != after {
            await delegate.setCursor(plannedAfter)
        }
        guard !life.closed else { return }
        pulling = true
        await reportCatchUp()
        for frame in frames.dropFirst()
        where frame.kind == ChatFrameType.row || frame.kind == ChatFrameType.rowsDone {
            await applyPullFrame(frame)
        }
        pulling = false
        await reportCatchUp()
    }

    private func applyPullFrame(_ frame: ChatWireFrame) async {
        switch frame.kind {
        case ChatFrameType.row:
            guard let seq = (frame.header["seq"] as? NSNumber)?.uint64Value else { return }
            let cursor = await delegate.cursor()
            if seq > cursor + 1 {
                roomLog.warning("chat2 \(self.chatId, privacy: .public): HTTPS row gap (seq=\(seq), cursor=\(cursor))")
                await delegate.applyRow(frame.payload, cursor)
                return
            }
            await delegate.applyRow(frame.payload, seq)
        case ChatFrameType.rowsDone:
            // An HTTPS pull completed, but it must not mark the WebSocket as
            // joined: polling continues until the socket itself reaches its
            // protocol state, otherwise one successful pull would disable
            // future polling on a WS-hostile network.
            await delegate.event(.connected)
        default:
            break
        }
    }

    func stop() {
        life.stop()
        checkpointLoad?.task.cancel()
        checkpointLoad = nil
        checkpointPartial = nil
    }

    /// Local batches the server hasn't acked yet. The queue is in-memory
    /// only, so stopping the client now would drop them.
    var hasPendingPushes: Bool { !pending.isEmpty }

    /// Queue one local update batch for push. The batch survives reconnects
    /// until acked — the server dedupes replays by batchId.
    ///
    /// Batches over `maxPushBytes` are refused here: the server can never
    /// accept them, and a queued-forever batch would replay on every
    /// reconnect — the exact wedge class chat2 replaces. The ops stay in the
    /// local doc; on a viewer device there is no checkpoint fallback, so
    /// this is loud (post-strip updates are KB-scale — an over-cap update is
    /// an upstream bug).
    func enqueue(update: Data) {
        guard update.count <= ChatRoomClient.maxPushBytes else {
            roomLog.error(
                "chat2 \(self.chatId, privacy: .public): update \(update.count)B exceeds the row cap; not queued")
            return
        }
        pending.append(PendingPush(batchId: UUID().uuidString.lowercased(), bytes: update))
        if life.joined {
            Task { await self.pushPending() }
        }
    }

    /// Foreground hook (the chat2 twin of RegistryClient.kick): suspension
    /// kills the socket without running any failure path. A dead or unjoined
    /// session redials NOW on fresh backoff; a joined one gets an immediate
    /// deadline-checked probe (post-suspend sockets are half-open more often
    /// than not).
    func kick() async {
        guard !life.closed else { return }
        life.resetBackoff()
        if life.socket == nil {
            connect()
            return
        }
        if !life.joined {
            // A handshake/catch-up is already in flight on this socket — its
            // own deadlines police it (hello 15s, backfill 120s). Redialing
            // here abandoned a healthy catch-up and refetched the checkpoint
            // (observed on launch: foregrounded() fires during the first
            // join). A zombie socket with NO handshake pending is redialed.
            if life.helloSentAt == nil, backfillProgressAt == nil {
                connect()
            }
            return
        }
        guard life.probeSentAt == nil else { return }
        await life.sendProbe(owner: self)
    }

    private func connect() {
        guard let gen = life.beginDial() else { return }
        backfillProgressAt = nil
        gapRepair = false
        gapRepairs = 0
        for ix in pending.indices {
            pending[ix].inFlight = false
        }

        Task {
            if previewEnabled { _ = await delegate.previewRetry() }
            guard let url = await urlProvider() else {
                // No URL = no token (refresh failed or signed out) — the most
                // confusing silent failure: everything cached renders,
                // nothing syncs. Say so and back off.
                roomLog.error("chat2 \(self.chatId, privacy: .public): no socket URL (token unavailable); backing off")
                self.life.scheduleReconnect(gen: gen, owner: self)
                return
            }
            await self.openSocket(url: url, gen: gen)
        }
    }

    private func openSocket(url: URL, gen: Int) async {
        var request = URLRequest(url: url)
        if previewEnabled {
            request.setValue(StreamPreviewWire.capability, forHTTPHeaderField: "x-cypher-preview-capability")
        }
        guard life.open(request, gen: gen, owner: self) else { return }
        // Hello with the persisted cursor. The deadline is armed BEFORE the
        // send — an unanswered hello must never hang the session.
        life.armHello()
        let cursor = await delegate.cursor()
        await send(
            ChatWire.encode(
                ChatFrameType.hello,
                header: ["cursor": cursor, "device": device]))
    }

    private func onSocketError(gen: Int) async {
        if previewEnabled, gen == life.generation { _ = await delegate.preview(nil) }
        if gen == life.generation {
            backfillProgressAt = nil
            await reportCatchUp()
        }
        await life.fail(gen: gen, owner: self)
    }

    // MARK: Timers

    /// Hello answers, the catch-up, and probes run against hard deadlines; a
    /// long-quiet joined room gets a probe. Any protocol frame clears the
    /// probe deadline; catch-up progress (rows landing, checkpoint bytes
    /// arriving) renews the catch-up deadline, so a draining backfill or a
    /// slow download is never killed mid-stream.
    private func livenessTick(gen: Int) async {
        guard gen == life.generation, life.socket != nil, !life.closed else { return }
        if previewEnabled, let frame = await delegate.previewRetry() { await send(frame) }
        // The receive loop waits out the checkpoint load inside the state
        // answer, so the socket's own frames (pongs included) go unread
        // meanwhile: bytes still arriving are what prove the link alive. A
        // silence lease that ignored them redialed every download longer
        // than 45 seconds.
        let received = checkpointProgress.bytes
        if received != checkpointBytesSeen {
            checkpointBytesSeen = received
            if backfillProgressAt != nil {
                backfillProgressAt = life.clock.now()
                life.noteInbound()
            }
        }
        await life.livenessTick(gen: gen, owner: self) { now in
            guard let progress = backfillProgressAt, now - progress > ChatRoomClient.backfillDeadlineNs else {
                return nil
            }
            return "backfill made no progress within deadline; redialing"
        }
    }
    // MARK: Inbound

    private func handleInbound(_ message: URLSessionWebSocketTask.Message, gen: Int) async {
        guard gen == life.generation else { return }
        life.noteInbound()
        guard case .data(let data) = message else { return }  // "pong" text
        if previewEnabled, let first = data.first, (0x20...0x26).contains(first) {
            for reply in await delegate.preview(data) { await send(reply) }
            return  // Preview must not satisfy the durable progress/probe deadline.
        }
        guard let frame = ChatWire.decode(data) else {
            // Protocol breakdown — redial rather than run blind against a
            // server we can't parse.
            roomLog.error("chat2 \(self.chatId, privacy: .public): unparseable frame; redialing")
            await onSocketError(gen: gen)
            return
        }
        if previewEnabled, frame.kind == ChatFrameType.error, let code = frame.header["code"] as? String,
            code.hasPrefix("preview_") || code.hasPrefix("bad_preview_")
        {
            return  // A late receipt/Resume error cannot revoke a newer grant or satisfy a durable probe.
        }
        life.noteProtocolFrame()

        switch frame.kind {
        case ChatFrameType.state:
            await handleState(frame, gen: gen)

        case ChatFrameType.row:
            guard let seq = (frame.header["seq"] as? NSNumber)?.uint64Value else { return }
            // Own-device rows can still arrive (first-backfill redownload, a
            // racing second socket) — Loro re-import is a no-op; the cursor
            // advance is what matters.
            if backfillProgressAt != nil { backfillProgressAt = life.clock.now() }
            let cursor = await delegate.cursor()
            if seq > cursor + 1 {
                gapRepair = true
                roomLog.warning(
                    "chat2 \(self.chatId, privacy: .public): row gap (seq=\(seq), cursor=\(cursor)); holding cursor")
                await delegate.applyRow(frame.payload, cursor)
            } else {
                await delegate.applyRow(frame.payload, seq)
            }
            await maybeRepairGap(gen: gen)

        case ChatFrameType.rowsDone:
            guard backfillProgressAt != nil else { return }
            backfillProgressAt = nil
            let wasResumed = resumed
            resumed = true
            life.didJoin()
            roomLog.info("chat2 \(self.chatId, privacy: .public): joined (converged, resumed=\(wasResumed))")
            await reportCatchUp()
            await delegate.event(.connected)
            // Anything pending (offline writes, reconnect re-pushes) goes
            // now — the server's batchId dedupe makes replays exact no-ops.
            await pushPending()
            await maybeRepairGap(gen: gen)

        case ChatFrameType.ack:
            guard let batchId = frame.header["batchId"] as? String,
                let seq = (frame.header["seq"] as? NSNumber)?.uint64Value
            else { return }
            pending.removeAll { $0.batchId == batchId }
            let cursor = await delegate.cursor()
            if seq > cursor + 1 {
                gapRepair = true
                roomLog.warning(
                    "chat2 \(self.chatId, privacy: .public): ack gap (seq=\(seq), cursor=\(cursor)); holding cursor")
            } else {
                await delegate.advanceCursor(seq)
            }
            // A grant after a quota rejection: drain whatever the error
            // handler un-flagged (sparse mobile queues — no livelock risk).
            await pushPending()
            await maybeRepairGap(gen: gen)

        case ChatFrameType.probeOk:
            break  // liveness proven; clocks already advanced above

        case ChatFrameType.presence:
            break  // no consumer on mobile today

        case ChatFrameType.error:
            await handleErrorFrame(frame.header, gen: gen)

        default:
            // Unknown server frame: tolerate (future protocol additions).
            break
        }
    }

    /// The hello answer: plan the catch-up (chat_client.rs run_session).
    private func handleState(_ frame: ChatWireFrame, gen: Int) async {
        guard let state = ChatStateHeader(frame.header) else {
            roomLog.error("chat2 \(self.chatId, privacy: .public): malformed state header; redialing")
            await onSocketError(gen: gen)
            return
        }
        guard life.helloSentAt != nil else { return }  // late duplicate — ignore
        life.helloAnswered()
        backfillProgressAt = life.clock.now()
        await reportCatchUp()

        let cursor = await delegate.cursor()
        if cursor > state.headSeq {
            // Server behind our cursor = the room was reset/wiped. A viewer
            // owes no re-seed (that is the host's checkpoint duty) — treat
            // the cursor as fresh and reload from what the room has.
            roomLog.warning(
                "chat2 \(self.chatId, privacy: .public): server lost state (headSeq=\(state.headSeq) < cursor=\(cursor)); treating as fresh"
            )
        }
        // Once per client, clamp a possibly dishonest persisted cursor to the
        // checkpoint (or zero when no checkpoint exists) so parked rows are
        // fetched again. Re-imports are idempotent and the room's compaction
        // policy bounds the normal repair window. A cursor the delegate can
        // vouch for has nothing to repair: clamping it refetched every row
        // since the checkpoint on each cold open.
        if !cursorAmnestyDone {
            cursorAmnestyDone = true
            if !(await delegate.cursorHonest()) {
                await delegate.clampCursor(state.checkpointSize > 0 ? state.checkpointSeq : 0)
            }
        }
        // Same presence rule as chatPlanCatchUp: SIZE, not seq — a seeded
        // room's checkpoint covers seq 0.
        var contained = state.checkpointSize == 0
        if !contained {
            contained = await delegate.containsFrontier(frame.payload)
        }
        let planCursor = await delegate.cursor()
        let plan = chatPlanCatchUp(cursor: planCursor, state: state, frontierContained: contained)
        let after: UInt64
        switch plan {
        case .rowsOnly(let a):
            after = a
        case .checkpointThenRows(let a):
            let loaded = await loadCheckpoint(state)
            guard gen == life.generation, !life.closed else { return }
            guard loaded else {
                roomLog.warning("chat2 \(self.chatId, privacy: .public): checkpoint not loaded; redialing")
                await onSocketError(gen: gen)
                return
            }
            after = a
        }
        // A contained checkpoint can move the plan upward; amnesty/reset can
        // move it downward. Persist the plan's cursor before requesting rows.
        await delegate.setCursor(after)
        await send(
            ChatWire.encode(
                ChatFrameType.rowsReq,
                header: ["after": after, "excludeOwn": resumed]))
    }

    /// Request rows from the last honest cursor after a row/ack gap. The
    /// repair budget is per socket session; exhaustion forces a reconnect and
    /// therefore a fresh full catch-up.
    private func maybeRepairGap(gen: Int) async {
        guard gapRepair, gen == life.generation, life.socket != nil else { return }
        gapRepair = false
        gapRepairs += 1
        guard gapRepairs <= 3 else {
            roomLog.warning("chat2 \(self.chatId, privacy: .public): gap repairs exhausted; redialing")
            await onSocketError(gen: gen)
            return
        }
        let after = await delegate.cursor()
        roomLog.info(
            "chat2 \(self.chatId, privacy: .public): backfilling over a row gap (after=\(after), attempt \(self.gapRepairs))"
        )
        await send(
            ChatWire.encode(
                ChatFrameType.rowsReq,
                header: ["after": after, "excludeOwn": false]))
    }

    private func handleErrorFrame(_ header: [String: Any], gen: Int) async {
        let code = header["code"] as? String ?? "?"
        let message = header["message"] as? String ?? ""
        let batchId = header["batchId"] as? String ?? ""
        roomLog.error(
            "chat2 \(self.chatId, privacy: .public): server rejected a frame: \(code, privacy: .public): \(message, privacy: .public)"
        )
        switch code {
        case "too_large", "empty", "bad_push":
            // Permanent verdicts on a specific batch: retire it, or it
            // replays on every reconnect forever — the wedge class this
            // design exists to kill. The ops stay in the local doc.
            if !batchId.isEmpty {
                pending.removeAll { $0.batchId == batchId }
            }
        case "quota":
            // Transient: the window passes on its own — keep the batch
            // queued and probe with the HEAD batch on a short clock
            // (one-per-grant; a full-queue replay would itself consume the
            // server's quota window).
            for ix in pending.indices {
                pending[ix].inFlight = false
            }
            quotaTask?.cancel()
            let clock = life.clock
            let retry = Task { [weak self] in
                await clock.sleep(nanoseconds: ChatRoomClient.quotaRetryNs)
                guard !Task.isCancelled, let self else { return }
                await self.pushHead(gen: gen)
            }
            quotaTask = retry
            life.adopt(retry)
        case "hello_first":
            // Session state desynced from the server — start over.
            await onSocketError(gen: gen)
        default:
            break
        }
    }

    // MARK: Outbound

    private func pushPending() async {
        guard life.joined else { return }
        for ix in pending.indices where !pending[ix].inFlight {
            pending[ix].inFlight = true
            let push = pending[ix]
            await send(
                ChatWire.encode(
                    ChatFrameType.push,
                    header: ["batchId": push.batchId],
                    payload: push.bytes))
        }
    }

    /// Send only the queue's head batch — the quota-probe path.
    private func pushHead(gen: Int) async {
        guard gen == life.generation, life.joined, let head = pending.first else { return }
        pending[0].inFlight = true
        await send(
            ChatWire.encode(
                ChatFrameType.push,
                header: ["batchId": head.batchId],
                payload: head.bytes))
    }

    private func send(_ frame: Data) async {
        guard let socket = life.socket else { return }
        try? await socket.send(.data(frame))
    }

    // MARK: Checkpoint fetch (GET /chat2/{chatId}/checkpoint, Range resume)

    /// Fetch and import the checkpoint `state` names, once however many
    /// callers wait on it (`checkpointLoad`). True once it is in the doc.
    private func loadCheckpoint(_ state: ChatStateHeader) async -> Bool {
        if let load = checkpointLoad, load.seq == state.checkpointSeq, load.size == state.checkpointSize {
            return await load.task.value
        }
        checkpointLoad?.task.cancel()
        let seq = state.checkpointSeq
        roomLog.info(
            "chat2 \(self.chatId, privacy: .public): fetching checkpoint (seq=\(seq), \(state.checkpointSize)B)")
        let size = Int(state.checkpointSize)
        let task = Task { await self.fetchAndApplyCheckpoint(seq: seq, size: size) }
        checkpointLoad = (seq, state.checkpointSize, task)
        let loaded = await task.value
        // Done either way: a failed load is retried by a fresh one, which
        // resumes from `checkpointPartial`.
        if checkpointLoad?.task == task { checkpointLoad = nil }
        return loaded
    }

    private func fetchAndApplyCheckpoint(seq: UInt64, size: Int) async -> Bool {
        guard let bytes = await fetchCheckpoint(seq: seq, size: size) else {
            roomLog.warning("chat2 \(self.chatId, privacy: .public): checkpoint fetch failed")
            return false
        }
        guard !Task.isCancelled else { return false }
        guard await delegate.applyCheckpoint(bytes, seq) else {
            roomLog.error("chat2 \(self.chatId, privacy: .public): checkpoint import failed")
            return false
        }
        return true
    }

    /// Range-resume loop (chat2_host.rs EdgeCheckpointFetcher): each attempt
    /// continues at the byte where the last one stopped; the DO stamps every
    /// response with the checkpoint's seq, and a mid-download replacement
    /// restarts from 0 (a Range against a new blob would splice two blobs).
    /// A download cut off for good (attempts spent, or cancelled) leaves its
    /// bytes in `checkpointPartial`, and the next fetch of the same
    /// checkpoint resumes from them rather than from byte 0.
    private func fetchCheckpoint(seq: UInt64, size: Int) async -> Data? {
        var got = Data()
        var seenSeq: String?
        if let partial = checkpointPartial, UInt64(partial.seq) == seq {
            got = partial.bytes
            seenSeq = partial.seq
            roomLog.info("chat2 \(self.chatId, privacy: .public): resuming checkpoint at \(got.count)B")
        }
        checkpointPartial = nil
        // Cut off after its last byte: nothing is left to ask for (a Range
        // past the end is a 416).
        if size > 0, got.count == size { return got }
        for _ in 0..<4 {
            guard !Task.isCancelled else { break }
            guard var request = await checkpointRequest() else { break }
            if !got.isEmpty {
                request.setValue("bytes=\(got.count)-", forHTTPHeaderField: "Range")
            }
            let fetch = await ChunkedFetch.run(request, progress: checkpointProgress)
            guard let http = fetch.response else { continue }
            let seq = http.value(forHTTPHeaderField: "x-chat2-checkpoint-seq")
            if let seq {
                if let prev = seenSeq, seq != prev {
                    roomLog.info("chat2 \(self.chatId, privacy: .public): checkpoint replaced mid-download; restarting")
                    got.removeAll()
                    seenSeq = seq
                    continue
                }
                seenSeq = seq
            }
            switch http.statusCode {
            case 200: got = fetch.body
            case 206: got.append(fetch.body)
            default:
                roomLog.warning("chat2 \(self.chatId, privacy: .public): checkpoint HTTP \(http.statusCode)")
                return nil
            }
            if !fetch.dropped { return got }
            // Mid-body drop: keep the bytes, resume via Range.
            roomLog.warning(
                "chat2 \(self.chatId, privacy: .public): checkpoint stream dropped at \(got.count)B; resuming")
        }
        if !got.isEmpty, let seenSeq { checkpointPartial = (seenSeq, got) }
        return nil
    }
}

/// Checkpoint bytes received so far, over every download: what tells the
/// catch-up deadline a slow download is still moving.
final class FetchProgress: @unchecked Sendable {
    private let lock = NSLock()
    private var total = 0

    var bytes: Int { lock.withLock { total } }

    func add(_ count: Int) {
        lock.withLock { total += count }
    }
}

/// One GET whose body arrives in the chunks URLSession receives, and is kept
/// when the transfer drops partway (the checkpoint fetch resumes it by
/// Range). A long session's checkpoint runs to megabytes: awaiting it a byte
/// at a time through `URLSession.bytes` cost ~1s of CPU per 7.5MB on a Mac,
/// several times that on a phone, before its first message could show.
private final class ChunkedFetch: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    struct Result: Sendable {
        var response: HTTPURLResponse?
        var body: Data
        /// The transfer failed after its response: `body` is a prefix.
        var dropped: Bool
    }

    private let lock = NSLock()
    private var response: HTTPURLResponse?
    private var body = Data()
    private var continuation: CheckedContinuation<Result, Never>?
    private var progress: FetchProgress?

    static func run(_ request: URLRequest, progress: FetchProgress? = nil) async -> Result {
        let fetch = ChunkedFetch()
        fetch.progress = progress
        let task = URLSession.shared.dataTask(with: request)
        task.delegate = fetch
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                fetch.lock.withLock { fetch.continuation = continuation }
                task.resume()
            }
        } onCancel: {
            task.cancel()
        }
    }

    func urlSession(
        _ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        lock.withLock {
            self.response = response as? HTTPURLResponse
            if response.expectedContentLength > 0 {
                body.reserveCapacity(Int(response.expectedContentLength))
            }
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        lock.withLock { body.append(data) }
        progress?.add(data.count)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        lock.lock()
        let waiter = continuation
        continuation = nil
        let result = Result(response: response, body: body, dropped: error != nil)
        lock.unlock()
        waiter?.resume(returning: result)
    }
}

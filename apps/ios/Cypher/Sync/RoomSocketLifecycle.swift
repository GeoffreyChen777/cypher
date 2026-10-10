// Socket lifecycle shared by ChatRoomClient and RegistryClient (the Rust
// clients, crates/sync/src/chat_client.rs and registry.rs, keep the same
// rules): dial with exponential backoff, one generation per dial so stale
// callbacks are dropped, a transport ping lease, hard deadlines on the hello
// answer and on probes, and the HTTPS fallback poll while not joined.
//
// The text "ping" elicits a runtime auto-pong that proves NOTHING about the
// Durable Object, so room health is judged only by protocol frames.

import Foundation

/// A wake-up from one of the lifecycle's tasks, routed by the owning client.
enum RoomSocketWake: Sendable {
    case message(URLSessionWebSocketTask.Message)
    /// The socket's receive failed.
    case failed
    case ping
    case liveness
    /// A backoff delay elapsed; redial if the generation is still current.
    case reconnect
    /// One of the owner's extra timers (`Hooks.timers`, by index).
    case timer(Int)
    /// The HTTPS fallback interval elapsed.
    case poll
}

/// Owned by one client actor and only used on it: the async methods are
/// `nonisolated(nonsending)`, so they run on the calling actor, and the tasks
/// this spawns reach the owner only through `Hooks`.
final class RoomSocketLifecycle<Owner: Actor> {
    struct Timing: Sendable {
        var pingIntervalNs: UInt64 = 15_000_000_000
        var silenceLeaseNs: UInt64 = 45_000_000_000
        var helloDeadlineNs: UInt64 = 15_000_000_000
        var probeDeadlineNs: UInt64 = 10_000_000_000
        /// A joined room this quiet gets a probe (15 minutes).
        var probeQuietNs: UInt64 = 900_000_000_000
        var livenessTickNs: UInt64 = 1_000_000_000
        var backoffBaseMs = 250
        var backoffCapMs = 30_000
        var httpPollNs: UInt64 = 20_000_000_000
    }

    /// The client-specific parts. Each closure is handed the owner, so none
    /// captures it.
    struct Hooks: Sendable {
        /// Log prefix: "registry", "chat2 <chatId>".
        var label: String
        /// Routes a wake-up into the owner, which normally calls back here.
        var wake: @Sendable (Owner, RoomSocketWake, Int) async -> Void
        /// Sends the client's probe frame.
        var probe: @Sendable (Owner) async -> Void
        /// The session dropped; tell the client's delegate.
        var disconnected: @Sendable (Owner) async -> Void
        /// Extra periodic timers for a joined session, by interval.
        var timers: [UInt64] = []
    }

    let timing: Timing
    let clock: any RoomClock
    private let transport: any WebSocketTransport
    private let hooks: Hooks

    private(set) var socket: (any WebSocketConnection)?
    private var tasks: [Task<Void, Never>] = []
    private var pollTask: Task<Void, Never>?
    private(set) var generation = 0
    private(set) var closed = false
    private(set) var joined = false
    private(set) var backoffMs: Int
    /// Transport clock — pongs count, so a healthy socket never trips it.
    private var lastInbound: UInt64
    /// Protocol clock — only real frames count (pongs prove nothing).
    private var lastProtocolRx: UInt64
    /// Set while the hello awaits its answer.
    private(set) var helloSentAt: UInt64?
    /// Set while a probe awaits its answer.
    private(set) var probeSentAt: UInt64?

    init(timing: Timing = Timing(), transport: any WebSocketTransport, clock: any RoomClock, hooks: Hooks) {
        self.timing = timing
        self.transport = transport
        self.clock = clock
        self.hooks = hooks
        backoffMs = timing.backoffBaseMs
        lastInbound = clock.now()
        lastProtocolRx = clock.now()
    }

    /// The HTTPS fallback runs only while the socket is not joined.
    var shouldPoll: Bool { !closed && !joined }

    func socket(for gen: Int) -> (any WebSocketConnection)? {
        gen == generation ? socket : nil
    }

    // MARK: Session

    /// Reopens after `stop()` and starts the fallback poll: one `.poll` wake
    /// now, then one per interval.
    func start(owner: Owner) {
        closed = false
        pollTask?.cancel()
        let hooks = hooks
        let clock = clock
        let interval = timing.httpPollNs
        pollTask = Task { [weak owner] in
            guard let first = owner else { return }
            await hooks.wake(first, .poll, 0)
            while !Task.isCancelled {
                await clock.sleep(nanoseconds: interval)
                guard let owner, !Task.isCancelled else { return }
                await hooks.wake(owner, .poll, 0)
            }
        }
    }

    func stop() {
        closed = true
        generation += 1
        cancelTasks()
        pollTask?.cancel()
        pollTask = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        joined = false
    }

    /// Starts a dial and returns its generation (nil once stopped). A socket
    /// it supersedes (a kick over a live one) is closed and its timers
    /// stopped rather than left running unowned.
    func beginDial() -> Int? {
        guard !closed else { return nil }
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        cancelTasks()
        generation += 1
        joined = false
        helloSentAt = nil
        probeSentAt = nil
        lastProtocolRx = clock.now()
        return generation
    }

    /// Opens the socket for `gen` and starts its receive loop and timers.
    /// False when the dial was superseded or the client stopped meanwhile.
    func open(_ request: URLRequest, gen: Int, owner: Owner) -> Bool {
        guard gen == generation, !closed else { return false }
        let socket = transport.open(request)
        self.socket = socket
        lastInbound = clock.now()
        lastProtocolRx = clock.now()
        let hooks = hooks
        tasks.append(
            Task { [weak owner] in
                while !Task.isCancelled {
                    guard let owner else { return }
                    do {
                        let message = try await socket.receive()
                        await hooks.wake(owner, .message(message), gen)
                    } catch {
                        await hooks.wake(owner, .failed, gen)
                        return
                    }
                }
            })
        tasks.append(repeating(every: timing.pingIntervalNs, .ping, gen: gen, owner: owner))
        tasks.append(repeating(every: timing.livenessTickNs, .liveness, gen: gen, owner: owner))
        for (index, interval) in hooks.timers.enumerated() {
            tasks.append(repeating(every: interval, .timer(index), gen: gen, owner: owner))
        }
        return true
    }

    private func repeating(
        every interval: UInt64, _ wake: RoomSocketWake, gen: Int,
        owner: Owner
    ) -> Task<Void, Never> {
        let hooks = hooks
        let clock = clock
        return Task { [weak owner] in
            while !Task.isCancelled {
                await clock.sleep(nanoseconds: interval)
                guard let owner else { return }
                await hooks.wake(owner, wake, gen)
            }
        }
    }

    /// Cancelled with the socket's own tasks when the session ends.
    func adopt(_ task: Task<Void, Never>) {
        tasks.append(task)
    }

    private func cancelTasks() {
        for task in tasks { task.cancel() }
        tasks.removeAll()
    }

    // MARK: Protocol state

    /// Armed BEFORE the hello is sent — an unanswered hello must never hang
    /// the session.
    func armHello() {
        helloSentAt = clock.now()
    }

    /// The hello was answered.
    func helloAnswered() {
        helloSentAt = nil
    }

    func didJoin() {
        joined = true
        backoffMs = timing.backoffBaseMs
    }

    func resetBackoff() {
        backoffMs = timing.backoffBaseMs
    }

    /// Anything arrived, pongs included.
    func noteInbound() {
        lastInbound = clock.now()
    }

    /// A protocol frame arrived: the room is alive and any probe is answered.
    func noteProtocolFrame() {
        lastProtocolRx = clock.now()
        probeSentAt = nil
    }

    // MARK: Failure and redial

    /// The session failed: tell the delegate and redial after backoff.
    /// Ignored for a superseded generation, and for a session already torn
    /// down (`socket == nil`) — that is the cancelled socket's own receive
    /// error.
    nonisolated(nonsending) func fail(gen: Int, owner: Owner) async {
        guard gen == generation, !closed, socket != nil else { return }
        roomLog.warning(
            "\(self.hooks.label, privacy: .public): session ended (joined=\(self.joined)); redialing in \(self.backoffMs)ms"
        )
        joined = false
        await hooks.disconnected(owner)
        scheduleReconnect(gen: gen, owner: owner)
    }

    /// Tears the session down and wakes the owner with `.reconnect` after
    /// the current backoff, which then doubles up to the cap.
    func scheduleReconnect(gen: Int, owner: Owner) {
        guard gen == generation, !closed else { return }
        socket?.cancel(with: .abnormalClosure, reason: nil)
        socket = nil
        cancelTasks()
        let delay = backoffMs
        backoffMs = min(backoffMs * 2, timing.backoffCapMs)
        let hooks = hooks
        let clock = clock
        Task {
            await clock.sleep(nanoseconds: UInt64(delay) * 1_000_000)
            await hooks.wake(owner, .reconnect, gen)
        }
    }

    // MARK: Timers

    /// A socket silent past the lease is dead; otherwise renew it.
    nonisolated(nonsending) func pingTick(gen: Int, owner: Owner) async {
        guard gen == generation, let socket else { return }
        if clock.now() - lastInbound > timing.silenceLeaseNs {
            roomLog.warning("\(self.hooks.label, privacy: .public): socket silent past lease; treating as dead")
            await hooks.wake(owner, .failed, gen)
            return
        }
        try? await socket.send(.string("ping"))
    }

    /// The hello answer and probes run against hard deadlines, and a
    /// long-quiet joined room gets a probe. `extraDeadline` (the chat
    /// backfill) is checked between the two and describes a miss.
    nonisolated(nonsending) func livenessTick(
        gen: Int, owner: Owner,
        extraDeadline: (UInt64) -> String? = { _ in nil }
    ) async {
        guard gen == generation, socket != nil, !closed else { return }
        let now = clock.now()
        var missed: String?
        if let sent = helloSentAt, now - sent > timing.helloDeadlineNs {
            missed = "no state frame within deadline; room presumed wedged, redialing"
        } else if let extra = extraDeadline(now) {
            missed = extra
        } else if let sent = probeSentAt, now - sent > timing.probeDeadlineNs {
            missed = "probe unanswered past deadline; redialing"
        }
        if let missed {
            roomLog.warning("\(self.hooks.label, privacy: .public): \(missed, privacy: .public)")
            await hooks.wake(owner, .failed, gen)
            return
        }
        if joined, probeSentAt == nil, helloSentAt == nil, now - lastProtocolRx > timing.probeQuietNs {
            await sendProbe(owner: owner)
            // Don't re-arm the quiet timer against the same silence.
            lastProtocolRx = clock.now()
        }
    }

    nonisolated(nonsending) func sendProbe(owner: Owner) async {
        // Armed BEFORE the send suspends — the actor is reentrant across the
        // await, and the answer must find the deadline already set.
        probeSentAt = clock.now()
        await hooks.probe(owner)
    }
}

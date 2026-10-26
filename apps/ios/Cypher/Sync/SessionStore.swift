// Session doc mirror — transcript entries + the durable command queue for one
// chat (crates/doc/src/schema.rs), synced over the chat2 log relay
// (docs/design/chat2-sync.md; s2 is dead on mobile). A viewer device never writes
// message entries; it appends command ledger entries (rule 1) and lets the
// host drain them. Optimistic echo: pending sends render locally under their
// client-minted message id until the host writes the real entry with the
// same id.
//
// Every chat syncs through its chat2 room, whatever the registry row's
// `roomGen` says. The local doc is always the chat2 lineage; a cached pre-chat2
// snapshot is never read (unrelated Loro histories would duplicate every
// message).
//
// The doc syncs whole, but `entries` is a window on its tail: a transcript
// opens on its last few messages and reads earlier pages as its reader
// scrolls back (`loadEarlier`), so a long session costs a few messages to
// open and to keep up to date, not its whole history.

import Foundation
import Loro
import Observation

@MainActor
@Observable
final class SessionStore {
    let chatId: String
    /// The chat's host device — nudge target for cold-host command drains.
    var hostDeviceId: String?
    private(set) var entries: [MessageEntry] = []
    @ObservationIgnored private var durableEntries: [MessageEntry] = []
    @ObservationIgnored private var durablePreviewCoverage: PreviewCoverage?
    @ObservationIgnored private var previewProjection = PreviewProjection()
    /// Bumped on every change to `entries` / `pendingSends`. The transcript's
    /// row builder memoizes on it, so a body re-eval that was triggered by
    /// something else (scrolling) costs O(1) instead of re-deriving every row.
    private(set) var revision: UInt64 = 0
    /// Whether this chat's transcript has already been revealed once.
    ///
    /// Lives on the store, not the view: the reveal gate is `@State`, so any
    /// re-creation of TranscriptView reset it to "hidden" and blanked an
    /// already-visible transcript until the settle loop finished. The store is
    /// cached per chat, so it outlives that churn.
    @ObservationIgnored var hasRevealed = false
    /// Transcript parse/row cache — store-owned so parses survive view
    /// churn, and prewarmed off-main whenever a projection lands so opening
    /// the chat never parses markdown inside the first body pass.
    @ObservationIgnored let transcriptCache = TranscriptBuilderCache()
    private(set) var connected = false
    /// Client-minted ids of sends the host hasn't materialized yet.
    private(set) var pendingSends: [PendingSend] = []
    /// Messages precede the loaded window: the transcript can page back.
    private(set) var hasEarlier = false
    /// The rounds above the loaded window, for the turn scrubber: it reaches
    /// every round of a long session without the messages between them.
    private(set) var earlierRounds: [TranscriptRound] = []
    /// The doc's prompts, read once and kept across projections.
    @ObservationIgnored private var outline = RoundOutline()
    /// The prompts above a long session's window are being read, after the
    /// window itself was shown.
    @ObservationIgnored private var outlineReading = false
    /// The whole transcript a store fed through `setEntries` holds, of which
    /// `entries` is the window. Empty for a doc-backed store.
    @ObservationIgnored private(set) var fedEntries: [MessageEntry] = []
    @ObservationIgnored private var fed = false
    /// Where the window starts — an index into the doc's `messages` (or
    /// `fedEntries`); nil until a projection with messages fixes it. Ahead of
    /// `shownStart` while a requested page is on its way.
    @ObservationIgnored private var windowStart: Int?
    /// Where the published `entries` start.
    @ObservationIgnored private var shownStart = 0
    /// Transcripts on screen (`holdWindow`). With none the window follows the
    /// tail, so a store kept warm in the background stays a few messages; with one
    /// it only grows, so nothing drops off the top under a reader.
    @ObservationIgnored private var windowHolds = 0
    /// Questions this device answered whose part the host hasn't resolved
    /// yet: request id → the respondInput command id ("" over a direct
    /// transport). The panel retires on the tap instead of waiting out the
    /// host round trip; it only comes back if the answer is turned down.
    private(set) var answeredInputs: [String: String] = [:]
    /// Why an answered question came back — shown on the reopened panel.
    private(set) var inputAnswerFailure: String?
    /// Host outcomes of respondInput commands (command id → status), read
    /// with each projection.
    @ObservationIgnored private var inputCommandOutcomes: [String: String] = [:]

    let doc = LoroDoc()
    /// The chat2 room cursor — the last server row seq folded into `doc`.
    /// Persisted WITH the snapshot in one atomic file (DocDisk.saveChat2, the
    /// C2 rule), so content and cursor can never diverge.
    @ObservationIgnored private var cursor: UInt64 = 0
    private var chatRoom: ChatRoomClient?
    private var subscriptions: [Subscription] = []
    private let config: AppConfig
    @ObservationIgnored private var started = false

    /// Demo mode: no room, entries driven externally.
    private let offline: Bool
    /// Demo hook: invoked instead of the command plane when offline.
    @ObservationIgnored var demoResponder: ((String, Bool) -> Void)?
    /// For a transcript that isn't a synced doc (a side chat, fed through
    /// `setEntries`): sends, stops and answers go straight to the host
    /// instead of the doc's command ledger. Returns whether it was accepted.
    @ObservationIgnored var directTransport: DirectTransport?

    struct DirectTransport {
        var send: (_ prompt: String, _ messageId: String) -> Bool
        var interrupt: () -> Bool
        var respondInput: (_ requestId: String, _ answers: [UserInputAnswer]) -> Bool
    }

    init(chatId: String, config: AppConfig, offline: Bool = false) {
        self.chatId = chatId
        self.config = config
        self.offline = offline
        AttachmentImageCache.shared.configure(config: config)
    }

    // MARK: Attachments (uploads target the chat's host device)

    @ObservationIgnored private var hostRelay: (deviceId: String, client: DeviceRelayClient)?

    /// Chunked upload of one staged image to the host device; returns the
    /// durable absolute path on that device (what the refs trailer carries).
    func uploadAttachment(name: String, data: Data) async throws -> String {
        guard let hostDeviceId else { throw RelayError.hostOffline }
        let relay: DeviceRelayClient
        if let hostRelay, hostRelay.deviceId == hostDeviceId {
            relay = hostRelay.client
        } else {
            relay = DeviceRelayClient(deviceId: hostDeviceId, config: config)
            hostRelay = (hostDeviceId, relay)
        }
        return try await uploadAttachmentChunked(relay: relay, name: name, data: data)
    }

    // MARK: Lifecycle

    /// Demo-mode injection point (also used by SwiftUI previews and side
    /// chats): the whole transcript, windowed like a doc's.
    func setEntries(_ new: [MessageEntry]) {
        fed = true
        fedEntries = new
        publishFed()
    }

    private func publishFed() {
        let start = Self.windowStart(held: heldStart, count: fedEntries.count)
        windowStart = start
        setShownStart(start)
        let earlier = fedEntries[..<start].enumerated().compactMap { TranscriptRound.prompt($1, at: $0) }
        if earlierRounds != earlier { earlierRounds = earlier }
        let window = start > 0 ? Array(fedEntries[start...]) : fedEntries
        durableEntries = window
        entries = window
        // Echoes whose real entry has landed are done.
        if !pendingSends.isEmpty {
            let ids = Set(window.map(\.id))
            pendingSends.removeAll { ids.contains($0.messageId) }
        }
        settleAnsweredInputs()
        revision &+= 1
        transcriptCache.prewarm(entries: entries)
    }

    // MARK: Window

    /// The messages a transcript opens with: the last few, so the first
    /// paint reads, parses and lays out little more than a screen. A window
    /// shorter than the screen reads the next page at once (TranscriptView).
    nonisolated static let openMessages = 5

    /// How many more messages each `loadEarlier` adds.
    nonisolated static let pageMessages = 40

    /// Where a window over `count` messages starts: the last few, or from
    /// `held` while a reader holds it — never fewer than it opens with, and
    /// back to the last few if the messages it started at are gone (a
    /// rewind).
    nonisolated static func windowStart(held: Int?, count: Int) -> Int {
        let tail = max(0, count - openMessages)
        guard let held, held < count else { return tail }
        return min(held, tail)
    }

    private var heldStart: Int? { windowHolds > 0 ? windowStart : nil }

    private func setShownStart(_ start: Int) {
        shownStart = start
        if hasEarlier != (start > 0) { hasEarlier = start > 0 }
    }

    /// Reads the page above the window; its entries land with the next
    /// projection. A no-op while a page is on its way, with nothing above,
    /// or with no transcript holding the window.
    func loadEarlier() {
        guard windowHolds > 0, let start = windowStart, start == shownStart, start > 0 else { return }
        windowStart = max(0, start - Self.pageMessages)
        readPage()
    }

    /// Reads the window down to message `index`: a jump to one of the
    /// `earlierRounds`.
    func loadEarlier(through index: Int) {
        guard windowHolds > 0, let start = windowStart, index < start else { return }
        windowStart = max(0, index)
        readPage()
    }

    /// Demo rig (`-slow-pages`): how long a fed store's page takes to land,
    /// so it can land under a reader at rest.
    @ObservationIgnored var fedPageDelay: Duration?

    private func readPage() {
        if fed, let fedPageDelay {
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: fedPageDelay)
                self?.publishFed()
            }
        } else {
            refreshWindow()
        }
    }

    /// A transcript went on screen: until it lets go, the window only grows.
    func holdWindow() {
        windowHolds += 1
    }

    /// A transcript went away for good. With none left, the window follows
    /// the tail again and drops the pages its reader loaded.
    func releaseWindow() {
        windowHolds = max(0, windowHolds - 1)
        if windowHolds == 0 { refreshWindow() }
    }

    private func refreshWindow() {
        if fed {
            publishFed()
        } else if started {
            project()
        }
    }

    @ObservationIgnored private var saver: DocSaver?

    func start() {
        guard !started, !offline else { return }
        started = true
        // Local-first: the last-synced chat2 snapshot renders instantly (even
        // when the host device is offline); the join backfills incrementally
        // from its cursor.
        if let saved = DocDisk.loadChat2(into: doc, id: chatId) {
            cursor = saved
            project()
        }
        saver = DocSaver { [weak self] in
            guard let self else { return }
            DocDisk.saveChat2Later(doc: self.doc, id: self.chatId, cursor: self.cursor)
        }
        // Subscription BEFORE any connect: every local commit lands in the
        // client when it exists; commits made earlier are covered by the
        // first-contact full-log push below (cursor 0 whenever no client has
        // ever acked — see connectIfReady).
        let localSub = doc.subscribeLocalUpdate { [weak self] update in
            let bytes = Data(update)
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let room = self.chatRoom {
                    Task { await room.enqueue(update: bytes) }
                }
                self.saver?.poke()
            }
        }
        subscriptions.append(localSub)
        connectIfReady()
        project()
    }

    private func connectIfReady() {
        guard started, !offline, chatRoom == nil else { return }
        let delegate = ChatRoomClient.Delegate(
            cursor: { [weak self] in self?.cursor ?? 0 },
            containsFrontier: { [weak self] frontier in
                // Empty or encoded-empty frontiers are not proof of
                // containment. A present checkpoint may carry either form,
                // and fresh readers must fetch it rather than skip history.
                guard !frontier.isEmpty else { return false }
                guard let self,
                    let vv = try? VersionVector.decode(bytes: frontier)
                else { return false }
                guard !vv.toHashmap().isEmpty else { return false }
                return self.doc.oplogVv().includesVv(other: vv)
            },
            applyCheckpoint: { [weak self] bytes, seq in
                guard let self,
                    (try? self.doc.importWith(bytes: bytes, origin: "remote")) != nil
                else {
                    return false
                }
                self.cursor = max(self.cursor, seq)
                self.project()
                self.saver?.poke()
                return true
            },
            applyRow: { [weak self] bytes, seq in
                guard let self else { return }
                // Malformed remote bytes cost the row, never the doc. The
                // cursor still advances: replaying a poison row forever is
                // the wedge class chat2 replaces.
                if (try? self.doc.importWith(bytes: bytes, origin: "remote")) == nil {
                    roomLog.warning("chat2 \(self.chatId, privacy: .public): row import failed; skipping row \(seq)")
                }
                self.cursor = max(self.cursor, seq)
                self.project()
                self.saver?.poke()
            },
            advanceCursor: { [weak self] seq in
                guard let self else { return }
                self.cursor = max(self.cursor, seq)
                self.saver?.poke()
            },
            clampCursor: { [weak self] seq in
                guard let self, self.cursor != seq else { return }
                self.cursor = seq
                self.saver?.poke()
            },
            setCursor: { [weak self] seq in
                guard let self, self.cursor != seq else { return }
                self.cursor = seq
                self.saver?.poke()
            },
            event: { [weak self] event in self?.handle(event) },
            preview: { [weak self] data in
                guard let self else { return [] }
                let replies: [Data]
                if let data {
                    replies = self.previewProjection.receive(data, chatId: self.chatId)
                } else {
                    self.previewProjection.disconnect()
                    replies = []
                }
                self.entries = self.previewProjection.overlay(
                    self.durableEntries, coverage: self.durablePreviewCoverage)
                self.revision &+= 1
                return replies  // No saver, doc import, command, or status mutation.
            },
            previewRetry: { [weak self] in
                guard let self else { return nil }
                if self.previewProjection.expire() {
                    self.entries = self.previewProjection.overlay(
                        self.durableEntries, coverage: self.durablePreviewCoverage)
                    self.revision &+= 1
                }
                return self.previewProjection.retry(chatId: self.chatId, cursor: self.cursor)
            }
        )
        let client = ChatRoomClient(
            chatId: chatId, device: config.deviceId,
            urlProvider: { [config, chatId] in await config.chat2SocketURL(chatId: chatId) },
            checkpointRequest: { [config, chatId] in
                await config.chat2CheckpointRequest(chatId: chatId)
            },
            rowsRequest: { [config, chatId] after in
                await config.chat2RowsRequest(chatId: chatId, after: after)
            },
            pushRequest: { [config, chatId] batchId in
                await config.chat2PushRequest(chatId: chatId, batchId: batchId)
            },
            delegate: delegate, previewEnabled: config.streamPreviewEnabled)
        chatRoom = client
        // First contact with the room (cursor 0): everything committed
        // BEFORE the local-update subscription saw a client — an adopt's
        // requeued commands, sends queued while offline — is invisible to
        // the push path, yet every later commit
        // causally depends on it (doc_host.rs first-contact rule; rows built
        // on unpushed deps sit in peers' pending-dep buffers forever). Push
        // the doc's full update log as the join's first batch; once acked
        // the cursor moves and this never re-arms.
        if cursor == 0,
            let all = try? doc.export(mode: .updates(from: VersionVector())), !all.isEmpty
        {
            Task { await client.enqueue(update: all) }
        }
        Task { await client.start() }
    }

    /// Backgrounding hook: persist immediately.
    func flushToDisk() {
        saver?.flush()
        DocDisk.finishWrites()
    }

    /// Foreground hook: revive the room after a suspension (see
    /// ChatRoomClient.kick), or dial it if it never started.
    func kickRoom() {
        connectIfReady()
        guard let chatRoom else { return }
        Task { await chatRoom.kick() }
    }

    /// Whether local commits are still waiting on the room (unacked pushes or
    /// unmaterialized sends) — stopping now would strand them, since the push
    /// queue doesn't survive the client.
    func hasUnpushedUpdates() async -> Bool {
        if !pendingSends.isEmpty { return true }
        guard let chatRoom else { return false }
        return await chatRoom.hasPendingPushes
    }

    func stop() {
        subscriptions.removeAll()
        saver?.flush()
        if let chatRoom {
            Task { await chatRoom.stop() }
        }
        chatRoom = nil
        connected = false
        previewProjection.disconnect()
        entries = previewProjection.overlay(durableEntries, coverage: durablePreviewCoverage)
    }

    private func handle(_ event: ChatRoomEvent) {
        switch event {
        case .connected:
            connected = true
            project()
        case .disconnected:
            connected = false
        }
    }

    // MARK: Projection

    /// In-flight guard + trailing re-run for the off-main projection below.
    @ObservationIgnored private var projecting = false
    @ObservationIgnored private var projectPending = false

    /// Re-derive `entries` from the window on the doc, off the main thread.
    ///
    /// The read and the decode are O(window), not O(transcript): messages are
    /// read one by one from the window's start, never through the whole doc's
    /// `getDeepValue()`, and the window's settled markdown is parsed in the
    /// same background task, so the rows assemble from memo hits. This runs on
    /// every remote update. Reading the doc from a background task is the
    /// access class the design already has: `ChatRoomClient` applies through
    /// main-actor closures, but the doc remains concurrently readable
    /// regardless; a read torn by an import is redone by the trailing re-run
    /// that import queues.
    ///
    /// Overlapping calls coalesce to a single trailing re-run — a streaming
    /// burst must not queue one projection per token.
    private func project() {
        guard !projecting else {
            projectPending = true
            return
        }
        projecting = true
        let doc = self.doc
        let held = heldStart
        let memo = transcriptCache.settledParses
        let outline = outlineReading ? nil : self.outline
        Task { @MainActor [weak self] in
            let read = await Task.detached(priority: .userInitiated) {
                let read = Self.readWindow(of: doc, held: held)
                // Up to a page of prompts inline; a long history's outline
                // must not hold up its first page (`readOutline`).
                let rounds = outline.flatMap {
                    Self.extendOutline($0, of: doc, to: read.start, steers: read.steers, limit: Self.pageMessages)
                }
                return (read, rounds, TranscriptBuilderCache.parseSettled(read.entries, memo: memo))
            }.value
            guard let self else { return }
            self.projecting = false
            self.transcriptCache.adopt(read.2)
            self.inputCommandOutcomes = read.0.inputOutcomes
            if let rounds = read.1 {
                self.outline = rounds
                if self.earlierRounds != rounds.rounds { self.earlierRounds = rounds.rounds }
            } else if !self.outlineReading {
                self.readOutline(to: read.0.start, steers: read.0.steers)
            }
            // A page requested meanwhile stays requested (the re-run below
            // reads it); without a reader the window is wherever the tail is.
            if self.windowHolds > 0, let start = self.windowStart {
                self.windowStart = min(start, read.0.start)
            } else {
                self.windowStart = read.0.start
            }
            self.setShownStart(read.0.start)
            self.apply(read.0.entries, coverage: read.0.coverage)
            if self.projectPending {
                self.projectPending = false
                self.project()
            }
        }
    }

    /// One projection's read of the doc.
    struct WindowRead {
        /// The window's entries, continuations joined.
        var entries: [MessageEntry]
        /// The index in `messages` the window starts at.
        var start: Int
        var coverage: PreviewCoverage?
        var inputOutcomes: [String: String]
        /// The messages steer commands sent.
        var steers: Set<String>
    }

    /// The prompts among the doc's first `upTo` messages. A light read — a
    /// message's role, and a prompt's own fields — made once for the messages
    /// above the first window, then trimmed as the window grows over them.
    struct RoundOutline {
        var rounds: [TranscriptRound] = []
        var upTo = 0
    }

    /// Reads the prompts above the window off the main thread after the
    /// window itself is shown, then projects again to bring them up to where
    /// the window is by then. Outlining a long history takes longer than
    /// reading its last page (3,000 messages: ~100ms on the simulator).
    private func readOutline(to start: Int, steers: Set<String>) {
        outlineReading = true
        let doc = self.doc
        let outline = self.outline
        Task { @MainActor [weak self] in
            let read = await Task.detached(priority: .utility) {
                Self.extendOutline(outline, of: doc, to: start, steers: steers)
            }.value
            guard let self else { return }
            self.outlineReading = false
            if let read { self.outline = read }
            self.project()
        }
    }

    /// `outline` brought up to message `start` (trimmed when the window grew
    /// over it), or nil if that means reading more than `limit` messages.
    nonisolated static func extendOutline(
        _ outline: RoundOutline, of doc: LoroDoc, to start: Int, steers: Set<String>, limit: Int = .max
    ) -> RoundOutline? {
        let messages = doc.getList(id: "messages")
        var outline = outline
        if Int(messages.len()) < outline.upTo { outline = RoundOutline() }  // rewound into it
        if start < outline.upTo {
            outline.rounds.removeAll { ($0.messageIndex ?? 0) >= start }
        } else {
            guard start - outline.upTo <= limit else { return nil }
            outline.rounds += (outline.upTo..<start).compactMap { prompt(at: $0, in: messages, steers: steers) }
        }
        outline.upTo = start
        return outline
    }

    /// Decodes the window from `held` (see `windowStart(held:count:)`) to the
    /// end of the doc's `messages`.
    nonisolated static func readWindow(of doc: LoroDoc, held: Int?) -> WindowRead {
        let messages = doc.getList(id: "messages")
        let count = Int(messages.len())
        var start = windowStart(held: held, count: count)
        var raw = (start..<count).compactMap { message(at: $0, in: messages) }
        // A continuation joins its root, which an older doc can hold above
        // the page: reach back to it (current hosts write no continuations).
        var missing = Set(raw.compactMap(\.continuationOf)).subtracting(raw.map(\.id))
        var above: [MessageEntry] = []
        while !missing.isEmpty, start > 0 {
            start -= 1
            guard let entry = message(at: start, in: messages) else { continue }
            missing.remove(entry.id)
            above.append(entry)
        }
        raw.insert(contentsOf: above.reversed(), at: 0)
        let commands = doc.getList(id: "commands").getDeepValue().listValue ?? []
        let steers = steerMessageIDs(commands: commands)
        let marker = doc.getMap(id: "meta").getDeepValue().mapValue?["previewCoverage"]?.stringValue
        return WindowRead(
            entries: decodeEntries(raw, steers: steers), start: start,
            coverage: marker.flatMap { try? JSONDecoder().decode(PreviewCoverage.self, from: Data($0.utf8)) },
            inputOutcomes: inputCommandOutcomes(commands: commands), steers: steers)
    }

    nonisolated private static func message(at index: Int, in messages: LoroList) -> MessageEntry? {
        guard let item = messages.get(index: UInt32(index)) else { return nil }
        return (item.asLoroMap()?.getDeepValue() ?? item.asValue()).flatMap(entryFrom)
    }

    /// The round message `index` starts, reading a reply no further than its
    /// role.
    nonisolated private static func prompt(
        at index: Int, in messages: LoroList, steers: Set<String>
    ) -> TranscriptRound? {
        guard let map = messages.get(index: UInt32(index))?.asLoroMap(),
            map.get(key: "role")?.asValue()?.stringValue == MessageRole.user.rawValue,
            var entry = entryFrom(map.getDeepValue())
        else { return nil }
        entry.isSteer = steers.contains(entry.id)
        return TranscriptRound.prompt(entry, at: index)
    }

    private func apply(_ decoded: [MessageEntry], coverage: PreviewCoverage? = nil) {
        durableEntries = decoded
        durablePreviewCoverage = coverage
        entries = previewProjection.overlay(decoded, coverage: coverage)
        // Drop echoes the host has materialized.
        let ids = Set(decoded.map(\.id))
        pendingSends.removeAll { ids.contains($0.messageId) }
        settleAnsweredInputs()
        revision &+= 1
        // If no transcript view is open, settle the parses now (off-main) so
        // the eventual open is memo hits all the way down.
        transcriptCache.prewarm(entries: entries)
    }

    /// Whole-doc decode. `nil` means the doc has no map root yet — leave the
    /// previous projection standing rather than blanking a live transcript.
    nonisolated static func decodeEntries(from doc: LoroDoc) -> [MessageEntry]? {
        guard let root = doc.getDeepValue().mapValue else { return nil }
        return decodeEntries(root: root)
    }

    nonisolated static func decodeEntries(root: [String: LoroValue]) -> [MessageEntry] {
        decodeEntries(
            (root["messages"]?.listValue ?? []).compactMap(entryFrom),
            commands: root["commands"]?.listValue ?? [])
    }

    /// Raw (unjoined) message entries as the transcript shows them.
    nonisolated static func decodeEntries(_ raw: [MessageEntry], commands: [LoroValue]) -> [MessageEntry] {
        decodeEntries(raw, steers: steerMessageIDs(commands: commands))
    }

    nonisolated private static func decodeEntries(_ raw: [MessageEntry], steers: Set<String>) -> [MessageEntry] {
        let marked = raw.map { entry in
            var entry = entry
            entry.isSteer = entry.role == .user && steers.contains(entry.id)
            return entry
        }
        return joinContinuations(marked)
    }

    /// The messages steer commands sent. Commands are append-only and sync
    /// with the transcript. Join explicit steer message IDs instead of
    /// guessing from timing/status, so the optimistic echo and a
    /// reopened/cross-device transcript agree. "applied" only means
    /// routed/queued, not consumed: don't expose it as a delivery receipt.
    /// Old messages without a matching ID stay normal.
    nonisolated static func steerMessageIDs(commands: [LoroValue]) -> Set<String> {
        Set(
            commands.compactMap { command -> String? in
                guard let command = command.mapValue,
                    command["kind"]?.stringValue == "steer",
                    let payload = command["payload"]?.mapValue,
                    payload["kind"]?.stringValue == "steer",
                    let id = payload["messageId"]?.stringValue, !id.isEmpty
                else { return nil }
                return id
            })
    }

    /// Settled respondInput commands: command id → host status.
    nonisolated static func inputCommandOutcomes(root: [String: LoroValue]) -> [String: String] {
        inputCommandOutcomes(commands: root["commands"]?.listValue ?? [])
    }

    nonisolated static func inputCommandOutcomes(commands: [LoroValue]) -> [String: String] {
        var outcomes: [String: String] = [:]
        for command in commands {
            guard let command = command.mapValue,
                command["kind"]?.stringValue == "respondInput",
                let id = command["id"]?.stringValue,
                let status = command["status"]?.stringValue, status != "pending"
            else { continue }
            outcomes[id] = status
        }
        return outcomes
    }

    nonisolated static func entryFrom(_ value: LoroValue) -> MessageEntry? {
        guard let m = value.mapValue,
            let id = m["id"]?.stringValue,
            let roleStr = m["role"]?.stringValue,
            let role = MessageRole(rawValue: roleStr)
        else { return nil }
        let parts = (m["parts"]?.listValue ?? []).compactMap(partFrom)
        return MessageEntry(
            id: id, role: role, parts: parts,
            createdAt: m["createdAt"]?.i64Value ?? 0,
            deviceId: m["deviceId"]?.stringValue ?? "",
            status: m["status"]?.stringValue.flatMap(MessageStatus.init(rawValue:)),
            continuationOf: m["continuationOf"]?.stringValue,
            models: (m["models"]?.listValue ?? []).compactMap { value -> AnsweredModel? in
                guard let model = value.mapValue?["model"]?.stringValue, !model.isEmpty else { return nil }
                return AnsweredModel(model: model, requested: value.mapValue?["requested"]?.stringValue)
            },
            // A malformed record costs only itself (salvage.rs
            // `recovery_from_json`): the prompt it rides still renders.
            recovery: m["recovery"].flatMap { record in
                guard let data = try? JSONSerialization.data(withJSONObject: record.jsonObject) else { return nil }
                return try? JSONDecoder().decode(ContextRecovery.self, from: data)
            })
    }

    nonisolated private static func partFrom(_ value: LoroValue) -> MessagePart? {
        guard let m = value.mapValue,
            let id = m["id"]?.stringValue,
            let kind = m["kind"]?.stringValue
        else { return nil }
        switch kind {
        case "text":
            return .text(
                id: id, text: m["text"]?.stringValue ?? "",
                agentText: m["agentText"]?.stringValue)
        case "reasoning":
            return .reasoning(id: id, text: m["reasoning"]?.stringValue ?? "")
        case "tool":
            guard let callMap = m["call"]?.mapValue else { return nil }
            let tag = callMap["kind"]?.stringValue ?? "unknown"
            var fields: [String: AnyHashable] = [:]
            for (k, v) in callMap where k != "kind" {
                if let s = v.stringValue {
                    fields[k] = s
                } else if let b = v.boolValue {
                    fields[k] = b
                } else if let i = v.i64Value {
                    fields[k] = i
                } else if let list = v.listValue {
                    // ApplyPatch changes / Todo items — keep a JSON echo.
                    fields[k] = list.map { "\($0.jsonObject)" }
                }
            }
            // Pi's codemode script and tool_search query: the one input field
            // the doc keeps for each (parts.rs sanitize_tool_call).
            if tag == "unknown", let name = callMap["name"]?.stringValue,
                let key = RenderToolCall.keptInputField(name),
                let value = callMap["input"]?.mapValue?[key]?.stringValue
            {
                fields[key] = value
            }
            // isError presence IS the resolution marker (schema.rs:96).
            let isError = m["isError"]?.boolValue
            var call = RenderToolCall(tag: tag, fields: fields)
            if tag == "unknown", callMap["name"]?.stringValue == "subagent",
                let input = callMap["input"]?.mapValue,
                let agent = input["agent"]?.stringValue, !agent.isEmpty
            {
                call.subagent = SubagentCallMetadata(
                    agent: String(agent.prefix(120)),
                    task: String((input["task"]?.stringValue ?? "").prefix(500)),
                    isAsync: input["async"]?.boolValue ?? false)
                call.progress = SubagentProjection.boundedProgress(m["progress"]?.stringValue)
            }
            if let record = m["permission"],
                let data = try? JSONSerialization.data(withJSONObject: record.jsonObject)
            {
                call.permission = try? JSONDecoder().decode(ToolPermission.self, from: data)
            }
            return .tool(
                id: id, call: call,
                isError: isError ?? false, resolved: isError != nil)
        case "input":
            var questions: [UserInputQuestion] = []
            if let list = m["questions"]?.listValue,
                let data = try? JSONSerialization.data(withJSONObject: list.map(\.jsonObject)),
                let decoded = try? JSONDecoder().decode([UserInputQuestion].self, from: data)
            {
                questions = decoded
            }
            return .input(
                id: id, requestId: id, questions: questions,
                resolved: m["resolved"]?.boolValue ?? false)
        case "error":
            return .error(id: id, message: m["message"]?.stringValue ?? "")
        default:
            return nil
        }
    }

    /// schema.rs join_continuation_entries: concatenate continuation parts onto
    /// the root in list order; orphans surface standalone.
    nonisolated static func joinContinuations(_ raw: [MessageEntry]) -> [MessageEntry] {
        var roots: [MessageEntry] = []
        var index: [String: Int] = [:]
        for entry in raw {
            if let rootId = entry.continuationOf, let ix = index[rootId] {
                roots[ix].parts.append(contentsOf: entry.parts)
                for model in entry.models where !roots[ix].models.contains(model) {
                    roots[ix].models.append(model)
                }
            } else {
                index[entry.id] = roots.count
                roots.append(entry)
            }
        }
        return roots
    }

    /// The doc as an `@session` reference reads it (Mentions.swift), decoded
    /// off the main thread like `project()`.
    func referenceEntries() async -> [ReferenceEntry] {
        let doc = self.doc
        return await Task.detached(priority: .userInitiated) {
            guard let root = doc.getDeepValue().mapValue else { return [] }
            return SessionReferences.entries(root: root)
        }.value
    }

    // MARK: Derived

    var lastEntryId: String? { durableEntries.last?.id }

    /// The unresolved input request to surface in the question panel. One
    /// this device already answered is skipped: the answer is on its way.
    var openInputRequest: (entryId: String, requestId: String, questions: [UserInputQuestion])? {
        for entry in durableEntries.reversed() {
            for part in entry.parts.reversed() {
                // An empty question list can't be answered, so it must not take
                // the composer's place — leaving the user with no way to type.
                if case .input(_, let requestId, let questions, let resolved) = part,
                    !resolved, !questions.isEmpty, answeredInputs[requestId] == nil
                {
                    return (entry.id, requestId, questions)
                }
            }
        }
        return nil
    }

    /// Unresolved input request ids in the durable transcript.
    private var unresolvedInputIds: Set<String> {
        var ids = Set<String>()
        for entry in durableEntries {
            for part in entry.parts {
                if case .input(_, let requestId, _, false) = part { ids.insert(requestId) }
            }
        }
        return ids
    }

    /// Forget answers whose question resolved, and reopen any the host
    /// rejected or let expire — the question is still open, and silently
    /// keeping it hidden would leave the run waiting on nobody.
    private func settleAnsweredInputs() {
        guard !answeredInputs.isEmpty else { return }
        let open = unresolvedInputIds
        for (requestId, commandId) in answeredInputs {
            if !open.contains(requestId) {
                answeredInputs[requestId] = nil
            } else if let status = inputCommandOutcomes[commandId],
                status == "rejected" || status == "expired"
            {
                reopenInput(requestId, reason: "Your answer didn't reach the agent. Please answer again.")
            }
        }
    }

    /// Bring an answered question's panel back, e.g. when its answer failed
    /// to leave this device.
    func reopenInput(_ requestId: String, reason: String) {
        guard answeredInputs.removeValue(forKey: requestId) != nil else { return }
        inputAnswerFailure = reason
    }

    // MARK: Command plane (ledger rule 1: append-only, own entries only)

    @discardableResult
    func sendRun(prompt: String, chat: Chat, attachments: [String] = [], agentPrompt: String? = nil) -> Bool {
        guard !CommentPrompt.blocksSlash(prompt, hasComments: agentPrompt != nil) else { return false }
        if let directTransport { return sendDirect(prompt, isSteer: false, via: directTransport) }
        if offline {
            demoResponder?(prompt, false)
            return true
        }
        let messageId = UUID().uuidString.lowercased()
        let request = RunRequest(
            prompt: prompt,
            harness: chat.config?.harness,
            model: chat.config?.model,
            reasoning: chat.config?.reasoning,
            modelOptions: chat.config?.modelOptions ?? [:],
            cwd: chat.cwd ?? "",
            sandbox: chat.config?.sandbox ?? "workspace-write",
            attachments: attachments)
        var payload: [String: Any] = [
            "kind": "run",
            "request": encodableJSON(request),
            "messageId": messageId,
        ]
        if let agentPrompt { payload["agentPrompt"] = agentPrompt }
        guard queueCommand(kind: "run", payload: payload) != nil else { return false }
        pendingSends.append(PendingSend(messageId: messageId, text: prompt, at: nowMs()))
        revision &+= 1
        return true
    }

    @discardableResult
    func sendSteer(prompt: String, agentPrompt: String? = nil) -> Bool {
        guard !CommentPrompt.blocksSlash(prompt, hasComments: agentPrompt != nil) else { return false }
        if let directTransport { return sendDirect(prompt, isSteer: true, via: directTransport) }
        if offline {
            demoResponder?(prompt, true)
            return true
        }
        let messageId = UUID().uuidString.lowercased()
        var payload: [String: Any] = [
            "kind": "steer",
            "prompt": prompt,
            "messageId": messageId,
        ]
        if let agentPrompt { payload["agentPrompt"] = agentPrompt }
        guard queueCommand(kind: "steer", payload: payload) != nil else { return false }
        pendingSends.append(PendingSend(messageId: messageId, text: prompt, at: nowMs(), isSteer: true))
        revision &+= 1
        return true
    }

    @discardableResult
    func sendInterrupt() -> Bool {
        if let directTransport { return directTransport.interrupt() }
        return queueCommand(kind: "interrupt", payload: ["kind": "interrupt"]) != nil
    }

    /// Answer a question; on success its panel retires immediately.
    @discardableResult
    func respondInput(requestId: String, answers: [UserInputAnswer]) -> Bool {
        // A second tap racing the retire must not queue a duplicate answer.
        guard answeredInputs[requestId] == nil else { return true }
        let commandId: String?
        if let directTransport {
            commandId = directTransport.respondInput(requestId, answers) ? "" : nil
        } else {
            commandId = queueCommand(
                kind: "respondInput",
                payload: [
                    "kind": "respondInput",
                    "requestId": requestId,
                    "answers": answers.map(encodableJSON),
                ])
        }
        guard let commandId else { return false }
        answeredInputs[requestId] = commandId
        inputAnswerFailure = nil
        return true
    }

    /// A direct send, echoed optimistically like a queued one (the host's
    /// entry carries the same message id and replaces the echo).
    private func sendDirect(_ prompt: String, isSteer: Bool, via transport: DirectTransport) -> Bool {
        let messageId = UUID().uuidString.lowercased()
        guard transport.send(prompt, messageId) else { return false }
        pendingSends.append(PendingSend(messageId: messageId, text: prompt, at: nowMs(), isSteer: isSteer))
        revision &+= 1
        return true
    }

    /// schema.rs queue_command, field for field. Returns the command id.
    private func queueCommand(kind: String, payload: [String: Any]) -> String? {
        let commands = doc.getList(id: "commands")
        let id = UUID().uuidString.lowercased()
        do {
            let map = try commands.pushContainer(child: LoroMap())
            try map.insert(key: "id", v: id)
            try map.insert(key: "kind", v: kind)
            try map.insert(key: "payload", v: LoroValue.fromJSON(payload))
            try map.insert(key: "issuedBy", v: config.deviceId)
            try map.insert(key: "issuedAt", v: nowMs())
            if let turnId = lastEntryId {
                try map.insert(
                    key: "basedOn",
                    v: LoroValue.map(value: [
                        "turnId": .string(value: turnId),
                        "frontier": .null,
                    ]))
            }
            try map.insert(key: "expiresAt", v: nowMs() + commandDefaultTtlMs)
            try map.insert(key: "status", v: "pending")
            doc.commit()
        } catch { return nil }
        nudgeHost()
        return id
    }

    /// Durable-nudge the host device so a cold host opens the doc and drains
    /// (doc_host.rs nudge_remote_host). Fire-and-forget; the command is
    /// durable in the doc regardless.
    private func nudgeHost() {
        guard let hostDeviceId else { return }
        Task { [config, chatId] in
            await config.nudge(deviceId: hostDeviceId, chatId: chatId)
        }
    }
}

private func encodableJSON<T: Encodable>(_ value: T) -> Any {
    guard let data = try? JSONEncoder().encode(value),
        let obj = try? JSONSerialization.jsonObject(with: data)
    else { return [:] }
    return obj
}

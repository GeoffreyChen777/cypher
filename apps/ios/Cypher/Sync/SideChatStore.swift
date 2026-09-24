// Temporary Side Chat (desktop side_chats.rs, round 21): a throwaway chat
// about a selection, hosted by the parent chat's device in memory only — no
// registry row, no synced room — until it's opened as a normal chat.
//
// Everything goes through the host's relay: StartSideChat; the transcript
// and status as streams (WatchDocMessages, WatchSideChatStatus); sends,
// stops and answers as unary calls; PromoteSideChat; DisposeSideChat. The
// transcript feeds an offline SessionStore, so the regular transcript view
// and composer render it unchanged. The host reaps a side chat nobody has
// watched for 5 minutes — a phone that was away that long finds it gone.

import Foundation
import Observation

@MainActor @Observable
final class SideChatStore {
    enum Phase: Equatable {
        case starting
        case open
        /// Couldn't start, or the host let it go (reaped, restarted).
        case ended(String)
    }

    let parent: Chat
    let quote: String
    private(set) var phase: Phase = .starting
    private(set) var status: SessionStatus = .idle
    /// A send/stop the host refused.
    var error: String?
    /// The side chat's id once the host has started it.
    private(set) var sideChatId: String?
    /// The offline store the transcript view and composer bind to.
    private(set) var session: SessionStore

    @ObservationIgnored private let relay: DeviceRelayClient?
    @ObservationIgnored private let config: AppConfig
    @ObservationIgnored private let anchorEntryId: String?
    @ObservationIgnored private var feed = TranscriptFeed()
    @ObservationIgnored private var watches: [Task<Void, Never>] = []
    @ObservationIgnored private var disposed = false

    /// Live: talk to `relay` (the parent's host). Demo: `demo` supplies an
    /// offline store with a scripted responder.
    init(parent: Chat, quote: String, anchorEntryId: String?, relay: DeviceRelayClient?,
         config: AppConfig, demo: DemoDataset? = nil) {
        self.parent = parent
        self.quote = quote
        self.anchorEntryId = anchorEntryId
        self.relay = relay
        self.config = config
        if let demo {
            let id = "side-\(UUID().uuidString.lowercased().prefix(8))"
            sideChatId = id
            session = demo.sessionStore(for: id)
            phase = .open
        } else {
            session = SessionStore(chatId: "side-pending", config: config, offline: true)
        }
    }

    /// The synthetic row the composer drives: the parent's checkout and
    /// config under the side chat's id.
    var chat: Chat {
        var chat = parent
        chat.id = sideChatId ?? "side-pending"
        chat.title = "Side chat"
        chat.child = nil
        chat.lastMessageAt = nil
        return chat
    }

    func start() async {
        guard let relay, sideChatId == nil, !disposed else { return }
        struct Started: Decodable { var sideChatId: String }
        var source: [String: Any] = ["kind": "transcript"]
        if let anchorEntryId { source["anchorMessageId"] = anchorEntryId }
        do {
            let started: Started = try await relay.call(method: "StartSideChat", params: [
                "parentChatId": parent.id,
                "source": source,
                "selectedText": String(quote.prefix(64_000)),
            ], timeoutSeconds: 15)
            // Closed while starting: don't leave it running on the host.
            guard !disposed else {
                dispose(started.sideChatId)
                return
            }
            sideChatId = started.sideChatId
            session = SessionStore(chatId: started.sideChatId, config: config, offline: true)
            session.hostDeviceId = parent.deviceId
            session.directTransport = transport(id: started.sideChatId, relay: relay)
            phase = .open
            watch(id: started.sideChatId, relay: relay)
        } catch {
            phase = .ended(Self.startFailure(error))
        }
    }

    /// Open as a normal chat (same id); the side chat then syncs like any
    /// session. Returns the chat id.
    func promote(demo: DemoDataset?) async throws -> String {
        guard let sideChatId else { throw RelayError.notConnected }
        if let demo {
            var chat = self.chat
            chat.title = Self.promotedTitle(quote)
            chat.createdAt = nowMs()
            chat.lastMessageAt = nowMs()
            demo.chats.append(chat)
            disposed = true
            return sideChatId
        }
        guard let relay else { throw RelayError.notConnected }
        struct Promoted: Decodable { var chatId: String }
        let promoted: Promoted = try await relay.call(method: "PromoteSideChat",
                                                      params: ["sideChatId": sideChatId], timeoutSeconds: 15)
        disposed = true  // it's a real chat now: nothing to dispose
        stopWatching()
        return promoted.chatId
    }

    /// Close: the host forgets it (fire-and-forget; a no-op after promote).
    func close() {
        guard !disposed else { return }
        disposed = true
        stopWatching()
        if let sideChatId { dispose(sideChatId) }
    }

    /// side_chats.rs promoted title: the quote's first five words, ≤ 48.
    static func promotedTitle(_ quote: String) -> String {
        let words = quote.split(whereSeparator: \.isWhitespace).prefix(5).joined(separator: " ")
        return words.isEmpty ? "Side chat" : String(words.prefix(48))
    }

    // MARK: Host calls

    private func dispose(_ id: String) {
        guard let relay else { return }
        Task { let _: IgnoredReply? = try? await relay.call(method: "DisposeSideChat", params: ["sideChatId": id]) }
    }

    private func transport(id: String, relay: DeviceRelayClient) -> SessionStore.DirectTransport {
        let parent = parent
        return SessionStore.DirectTransport(
            send: { [weak self] prompt, messageId in
                // The side chat has no row, so the request must name the
                // harness and model itself (else the host's default harness).
                let request = RunRequest(prompt: prompt, harness: parent.config?.harness,
                                         model: parent.config?.model,
                                         reasoning: parent.config?.reasoning,
                                         modelOptions: parent.config?.modelOptions ?? [:],
                                         cwd: parent.cwd ?? "",
                                         sandbox: parent.config?.sandbox ?? "workspace-write")
                self?.call("SendSideChat", ["sideChatId": id, "request": Self.json(request),
                                            "messageId": messageId], failure: "Couldn't send")
                return true
            },
            interrupt: { [weak self] in
                self?.call("InterruptSideChat", ["sideChatId": id], failure: "Couldn't stop")
                return true
            },
            respondInput: { [weak self] requestId, answers in
                self?.call("RespondSideChatInput", ["sideChatId": id, "requestId": requestId,
                                                    "answers": answers.map(Self.json)],
                           failure: "Couldn't send the answer")
                return true
            })
    }

    private func call(_ method: String, _ params: [String: Any], failure: String) {
        guard let relay else { return }
        Task { @MainActor [weak self] in
            do {
                let _: IgnoredReply = try await relay.call(method: method, params: params, timeoutSeconds: 20)
                self?.error = nil
            } catch {
                self?.error = "\(failure) — \(error.localizedDescription)"
            }
        }
    }

    // MARK: Streams

    /// Resubscribes only when the side chat provably still exists: the
    /// host's `WatchDocMessages` opens (and would mint) a doc for ANY id, so
    /// blindly re-watching a reaped side chat would leave an empty chat doc
    /// on the host. `WatchSideChatStatus` rejects unknown ids — the probe.
    private func watch(id: String, relay: DeviceRelayClient) {
        watches.append(Task { @MainActor [weak self] in
            var desyncs = 0
            while !Task.isCancelled {
                do {
                    for try await item in relay.subscribe(method: "WatchDocMessages", params: ["chatId": id]) {
                        guard let self, !Task.isCancelled else { return }
                        guard let frame = try JSONSerialization.jsonObject(with: item) as? [String: Any] else { continue }
                        try self.feed.apply(frame)
                        self.session.setEntries(self.feed.messages)
                        desyncs = 0
                    }
                    break  // the host ended it: disposed or reaped
                } catch is TranscriptFeed.Desync {
                    // The stream was alive a moment ago; a fresh subscribe
                    // starts with a reset.
                    desyncs += 1
                    if desyncs > 3 { break }
                } catch {
                    try? await Task.sleep(nanoseconds: 1_000_000_000)
                    guard !Task.isCancelled, await Self.alive(id: id, relay: relay) else { break }
                }
            }
            guard let self, !Task.isCancelled, !self.disposed else { return }
            self.phase = .ended("This side chat has ended — the connection to its device dropped, or it sat unwatched too long.")
        })
        watches.append(Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do {
                    for try await item in relay.subscribe(method: "WatchSideChatStatus", params: ["sideChatId": id]) {
                        guard let self else { return }
                        let object = try? JSONSerialization.jsonObject(with: item, options: .fragmentsAllowed)
                        let status = (object as? [String: Any])?["status"] as? String
                        self.status = status.flatMap(SessionStatus.init(rawValue:)) ?? .idle
                    }
                    return
                } catch {
                    try? await Task.sleep(nanoseconds: 1_000_000_000)
                    guard !Task.isCancelled, await Self.alive(id: id, relay: relay) else { return }
                }
            }
        })
    }

    /// Whether the host still tracks this side chat: its status watch
    /// answers (first item) instead of refusing the id. Bounded at 8s.
    private static func alive(id: String, relay: DeviceRelayClient) async -> Bool {
        await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                do {
                    for try await _ in relay.subscribe(method: "WatchSideChatStatus", params: ["sideChatId": id]) {
                        return true
                    }
                } catch {}
                return false
            }
            group.addTask {
                try? await Task.sleep(nanoseconds: 8_000_000_000)
                return false
            }
            let first = await group.next() ?? false
            group.cancelAll()
            return first
        }
    }

    private func stopWatching() {
        watches.forEach { $0.cancel() }
        watches.removeAll()
    }

    // MARK: Helpers

    private static func json<T: Encodable>(_ value: T) -> Any {
        guard let data = try? JSONEncoder().encode(value),
              let object = try? JSONSerialization.jsonObject(with: data) else { return [:] }
        return object
    }

    private static func startFailure(_ error: Error) -> String {
        if case RelayError.rpc(let text) = error, text.lowercased().contains("unknown method") {
            return "The session's device runs an older Cypher — update it to use side chats."
        }
        switch error as? RelayError {
        case .notConnected, .hostOffline, .timeout:
            return "The session's device is unreachable."
        default:
            return "Couldn't start a side chat — \(error.localizedDescription)"
        }
    }
}

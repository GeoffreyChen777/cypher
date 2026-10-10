// In-memory WebSocket transport and manual clock for the room client
// lifecycle tests (ChatRoomClient, RegistryClient).

import Foundation
import XCTest
@testable import Cypher

/// A clock that only moves when the test advances it. Sleepers wake in
/// deadline order; a cancelled sleeper returns at once, like `Task.sleep`.
final class ManualRoomClock: RoomClock, @unchecked Sendable {
    private struct Sleeper {
        let id: UUID
        let duration: UInt64
        let deadline: UInt64
        let continuation: CheckedContinuation<Void, Never>
    }

    private let lock = NSLock()
    private var current: UInt64 = 1_000_000_000_000
    private var sleepers: [Sleeper] = []
    private var cancelled: Set<UUID> = []

    func now() -> UInt64 { lock.withLock { current } }

    func sleep(nanoseconds: UInt64) async {
        let id = UUID()
        await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let wakeNow = lock.withLock { () -> Bool in
                    if cancelled.remove(id) != nil || nanoseconds == 0 { return true }
                    sleepers.append(Sleeper(id: id, duration: nanoseconds, deadline: current + nanoseconds, continuation: continuation))
                    return false
                }
                if wakeNow { continuation.resume() }
            }
        } onCancel: {
            let sleeper = lock.withLock { () -> Sleeper? in
                guard let index = sleepers.firstIndex(where: { $0.id == id }) else {
                    cancelled.insert(id)
                    return nil
                }
                return sleepers.remove(at: index)
            }
            sleeper?.continuation.resume()
        }
    }

    /// Whether some task is asleep for exactly `nanoseconds` — lets a test
    /// wait until a timer is armed before moving time past it.
    func hasSleeper(lasting nanoseconds: UInt64) -> Bool {
        lock.withLock { sleepers.contains { $0.duration == nanoseconds } }
    }

    func advance(nanoseconds: UInt64) {
        let due = lock.withLock { () -> [Sleeper] in
            current += nanoseconds
            let due = sleepers.filter { $0.deadline <= current }.sorted { $0.deadline < $1.deadline }
            sleepers.removeAll { $0.deadline <= current }
            return due
        }
        for sleeper in due { sleeper.continuation.resume() }
    }
}

struct FakeSocketClosed: Error {}

/// One fake socket: records what the client sends, lets the test deliver
/// frames or fail it, and fails a pending receive on cancel as URLSession does.
final class FakeWebSocket: WebSocketConnection, @unchecked Sendable {
    private let lock = NSLock()
    private var inbox: [URLSessionWebSocketTask.Message] = []
    private var waiter: CheckedContinuation<URLSessionWebSocketTask.Message, Error>?
    private var failed = false
    private var _sent: [URLSessionWebSocketTask.Message] = []
    private var _closeCode: URLSessionWebSocketTask.CloseCode?

    var sent: [URLSessionWebSocketTask.Message] { lock.withLock { _sent } }
    var closeCode: URLSessionWebSocketTask.CloseCode? { lock.withLock { _closeCode } }
    var sentStrings: [String] {
        sent.compactMap { if case .string(let text) = $0 { text } else { nil } }
    }
    var sentData: [Data] {
        sent.compactMap { if case .data(let data) = $0 { data } else { nil } }
    }

    func send(_ message: URLSessionWebSocketTask.Message) async throws {
        try lock.withLock {
            if failed { throw FakeSocketClosed() }
            _sent.append(message)
        }
    }

    func receive() async throws -> URLSessionWebSocketTask.Message {
        try await withCheckedThrowingContinuation { continuation in
            let result = lock.withLock { () -> Result<URLSessionWebSocketTask.Message, Error>? in
                if !inbox.isEmpty { return .success(inbox.removeFirst()) }
                if failed { return .failure(FakeSocketClosed()) }
                waiter = continuation
                return nil
            }
            if let result { continuation.resume(with: result) }
        }
    }

    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        lock.withLock { _closeCode = closeCode }
        fail()
    }

    func deliver(_ message: URLSessionWebSocketTask.Message) {
        let receiver = lock.withLock { () -> CheckedContinuation<URLSessionWebSocketTask.Message, Error>? in
            guard let waiter else {
                inbox.append(message)
                return nil
            }
            self.waiter = nil
            return waiter
        }
        receiver?.resume(returning: message)
    }

    /// The connection drops: a pending or later receive throws.
    func fail() {
        let receiver = lock.withLock { () -> CheckedContinuation<URLSessionWebSocketTask.Message, Error>? in
            failed = true
            defer { waiter = nil }
            return waiter
        }
        receiver?.resume(throwing: FakeSocketClosed())
    }
}

final class FakeWebSocketTransport: WebSocketTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var _sockets: [FakeWebSocket] = []
    private var _requests: [URLRequest] = []

    var sockets: [FakeWebSocket] { lock.withLock { _sockets } }
    var requests: [URLRequest] { lock.withLock { _requests } }
    var last: FakeWebSocket? { sockets.last }

    func open(_ request: URLRequest) -> any WebSocketConnection {
        let socket = FakeWebSocket()
        lock.withLock {
            _sockets.append(socket)
            _requests.append(request)
        }
        return socket
    }
}

/// Lets queued actor and task work run. The clients hop between their actor,
/// the main actor and detached timer tasks, so a few real milliseconds are
/// needed rather than a fixed number of yields.
func settle() async {
    for _ in 0..<10 { try? await Task.sleep(nanoseconds: 1_000_000) }
}

/// Polls `condition` until it holds or two seconds pass.
func eventually(_ message: String = "condition never held", file: StaticString = #filePath, line: UInt = #line,
                _ condition: () async -> Bool) async {
    let deadline = Date().addingTimeInterval(2)
    while Date() < deadline {
        if await condition() { return }
        try? await Task.sleep(nanoseconds: 1_000_000)
    }
    XCTFail(message, file: file, line: line)
}

/// Records what a client's delegate is told, on the main actor.
@MainActor
final class EventLog {
    private(set) var events: [String] = []
    func append(_ event: Any) { events.append(String(describing: event)) }
    func count(_ name: String) -> Int { events.filter { $0 == name }.count }
}

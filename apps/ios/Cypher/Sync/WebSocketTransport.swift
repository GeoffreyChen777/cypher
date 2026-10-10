// Injectable WebSocket and clock seams for the room clients (ChatRoomClient,
// RegistryClient). Production uses URLSession and the system uptime clock;
// tests substitute in-memory fakes to drive the socket lifecycle.

import Foundation

/// One open WebSocket. `URLSessionWebSocketTask` conforms as-is.
protocol WebSocketConnection: AnyObject, Sendable {
    func send(_ message: URLSessionWebSocketTask.Message) async throws
    func receive() async throws -> URLSessionWebSocketTask.Message
    func cancel(with closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?)
}

extension URLSessionWebSocketTask: WebSocketConnection {}

/// Opens (and starts) a WebSocket for a request.
protocol WebSocketTransport: Sendable {
    func open(_ request: URLRequest) -> any WebSocketConnection
}

struct URLSessionWebSocketTransport: WebSocketTransport {
    func open(_ request: URLRequest) -> any WebSocketConnection {
        let task = URLSession.shared.webSocketTask(with: request)
        task.resume()
        return task
    }
}

/// Monotonic time and sleeping for the socket timers.
protocol RoomClock: Sendable {
    /// Monotonic nanoseconds; only differences are meaningful.
    func now() -> UInt64
    /// Returns after `nanoseconds`, or early (without throwing) when the
    /// calling task is cancelled.
    func sleep(nanoseconds: UInt64) async
}

struct SystemRoomClock: RoomClock {
    func now() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
    func sleep(nanoseconds: UInt64) async { try? await Task.sleep(nanoseconds: nanoseconds) }
}

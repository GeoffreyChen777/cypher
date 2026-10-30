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
        let task = URLSession.shared.webSocketTask(with: request.liftingBearer())
        task.resume()
        return task
    }
}

extension URLRequest {
    /// Room URLs name the bearer as a `token` query item. Move it into
    /// `Authorization` so it never appears in a URL on the wire or in the
    /// edge's request logs; the Rust dialer does the same
    /// (crates/net/src/dial.rs `lift_bearer`). An existing header wins, and the
    /// query item is dropped either way.
    func liftingBearer() -> URLRequest {
        guard let url,
            var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
            let items = components.queryItems,
            let token = items.first(where: { $0.name == "token" })?.value
        else { return self }
        let kept = items.filter { $0.name != "token" }
        components.queryItems = kept.isEmpty ? nil : kept
        var request = self
        request.url = components.url ?? url
        if request.value(forHTTPHeaderField: "Authorization") == nil, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return request
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

import Foundation

/// One bounded repair exchange, never an independent retry/polling owner.
/// The request provider is consulted for every call to refresh credentials.
@MainActor
struct Sync3HTTPTransport {
    let request: @MainActor () async throws -> URLRequest

    func exchange(_ frame: [String: JSONValue]) async throws -> [String: JSONValue] {
        var request = try await request()
        try Task.checkCancellation()
        guard let url = request.url,
              url.scheme == "https" || (url.scheme == "http" && ["127.0.0.1", "localhost", "::1"].contains(url.host)) else {
            try Sync3Wire.fail("insecure_endpoint")
        }
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try Sync3Wire.encode(frame)
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 15
        config.timeoutIntervalForResource = 15
        let session = URLSession(configuration: config, delegate: V3NoRedirect(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        return try await withTaskCancellationHandler {
            do {
                let (bytes, response) = try await session.bytes(for: request)
                guard let response = response as? HTTPURLResponse else { try Sync3Wire.fail("invalid_response") }
                if response.statusCode == 401 { try Sync3Wire.fail("reauth_required") }
                if response.statusCode == 403 { try Sync3Wire.fail("not_authorized") }
                if response.statusCode == 429 || response.statusCode >= 500 { try Sync3Wire.fail("transport_unavailable") }
                guard [200, 400, 409].contains(response.statusCode) else { try Sync3Wire.fail("unexpected_http_status") }
                if response.expectedContentLength > Int64(Sync3Wire.maxFrameBytes) { try Sync3Wire.fail("frame_too_large") }
                var data = Data()
                for try await byte in bytes {
                    guard data.count < Sync3Wire.maxFrameBytes else { try Sync3Wire.fail("frame_too_large") }
                    data.append(byte)
                }
                try Task.checkCancellation()
                return try Sync3Wire.decode(data)
            } catch is CancellationError { throw CancellationError() }
            catch let error as Sync3Error { throw error }
            catch {
                try Task.checkCancellation()
                try Sync3Wire.fail("transport_unavailable")
            }
        } onCancel: {
            session.invalidateAndCancel()
        }
    }

}

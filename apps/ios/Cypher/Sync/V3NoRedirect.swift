import Foundation

/// A scoped credential/request cannot be redirected into another endpoint.
final class V3NoRedirect: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

enum V3HTTP {
    static func notification(_ request: URLRequest) async throws -> (Data, URLResponse) {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 15; config.timeoutIntervalForResource = 15
        let session = URLSession(configuration: config, delegate: V3NoRedirect(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        return try await withTaskCancellationHandler {
            let (bytes, response) = try await session.bytes(for: request)
            guard response.expectedContentLength < 32768 else { throw Sync3Error.protocolError("frame_too_large") }
            var data = Data()
            for try await byte in bytes {
                guard data.count < 32767 else { throw Sync3Error.protocolError("frame_too_large") }
                data.append(byte)
            }
            try Task.checkCancellation()
            return (data, response)
        } onCancel: { session.invalidateAndCancel() }
    }
}

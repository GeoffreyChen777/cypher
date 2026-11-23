// Loading chat images: the bytes of an `ImageSource` (the chat's host through
// the Files RPCs, the web over an ephemeral session, a data URI), capped at
// `ImageSource.maxBytes`, decoded once off the main thread with ImageIO and
// downsampled to the size it is drawn at, and cached by source and size.

import ImageIO
import SwiftUI
import UIKit

/// How image previews read local files: the chat's checkout and the other
/// folders it may read, on its host, through the Files RPCs.
struct TranscriptImageFiles {
    /// Names the device, chat and checkout, so a cached image never shows in
    /// another chat that writes the same path.
    var scope: String
    /// The folders local paths resolve in (`ImageSource.Context`).
    var context: @MainActor () async -> ImageSource.Context
    /// One candidate's bytes, refused past `ImageSource.maxBytes`.
    var read: @MainActor (ImageSource.Candidate) async throws -> Data
}

extension EnvironmentValues {
    /// Where the transcript's images read local files; nil where there is no
    /// chat checkout (local images then say so).
    @Entry var transcriptImageFiles: TranscriptImageFiles? = nil
}

/// A decoded image, at most the pixel size it was asked for.
final class LoadedImage: @unchecked Sendable {
    enum Content {
        /// A bitmap — animated when it has frames.
        case bitmap(UIImage)
        /// An SVG's source: shown as a card that opens it as a page.
        case svg(String)
    }

    let content: Content
    /// The image's own size in pixels (oriented); its natural size in points.
    let pixelSize: CGSize
    let format: ImageSource.Format
    /// The bytes it was decoded from: what Share sends.
    let data: Data
    /// What the image is called when shared (`chart.png`).
    let fileName: String

    init(content: Content, pixelSize: CGSize, format: ImageSource.Format, data: Data, fileName: String) {
        self.content = content
        self.pixelSize = pixelSize
        self.format = format
        self.data = data
        self.fileName = fileName
    }

    var bitmap: UIImage? {
        if case .bitmap(let image) = content { return image }
        return nil
    }

    var animated: Bool { (bitmap?.images?.count ?? 0) > 1 }

    /// Decoded cost in bytes, for the cache.
    var cost: Int {
        guard let bitmap else { return data.count }
        let frames = max(1, bitmap.images?.count ?? 1)
        return Int(bitmap.size.width * bitmap.scale * bitmap.size.height * bitmap.scale) * 4 * frames
    }
}

/// Why an image shows its error placeholder instead.
enum ImageLoadError: Error, Equatable, LocalizedError {
    case notImage, insecure, unsupported, outside, noFiles, tooLarge, notFound, undecodable
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .notImage: "Not an image"
        case .insecure: "Not loaded: http:// is not secure"
        case .unsupported: "Unsupported data URI (base64 only)"
        case .outside: "Outside this session's folders"
        case .noFiles: "Files on the host aren't available here"
        case .tooLarge: "Larger than 20 MB"
        case .notFound: "File not found"
        case .undecodable: "Not an image iOS can show"
        case .failed(let reason): reason
        }
    }

    /// Whether trying again could help: not for what the source itself says.
    var retryable: Bool {
        switch self {
        case .notFound, .failed: true
        case .notImage, .insecure, .unsupported, .outside, .noFiles, .tooLarge, .undecodable: false
        }
    }
}

@MainActor
final class ImageLoader {
    static let shared = ImageLoader()

    /// The pixel sizes images decode at: a reply's block (a column at 3x, 360
    /// pt tall), the full-screen viewer, and a chip's or tool row's thumbnail.
    enum Size: Int {
        case block = 2048
        case viewer = 4096
        case thumbnail = 144

        var maxPixel: CGFloat { CGFloat(rawValue) }
    }

    private let decoded = NSCache<NSString, LoadedImage>()
    private let bytes = NSCache<NSString, NSData>()
    private var failures: [String: ImageLoadError] = [:]
    private var inFlight: [String: Task<Result<(Data, String), ImageLoadError>, Never>] = [:]
    private var decoding: [String: Task<LoadedImage?, Never>] = [:]
    private var contexts: [String: (context: ImageSource.Context, at: Date)] = [:]

    init() {
        decoded.totalCostLimit = 160 * 1024 * 1024
        bytes.totalCostLimit = 64 * 1024 * 1024
    }

    private static func key(_ src: String, _ files: TranscriptImageFiles?) -> String {
        // Only local paths depend on the chat; a URL or data URI is the same
        // image everywhere.
        let local: Bool
        switch ImageSource.classify(src, ImageSource.Context()) {
        case .local, .outside: local = true
        default: local = false
        }
        return local ? "\(files?.scope ?? "-")\u{1}\(src)" : src
    }

    /// The image, when it was decoded at `size` already: a row scrolled back
    /// in shows it on its first frame.
    func cached(_ src: String, files: TranscriptImageFiles?, size: Size) -> LoadedImage? {
        decoded.object(forKey: "\(Self.key(src, files))\u{1}\(size.rawValue)" as NSString)
    }

    /// The error an earlier load of `src` ended in, so a row scrolled back in
    /// does not try again.
    func failure(_ src: String, files: TranscriptImageFiles?) -> ImageLoadError? {
        failures[Self.key(src, files)]
    }

    /// Load `src` decoded at `size`.
    func load(_ src: String, files: TranscriptImageFiles?, size: Size) async -> Result<LoadedImage, ImageLoadError> {
        let base = Self.key(src, files)
        let key = "\(base)\u{1}\(size.rawValue)"
        if let hit = decoded.object(forKey: key as NSString) { return .success(hit) }
        if let failure = failures[base] { return .failure(failure) }
        let data: Data
        let name: String
        switch await fetch(src, key: base, files: files) {
        case .success(let fetched):
            (data, name) = fetched
        case .failure(let error):
            if !Task.isCancelled, error != .failed("Cancelled") { failures[base] = error }
            return .failure(error)
        }
        let task: Task<LoadedImage?, Never>
        if let running = decoding[key] {
            task = running
        } else {
            let hint = Self.format(src)
            task = Task.detached(priority: .userInitiated) {
                ImageDecoder.decode(data, hint: hint, maxPixel: size.maxPixel, fileName: name)
            }
            decoding[key] = task
        }
        let image = await task.value
        decoding[key] = nil
        guard let image else {
            failures[base] = .undecodable
            return .failure(.undecodable)
        }
        decoded.setObject(image, forKey: key as NSString, cost: image.cost)
        return .success(image)
    }

    /// Forget a failed load, for a retry.
    func forget(_ src: String, files: TranscriptImageFiles?) {
        failures[Self.key(src, files)] = nil
    }

    /// The format a source's path or media type names, if any.
    private static func format(_ src: String) -> ImageSource.Format? {
        switch ImageSource.classify(src, ImageSource.Context()) {
        case .local(_, let format), .data(let format, _): format
        case .remote(let url): ImageSource.remoteExtension(url).flatMap { ImageSource.Format(extension: $0) }
        case .outside(let path): ImageSource.Format(path: path)
        default: nil
        }
    }

    // MARK: Bytes

    /// The bytes behind `src`, read once at a time per source.
    private func fetch(
        _ src: String, key: String, files: TranscriptImageFiles?
    ) async -> Result<(Data, String), ImageLoadError> {
        if let data = bytes.object(forKey: key as NSString) {
            return .success((data as Data, Self.fileName(src)))
        }
        let task: Task<Result<(Data, String), ImageLoadError>, Never>
        if let running = inFlight[key] {
            task = running
        } else {
            task = Task { await self.read(src, files: files) }
            inFlight[key] = task
        }
        let result = await task.value
        inFlight[key] = nil
        if case .success(let (data, _)) = result {
            bytes.setObject(data as NSData, forKey: key as NSString, cost: data.count)
        }
        return result
    }

    private func read(_ src: String, files: TranscriptImageFiles?) async -> Result<(Data, String), ImageLoadError> {
        let quick = ImageSource.classify(src, ImageSource.Context())
        switch quick {
        case .notImage: return .failure(.notImage)
        case .rejected(.insecure): return .failure(.insecure)
        case .rejected(.unsupported): return .failure(.unsupported)
        case .data(_, let payload):
            // Base64 is 4/3 of its bytes: refuse before decoding.
            guard UInt64(payload.utf8.count) / 4 * 3 <= ImageSource.maxBytes else { return .failure(.tooLarge) }
            guard let data = Data(base64Encoded: payload, options: .ignoreUnknownCharacters) else {
                return .failure(.unsupported)
            }
            return .success((data, Self.fileName(src)))
        case .remote(let url):
            return await Self.download(url).map { ($0, Self.fileName(src)) }
        case .local, .outside:
            guard let files else { return .failure(.noFiles) }
            let context = await context(for: files)
            switch ImageSource.classify(src, context) {
            case .local(let candidates, _):
                var last: ImageLoadError = .notFound
                for candidate in candidates {
                    do {
                        let data = try await files.read(candidate)
                        guard UInt64(data.count) <= ImageSource.maxBytes else { return .failure(.tooLarge) }
                        return .success((data, Self.fileName(src)))
                    } catch is CancellationError {
                        return .failure(.failed("Cancelled"))
                    } catch {
                        last = Self.loadError(error)
                        if last == .tooLarge { return .failure(last) }
                    }
                }
                return .failure(last)
            case .outside:
                return .failure(.outside)
            default:
                return .failure(.notImage)
            }
        }
    }

    /// The folders `files` resolves in, asked once a minute at most.
    private func context(for files: TranscriptImageFiles) async -> ImageSource.Context {
        if let hit = contexts[files.scope], hit.at.timeIntervalSinceNow > -60 { return hit.context }
        let context = await files.context()
        contexts[files.scope] = (context, Date())
        return context
    }

    private static func loadError(_ error: Error) -> ImageLoadError {
        if let error = error as? ImageLoadError { return error }
        if case WorkspaceBrowserError.tooLarge = error { return .tooLarge }
        if case WorkspaceBrowserError.invalidPath = error { return .notFound }
        if case RelayError.rpc(let message) = error {
            let lower = message.lowercased()
            if lower.contains("no such file") || lower.contains("not found") { return .notFound }
            if lower.contains("too large") { return .tooLarge }
            return .failed(message)
        }
        return .failed(WorkspaceBrowserSession.errorMessage(error))
    }

    /// The name a source's image is shared under.
    static func fileName(_ src: String) -> String {
        switch ImageSource.classify(src, ImageSource.Context()) {
        case .data(let format, _):
            return "image.\(format.fileExtension)"
        case .remote(let url):
            let path = URL(string: url)?.lastPathComponent ?? ""
            return path.isEmpty || path == "/" ? "image" : path
        default:
            var path = Substring(src)
            if let cut = path.firstIndex(where: { $0 == "#" || $0 == "?" }) { path = path[..<cut] }
            let name = path.split(separator: "/").last.map(String.init) ?? "image"
            return name.removingPercentEncoding ?? name
        }
    }

    // MARK: Web

    /// No cookies, no stored credentials, no cache on disk; redirects stay on
    /// https and auth challenges are refused.
    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieAcceptPolicy = .never
        configuration.httpShouldSetCookies = false
        configuration.httpCookieStorage = nil
        configuration.urlCredentialStorage = nil
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 60
        return URLSession(configuration: configuration, delegate: WebDelegate(), delegateQueue: nil)
    }()

    private final class WebDelegate: NSObject, URLSessionTaskDelegate {
        func urlSession(
            _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
            newRequest request: URLRequest
        ) async -> URLRequest? {
            request.url?.scheme?.lowercased() == "https" ? request : nil
        }

        func urlSession(
            _ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge
        ) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
            challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust
                ? (.performDefaultHandling, nil) : (.cancelAuthenticationChallenge, nil)
        }
    }

    private static func download(_ string: String) async -> Result<Data, ImageLoadError> {
        guard
            let url = URL(string: string)
                ?? URL(
                    string: string.addingPercentEncoding(
                        withAllowedCharacters: .urlFragmentAllowed) ?? ""),
            url.scheme?.lowercased() == "https"
        else { return .failure(.notImage) }
        var request = URLRequest(url: url)
        request.httpShouldHandleCookies = false
        request.setValue("image/*", forHTTPHeaderField: "Accept")
        do {
            let (stream, response) = try await session.bytes(for: request)
            if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                return .failure(.failed("The server answered \(http.statusCode)"))
            }
            if response.expectedContentLength > Int64(ImageSource.maxBytes) { return .failure(.tooLarge) }
            var data = Data()
            if response.expectedContentLength > 0 { data.reserveCapacity(Int(response.expectedContentLength)) }
            var buffer = [UInt8]()
            buffer.reserveCapacity(64 * 1024)
            for try await byte in stream {
                buffer.append(byte)
                if buffer.count == 64 * 1024 {
                    data.append(contentsOf: buffer)
                    buffer.removeAll(keepingCapacity: true)
                    if UInt64(data.count) > ImageSource.maxBytes { return .failure(.tooLarge) }
                }
            }
            data.append(contentsOf: buffer)
            guard UInt64(data.count) <= ImageSource.maxBytes else { return .failure(.tooLarge) }
            return .success(data)
        } catch is CancellationError {
            return .failure(.failed("Cancelled"))
        } catch {
            return .failure(.failed((error as NSError).localizedDescription))
        }
    }
}

// MARK: - Decoding

enum ImageDecoder {
    /// Frames an animation keeps at most; a longer one shows its first.
    static let maxFrames = 120
    /// Decoded bytes an animation may take; a bigger one shows its first frame.
    static let maxAnimationBytes = 48 * 1024 * 1024

    /// `data` decoded no larger than `maxPixel` on its longer side, never
    /// past its own size. An SVG keeps its source.
    nonisolated static func decode(
        _ data: Data, hint: ImageSource.Format?, maxPixel: CGFloat, fileName: String
    ) -> LoadedImage? {
        let sniffed = ImageSource.Format(sniffing: data)
        guard let format = sniffed ?? hint else { return nil }
        if format == .svg {
            guard let text = String(data: data, encoding: .utf8), text.contains("<svg") else { return nil }
            return LoadedImage(
                content: .svg(text), pixelSize: svgSize(text), format: .svg, data: data, fileName: fileName)
        }
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0
        else { return nil }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
        var width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.doubleValue ?? 0
        var height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.doubleValue ?? 0
        let orientation = (properties[kCGImagePropertyOrientation] as? NSNumber)?.intValue ?? 1
        if (5...8).contains(orientation) { swap(&width, &height) }
        let longest = max(width, height)
        let target = longest > 0 ? min(maxPixel, longest) : maxPixel
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: target,
        ]
        func frame(_ index: Int) -> CGImage? {
            CGImageSourceCreateThumbnailAtIndex(source, index, options as CFDictionary)
        }
        guard let first = frame(0) else { return nil }
        if width <= 0 || height <= 0 {
            width = Double(first.width)
            height = Double(first.height)
        }
        let pixelSize = CGSize(width: width, height: height)
        let count = CGImageSourceGetCount(source)
        let frameBytes = first.width * first.height * 4
        if count > 1, count <= maxFrames, frameBytes * count <= maxAnimationBytes {
            var images: [UIImage] = [UIImage(cgImage: first)]
            var duration = delay(source, 0)
            for index in 1..<count {
                guard let image = frame(index) else { break }
                images.append(UIImage(cgImage: image))
                duration += delay(source, index)
            }
            if images.count > 1, let animated = UIImage.animatedImage(with: images, duration: duration) {
                return LoadedImage(
                    content: .bitmap(animated), pixelSize: pixelSize, format: format, data: data, fileName: fileName)
            }
        }
        return LoadedImage(
            content: .bitmap(UIImage(cgImage: first)), pixelSize: pixelSize, format: format, data: data,
            fileName: fileName)
    }

    /// One frame's delay in seconds (GIF or WebP), as browsers clamp it: a
    /// delay under 20 ms plays at 100 ms.
    private static func delay(_ source: CGImageSource, _ index: Int) -> Double {
        let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any] ?? [:]
        let dictionary =
            (properties[kCGImagePropertyGIFDictionary] as? [CFString: Any])
            ?? (properties[kCGImagePropertyWebPDictionary] as? [CFString: Any]) ?? [:]
        let value =
            (dictionary[kCGImagePropertyGIFUnclampedDelayTime] as? NSNumber)?.doubleValue
            ?? (dictionary[kCGImagePropertyGIFDelayTime] as? NSNumber)?.doubleValue
            ?? (dictionary[kCGImagePropertyWebPUnclampedDelayTime] as? NSNumber)?.doubleValue
            ?? (dictionary[kCGImagePropertyWebPDelayTime] as? NSNumber)?.doubleValue ?? 0.1
        return value < 0.02 ? 0.1 : value
    }

    /// An SVG's size from its root's `width`/`height` (else its `viewBox`),
    /// in px; zero when it names none.
    static func svgSize(_ text: String) -> CGSize {
        guard let open = text.range(of: "<svg"),
            let close = text[open.upperBound...].firstIndex(of: ">")
        else { return .zero }
        let tag = String(text[open.upperBound..<close])
        func attribute(_ name: String) -> String? {
            guard let match = tag.firstMatch(of: try! Regex("(?:^|\\s)\(name)\\s*=\\s*[\"']([^\"']*)[\"']")),
                let value = match.output[1].substring
            else { return nil }
            return String(value)
        }
        func number(_ value: String?) -> Double? {
            guard let value else { return nil }
            let digits = value.prefix { $0.isNumber || $0 == "." }
            return Double(digits).flatMap { value.hasSuffix("%") ? nil : $0 }
        }
        if let width = number(attribute("width")), let height = number(attribute("height")), width > 0, height > 0 {
            return CGSize(width: width, height: height)
        }
        let box = (attribute("viewBox") ?? "").split(whereSeparator: { $0 == " " || $0 == "," }).compactMap {
            Double($0)
        }
        if box.count == 4, box[2] > 0, box[3] > 0 { return CGSize(width: box[2], height: box[3]) }
        return .zero
    }
}

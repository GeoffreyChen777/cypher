// Loading file previews (FilePreview.swift): the file is read like a chat
// image (ImageLoader), from the chat's folder and then each root it may
// read on its host, through the Files RPCs, and stops at its kind's cap. A
// table reads only its head; a PDF shows its first pages (PDFKit, drawn for
// the column by PDFPageImages) and a video one frame (AVFoundation), both
// from a copy in the app's caches so QuickLook and the player can open it.
// Cached by chat and source, like the desktop's file_card store.

import AVFoundation
import PDFKit
import UIKit

/// A loaded file preview.
final class LoadedFilePreview: @unchecked Sendable {
    enum Body {
        case table(FilePreview.Table)
        /// A video's frame.
        case frame(UIImage)
        /// A PDF's first pages (`PreviewSize.pdfRowMax` at most), as many as
        /// its widest row shows: each one's size in points, drawn for the
        /// column from `file`.
        case pages([CGSize])
        /// An html or svg page: its `<title>` and source (the artifact viewer).
        case page(title: String?, code: String)
        /// Only the name and size: a video too large to fetch for its frame
        /// on its own, or a file with nothing to draw.
        case plain
    }

    let kind: FilePreview.Kind
    let name: String
    let size: UInt64
    let body: Body
    /// The copy QuickLook or the player opens (a PDF or a video).
    let file: URL?
    /// A PDF's pages, a video's length in seconds.
    let pages: Int?
    let duration: Double?

    init(
        kind: FilePreview.Kind, name: String, size: UInt64, body: Body, file: URL? = nil, pages: Int? = nil,
        duration: Double? = nil
    ) {
        self.kind = kind
        self.name = name
        self.size = size
        self.body = body
        self.file = file
        self.pages = pages
        self.duration = duration
    }

    /// What the card says under the name: "12 rows", "3 pages · 48 KB",
    /// "0:04 · 1.2 MB", "HTML page".
    var detail: String {
        switch body {
        case .table(let table): return table.countLabel
        case .page: return kind == .svg ? "SVG image" : "HTML page"
        case .frame, .pages, .plain:
            var parts: [String] = []
            if let pages { parts.append(pages == 1 ? "1 page" : "\(pages) pages") }
            if let duration, duration.isFinite, duration > 0 { parts.append(Self.clock(duration)) }
            parts.append(FilePreview.humanSize(size))
            return parts.joined(separator: " · ")
        }
    }

    /// `0:04`, `12:30`, `1:02:03`.
    static func clock(_ seconds: Double) -> String {
        let total = Int(seconds.rounded())
        let (h, m, s) = (total / 3600, total / 60 % 60, total % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}

/// Why a file shows a neutral card saying so instead of its preview.
enum FilePreviewError: Error, Equatable, LocalizedError {
    case outside, noFiles, notFound, unreadable
    case tooLarge(FilePreview.Kind)
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .outside: "Outside this session's folders"
        case .noFiles: "Files on the host aren't available here"
        case .notFound: "File not found"
        case .unreadable: "No preview for this file"
        case .tooLarge(let kind): "Too large (over \(kind.maxBytes / (1024 * 1024)) MB)"
        case .failed(let reason): reason
        }
    }

    /// Whether trying again could help.
    var retryable: Bool {
        switch self {
        case .notFound, .failed: true
        case .outside, .noFiles, .unreadable, .tooLarge: false
        }
    }

    init(_ error: ImageLoadError, kind: FilePreview.Kind) {
        switch error {
        case .outside: self = .outside
        case .noFiles: self = .noFiles
        case .notFound: self = .notFound
        case .tooLarge: self = .tooLarge(kind)
        case .failed(let reason): self = .failed(reason)
        case .notImage, .insecure, .unsupported, .undecodable: self = .unreadable
        }
    }
}

@MainActor
final class FilePreviewLoader {
    static let shared = FilePreviewLoader()

    /// A video's frame is fetched on its own only up to this size (the whole
    /// file comes over the relay); a larger one shows its name and size and
    /// is fetched when tapped.
    static let videoFrameBytes: UInt64 = 24 * 1024 * 1024
    /// The pixel size a video frame is drawn at, at most.
    nonisolated static let thumbPixels: CGFloat = 1440

    private let ready = NSCache<NSString, LoadedFilePreview>()
    private var failures: [String: FilePreviewError] = [:]
    private var inFlight: [String: Task<Result<LoadedFilePreview, FilePreviewError>, Never>] = [:]
    private var contexts: [String: (context: ImageSource.Context, at: Date)] = [:]

    init() {
        ready.countLimit = 32
    }

    private static func key(_ src: String, _ files: TranscriptImageFiles?) -> String {
        "\(files?.scope ?? "-")\u{1}\(src)"
    }

    /// The preview, when it was loaded already: a row scrolled back in shows
    /// it on its first frame.
    func cached(_ src: String, files: TranscriptImageFiles?) -> LoadedFilePreview? {
        ready.object(forKey: Self.key(src, files) as NSString)
    }

    func failure(_ src: String, files: TranscriptImageFiles?) -> FilePreviewError? {
        failures[Self.key(src, files)]
    }

    func forget(_ src: String, files: TranscriptImageFiles?) {
        failures[Self.key(src, files)] = nil
    }

    /// Load the preview of `src`, a file of `kind`; `whole` fetches a video
    /// past `videoFrameBytes` too (it was tapped).
    func load(
        _ src: String, kind: FilePreview.Kind, files: TranscriptImageFiles?, whole: Bool = false
    ) async -> Result<LoadedFilePreview, FilePreviewError> {
        let key = Self.key(src, files)
        if let hit = ready.object(forKey: key as NSString), !(whole && hit.file == nil && hit.kind == .video) {
            return .success(hit)
        }
        if !whole, let failure = failures[key] { return .failure(failure) }
        let task: Task<Result<LoadedFilePreview, FilePreviewError>, Never>
        let flight = whole ? key + "\u{1}whole" : key
        if let running = inFlight[flight] {
            task = running
        } else {
            task = Task { await self.read(src, kind: kind, files: files, whole: whole) }
            inFlight[flight] = task
        }
        let result = await task.value
        inFlight[flight] = nil
        switch result {
        case .success(let loaded):
            ready.setObject(loaded, forKey: key as NSString)
            failures[key] = nil
        case .failure(let error):
            if !Task.isCancelled, error != .failed("Cancelled") { failures[key] = error }
        }
        return result
    }

    private func read(
        _ src: String, kind: FilePreview.Kind, files: TranscriptImageFiles?, whole: Bool
    ) async -> Result<LoadedFilePreview, FilePreviewError> {
        guard let files, let preview = files.preview else { return .failure(.noFiles) }
        guard let path = ImageSource.localPath(src) else { return .failure(.unreadable) }
        let context = await context(for: files)
        let candidates: [ImageSource.Candidate]
        switch ImageSource.candidates(path, context) {
        case .inside(let found): candidates = found
        case .outside: return .failure(.outside)
        }
        let name = FilePreview.fileName(src)
        var last: FilePreviewError = .notFound
        for candidate in candidates {
            do {
                // A table reads its head; a video first only its size (its
                // frame needs the whole file); the rest read whole, up to the cap.
                let (limit, head): (UInt64, Bool) =
                    switch kind {
                    case .csv, .tsv: (kind.maxBytes, true)
                    case .video: (1, true)
                    case .pdf, .html, .svg: (kind.maxBytes, false)
                    }
                var (data, size) = try await preview(candidate, limit, head)
                if kind == .video {
                    guard size <= kind.maxBytes else { return .failure(.tooLarge(kind)) }
                    guard whole || size <= Self.videoFrameBytes else {
                        return .success(LoadedFilePreview(kind: kind, name: name, size: size, body: .plain))
                    }
                    (data, size) = try await preview(candidate, kind.maxBytes, false)
                }
                let fetched = data
                let total = size
                return await Task.detached(priority: .userInitiated) {
                    await Self.preview(kind: kind, name: name, data: fetched, size: total)
                }.value
            } catch is CancellationError {
                return .failure(.failed("Cancelled"))
            } catch {
                last = FilePreviewError(ImageLoader.loadError(error), kind: kind)
                if case .tooLarge = last { return .failure(last) }
            }
        }
        return .failure(last)
    }

    private func context(for files: TranscriptImageFiles) async -> ImageSource.Context {
        if let hit = contexts[files.scope], hit.at.timeIntervalSinceNow > -60 { return hit.context }
        let context = await files.context()
        contexts[files.scope] = (context, Date())
        return context
    }

    // MARK: Previews (off the main thread)

    /// What a read file shows: `data` is what was read of it, `size` its
    /// whole size.
    nonisolated static func preview(
        kind: FilePreview.Kind, name: String, data: Data, size: UInt64
    ) async -> Result<LoadedFilePreview, FilePreviewError> {
        switch kind {
        case .csv, .tsv:
            let text = String(decoding: data, as: UTF8.self)
            let table = FilePreview.parseTable(
                text, delimiter: kind.delimiter ?? ",", complete: UInt64(data.count) >= size)
            return .success(LoadedFilePreview(kind: kind, name: name, size: size, body: .table(table)))
        case .html, .svg:
            let code = String(decoding: data, as: UTF8.self)
            let title = kind == .html ? ArtifactFence.pageTitle(code) : nil
            return .success(
                LoadedFilePreview(kind: kind, name: name, size: size, body: .page(title: title, code: code)))
        case .pdf:
            guard let document = PDFDocument(data: data), document.pageCount > 0 else {
                return .failure(.unreadable)
            }
            let file = copy(data, name: name)
            // Up to the first page with no size: the row needs each shown
            // page's aspect, in order.
            let pages = Array(
                (0..<min(document.pageCount, PreviewSize.pdfRowMax))
                    .map { document.page(at: $0).map(PDFPageImages.size) ?? .zero }
                    .prefix { $0.width > 0 && $0.height > 0 })
            return .success(
                LoadedFilePreview(
                    kind: kind, name: name, size: size, body: file != nil && !pages.isEmpty ? .pages(pages) : .plain,
                    file: file, pages: document.pageCount))
        case .video:
            guard let file = copy(data, name: name) else { return .failure(.failed("Couldn't keep a copy to play")) }
            let asset = AVURLAsset(url: file)
            let duration = try? await asset.load(.duration).seconds
            let generator = AVAssetImageGenerator(asset: asset)
            generator.appliesPreferredTrackTransform = true
            generator.maximumSize = CGSize(width: thumbPixels, height: thumbPixels)
            generator.requestedTimeToleranceBefore = .positiveInfinity
            generator.requestedTimeToleranceAfter = .positiveInfinity
            // A little in: the very first frame is often black.
            let at = CMTime(seconds: min(0.5, (duration ?? 0) / 4), preferredTimescale: 600)
            let frame = try? await generator.image(at: at).image
            return .success(
                LoadedFilePreview(
                    kind: kind, name: name, size: size, body: frame.map { .frame(UIImage(cgImage: $0)) } ?? .plain,
                    file: file, duration: duration))
        }
    }

    /// The bytes as a file named like the original, in the caches folder
    /// (one folder per copy, so two files of one name don't meet).
    nonisolated static func copy(_ data: Data, name: String) -> URL? {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("file-previews", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let url = folder.appendingPathComponent(name.replacingOccurrences(of: "/", with: "-"))
            try data.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }
}

/// A PDF's shown pages drawn on white for a column: each page its own width
/// in pixels (`PreviewSize.pdfDrawWidths`), at its own aspect. One drawing
/// of each file is kept (a new one replaces it unless it already covers
/// the new one), so a row scrolled back in shows it at once; a wider column
/// draws it again.
@MainActor
final class PDFPageImages {
    static let shared = PDFPageImages()

    final class Drawn: @unchecked Sendable {
        /// Each page's drawn width in pixels.
        let pixelWidths: [CGFloat]
        let images: [UIImage]

        init(pixelWidths: [CGFloat], images: [UIImage]) {
            self.pixelWidths = pixelWidths
            self.images = images
        }

        /// Every page asked for is drawn at least as wide as asked.
        func covers(_ widths: [CGFloat]) -> Bool {
            images.count >= widths.count && zip(pixelWidths, widths).allSatisfy { $0 >= $1 }
        }
    }

    private let drawn = NSCache<NSURL, Drawn>()
    private var inFlight: [String: Task<Drawn?, Never>] = [:]

    init() {
        // A row of Letter pages for a phone column is ~10–20 MB of pixels.
        drawn.countLimit = 16
        drawn.totalCostLimit = 160 * 1024 * 1024
    }

    func cached(_ file: URL) -> Drawn? { drawn.object(forKey: file as NSURL) }

    /// The first pages of `file`, page i `pixelWidths[i]` wide (as many
    /// pages as widths).
    func draw(_ file: URL, pixelWidths: [CGFloat]) async -> Drawn? {
        if let hit = cached(file), hit.covers(pixelWidths) { return hit }
        let key = "\(file.path)\u{1}" + pixelWidths.map { String(Int($0)) }.joined(separator: ",")
        let task =
            inFlight[key]
            ?? Task.detached(priority: .userInitiated) { () -> Drawn? in
                guard let document = PDFDocument(url: file) else { return nil }
                var images: [UIImage] = []
                for (index, width) in pixelWidths.enumerated() where index < document.pageCount {
                    guard let image = document.page(at: index).flatMap({ Self.image($0, pixelWidth: width) })
                    else { break }
                    images.append(image)
                }
                return images.isEmpty
                    ? nil : Drawn(pixelWidths: Array(pixelWidths.prefix(images.count)), images: images)
            }
        inFlight[key] = task
        let result = await task.value
        inFlight[key] = nil
        if let result, !(cached(file)?.covers(result.pixelWidths) ?? false) {
            let bytes = result.images.reduce(0) { $0 + Int($1.size.width * $1.size.height * $1.scale * $1.scale) * 4 }
            drawn.setObject(result, forKey: file as NSURL, cost: bytes)
        }
        return result ?? cached(file)
    }

    /// A page's size in points (its media box).
    nonisolated static func size(_ page: PDFPage) -> CGSize {
        page.bounds(for: .mediaBox).size
    }

    /// A page drawn on white, `pixelWidth` wide at its aspect.
    nonisolated static func image(_ page: PDFPage, pixelWidth: CGFloat) -> UIImage? {
        let bounds = page.bounds(for: .mediaBox)
        guard bounds.width > 0, bounds.height > 0, pixelWidth > 0 else { return nil }
        let scale = pixelWidth / bounds.width
        let size = CGSize(width: pixelWidth.rounded(), height: (bounds.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        return UIGraphicsImageRenderer(size: size, format: format).image { context in
            UIColor.white.setFill()
            context.fill(CGRect(origin: .zero, size: size))
            let cg = context.cgContext
            cg.translateBy(x: 0, y: size.height)
            cg.scaleBy(x: scale, y: -scale)
            cg.translateBy(x: -bounds.minX, y: -bounds.minY)
            page.draw(with: .mediaBox, to: cg)
        }
    }
}

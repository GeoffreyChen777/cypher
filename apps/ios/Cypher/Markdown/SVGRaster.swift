// SVG images in a reply drawn as bitmaps, for the column's width: a hidden
// WKWebView decodes the source as an <img> (an SVG image runs no scripts and
// loads nothing) and paints it onto a canvas; the transcript shows the PNG
// natively, like any image. Only the part a preview shows is painted: a
// drawing taller than 4:5 stops at the crop (PreviewSize).
//
// One web view serves every SVG, one drawing at a time, and closes after a
// minute idle. The source crosses as a callAsyncJavaScript argument, never as
// markup; the page cannot navigate or reach the network.

import SwiftUI
import UIKit
import WebKit

@MainActor
final class SVGRaster: NSObject, WKNavigationDelegate {
    static let shared = SVGRaster()

    /// How long one drawing may take, WebKit's launch included.
    static var deadline: Duration = .seconds(15)
    static var idleTimeout: Duration = .seconds(60)
    /// The widest bitmap drawn, in pixels.
    nonisolated static let maxPixelWidth: CGFloat = 4096

    struct Key: Hashable {
        var source: String
        /// The bitmap's size in pixels: the column's width, and the shown
        /// part's height.
        var pixels: CGSize
        /// The drawing's whole height in pixels (more than `pixels.height`
        /// when it is cropped).
        var contentHeight: CGFloat
    }

    private let drawn = NSCache<NSString, UIImage>()
    /// The widest bitmap of each source, standing in while a wider one draws.
    private var widest: [String: (width: CGFloat, image: UIImage)] = [:]
    private var failed: Set<String> = []
    private var tail: Task<Void, Never>?
    private var webView: WKWebView?
    private var loading: [CheckedContinuation<Bool, Never>] = []
    private var loaded = false
    private var idle: Task<Void, Never>?

    override init() {
        super.init()
        drawn.countLimit = 24
    }

    private static func cacheKey(_ key: Key) -> NSString {
        "\(key.source.hashValue)\u{1}\(Int(key.pixels.width))\u{1}\(Int(key.pixels.height))" as NSString
    }

    /// A drawing of `source` already made: for `key` exactly, else the
    /// widest one so far.
    func cached(_ key: Key) -> UIImage? {
        drawn.object(forKey: Self.cacheKey(key)) ?? widest[key.source]?.image
    }

    func hasFailed(_ source: String) -> Bool { failed.contains(source) }

    /// The size, in pixels, `source` of natural size `natural` draws at for a
    /// column `column` points wide at `scale`; nil when it shows nothing.
    nonisolated static func key(source: String, natural: CGSize, column: CGFloat, scale: CGFloat) -> Key? {
        let size = PreviewSize.of(column: column, natural: natural)
        guard size.width > 0 else { return nil }
        let pixelScale = min(scale, maxPixelWidth / size.width)
        return Key(
            source: source,
            pixels: CGSize(width: (size.width * pixelScale).rounded(), height: (size.height * pixelScale).rounded()),
            contentHeight: (size.contentHeight * pixelScale).rounded())
    }

    /// Draw `key`, after the drawings asked for before it.
    func draw(_ key: Key) async -> UIImage? {
        if let hit = drawn.object(forKey: Self.cacheKey(key)) { return hit }
        let previous = tail
        let task = Task { @MainActor [weak self] () -> UIImage? in
            await previous?.value
            return await self?.paint(key)
        }
        tail = Task { _ = await task.value }
        let image = await task.value
        if let image {
            drawn.setObject(image, forKey: Self.cacheKey(key))
            if (widest[key.source]?.width ?? 0) < key.pixels.width {
                widest[key.source] = (key.pixels.width, image)
            }
            if widest.count > 48 { widest.removeAll() }
        } else {
            failed.insert(key.source)
        }
        return image
    }

    private func paint(_ key: Key) async -> UIImage? {
        idle?.cancel()
        defer { scheduleIdleClose() }
        guard let webView = await ready() else { return nil }
        let arguments: [String: Any] = [
            "svg": Data(key.source.utf8).base64EncodedString(),
            "w": Double(key.pixels.width), "h": Double(key.pixels.height), "full": Double(key.contentHeight),
        ]
        let script = """
            const img = new Image();
            img.src = 'data:image/svg+xml;base64,' + svg;
            await img.decode();
            const canvas = document.createElement('canvas');
            canvas.width = w;
            canvas.height = h;
            canvas.getContext('2d').drawImage(img, 0, 0, w, full);
            return canvas.toDataURL('image/png');
            """
        let result: String?? = await withDeadline(Self.deadline) {
            (try? await webView.callAsyncJavaScript(
                script, arguments: arguments, in: nil, contentWorld: .defaultClient)) as? String
        }
        guard case .some(.some(let url)) = result, let comma = url.firstIndex(of: ","),
            let png = Data(base64Encoded: String(url[url.index(after: comma)...])),
            let raster = UIImage(data: png)?.cgImage
        else {
            if result == nil { close() }
            return nil
        }
        return UIImage(cgImage: raster)
    }

    // MARK: Web view

    private func ready() async -> WKWebView? {
        if loaded, let webView { return webView }
        if webView == nil {
            let configuration = WKWebViewConfiguration()
            configuration.websiteDataStore = .nonPersistent()
            configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
            let view = WKWebView(frame: CGRect(x: 0, y: 0, width: 64, height: 64), configuration: configuration)
            view.navigationDelegate = self
            webView = view
            view.loadHTMLString("<!doctype html><html><body></body></html>", baseURL: nil)
        }
        let ok = await withCheckedContinuation { loading.append($0) }
        return ok ? webView : nil
    }

    private func finishLoading(_ ok: Bool) {
        loaded = ok
        let waiting = loading
        loading = []
        waiting.forEach { $0.resume(returning: ok) }
        if !ok { close() }
    }

    private func scheduleIdleClose() {
        idle?.cancel()
        idle = Task { [weak self] in
            try? await Task.sleep(for: Self.idleTimeout)
            guard !Task.isCancelled else { return }
            self?.close()
        }
    }

    private func close() {
        webView?.navigationDelegate = nil
        webView?.stopLoading()
        webView = nil
        loaded = false
        if !loading.isEmpty { finishLoading(false) }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if webView === self.webView { finishLoading(true) }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if webView === self.webView { finishLoading(false) }
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error
    ) {
        if webView === self.webView { finishLoading(false) }
    }

    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction
    ) async -> WKNavigationActionPolicy {
        // Only the blank page it starts on.
        navigationAction.request.url?.absoluteString == "about:blank" ? .allow : .cancel
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if webView === self.webView { close() }
    }
}

/// An SVG image block: drawn for the column's width (cropped to 4:5 from the
/// top like any image), a tap opening the page; `fallback` when it has no
/// size or can't be drawn.
struct SVGPreview<Fallback: View>: View {
    let text: String
    let natural: CGSize
    let title: String
    let open: () -> Void
    @ViewBuilder let fallback: () -> Fallback

    @Environment(\.displayScale) private var displayScale
    @State private var width: CGFloat = 0
    @State private var image: UIImage?
    @State private var failed = false

    private var key: SVGRaster.Key? {
        SVGRaster.key(source: text, natural: natural, column: width, scale: displayScale)
    }

    var body: some View {
        if natural.width <= 0 || natural.height <= 0 || failed || SVGRaster.shared.hasFailed(text) {
            fallback()
        } else {
            let shown = image ?? key.flatMap { SVGRaster.shared.cached($0) }
            Button(action: open) {
                // The bitmap is only the shown part: it fills the frame.
                PreviewFrame(natural: natural) { Color.clear }
                    .overlay(alignment: .top) {
                        if let shown {
                            Image(uiImage: shown)
                                .resizable()
                                .interpolation(.high)
                        } else {
                            FileReferenceChip.wash.swiftUI
                        }
                    }
                    .previewPicture()
            }
            .buttonStyle(.plain)
            .onGeometryChange(for: CGFloat.self) {
                $0.size.width
            } action: {
                width = $0
            }
            .task(id: key) {
                guard let key else { return }
                let drawn = await SVGRaster.shared.draw(key)
                guard !Task.isCancelled else { return }
                if let drawn { image = drawn } else { failed = true }
            }
            .accessibilityLabel(title)
            .accessibilityHint("Opens the image")
            .accessibilityIdentifier("inline-image-svg")
        }
    }
}

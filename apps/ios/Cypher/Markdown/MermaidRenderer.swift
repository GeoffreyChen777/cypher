// ```mermaid fences drawn as diagrams — the iOS side of
// crates/ui/src/markdown/mermaid.rs.
//
// Mermaid (bundled offline in MermaidRenderer.bundle, see
// apps/ios/MermaidRenderer) lays the source out in a hidden WKWebView, which
// measures label text in the font it draws it in, and hands back a PNG. The
// transcript shows that image natively, so a diagram scrolls, sizes and
// caches like any other block, and no web view sits in the transcript.
//
// One web view serves every diagram, one render at a time (Mermaid's
// configuration is global), and closes after a minute idle. The source crosses
// as a callAsyncJavaScript argument, never as markup; the page cannot reach
// the network, navigate, or call back into the app.

import SwiftUI
import UIKit
import WebKit

/// Fence tags drawn as a diagram.
func isMermaidFence(_ language: String?) -> Bool {
    guard let tag = language?.lowercased() else { return false }
    return tag == "mermaid" || tag == "mmd"
}

/// The chat palette a diagram is drawn in, as opaque `#rrggbb` per role: the
/// code-block panel's fill is the canvas, nodes are the ink washes chat chrome
/// uses (desktop `Palette::of`).
struct MermaidPalette: Hashable, Sendable {
    let dark: Bool
    let roles: [String: String]
    let series: [String]

    static func of(dark: Bool) -> MermaidPalette {
        let traits = UITraitCollection(userInterfaceStyle: dark ? .dark : .light)
        func rgba(_ color: Color) -> (CGFloat, CGFloat, CGFloat, CGFloat) {
            var (r, g, b, a): (CGFloat, CGFloat, CGFloat, CGFloat) = (0, 0, 0, 0)
            UIColor(color).resolvedColor(with: traits).getRed(&r, green: &g, blue: &b, alpha: &a)
            return (r, g, b, a)
        }
        func over(_ base: (CGFloat, CGFloat, CGFloat), _ color: Color, opacity: CGFloat = 1)
            -> (CGFloat, CGFloat, CGFloat)
        {
            let (r, g, b, a) = rgba(color)
            let alpha = a * opacity
            return (
                base.0 + (r - base.0) * alpha, base.1 + (g - base.1) * alpha,
                base.2 + (b - base.2) * alpha
            )
        }
        func hex(_ c: (CGFloat, CGFloat, CGFloat)) -> String {
            let channel = { (v: CGFloat) in Int((min(max(v, 0), 1) * 255).rounded()) }
            return String(format: "#%02x%02x%02x", channel(c.0), channel(c.1), channel(c.2))
        }
        let (r, g, b, _) = rgba(Theme.bg)
        let canvas = over((r, g, b), whiteAlpha(0.035))
        let border = hex(over(canvas, Theme.borderStrong))
        let roles: [String: String] = [
            "canvas": hex(canvas),
            "surface": hex(over(canvas, whiteAlpha(0.07))),
            "surfaceAlt": hex(over(canvas, whiteAlpha(0.12))),
            "surfaceMuted": hex(over(canvas, whiteAlpha(0.035))),
            "text": hex(over(canvas, Theme.text)),
            "textMuted": hex(over(canvas, Theme.textMuted)),
            "border": border,
            "line": hex(over(canvas, Theme.textMuted)),
            "noteBackground": hex(over(canvas, Theme.warning, opacity: 0.14)),
            "noteBorder": hex(over(canvas, Theme.warning, opacity: 0.55)),
            "danger": hex(over(canvas, Theme.danger)),
            "warning": hex(over(canvas, Theme.warning)),
            "success": hex(over(canvas, Theme.success)),
        ]
        let series = [
            Theme.accent, Theme.success, Theme.warning, Theme.statusWorking,
            Theme.inlineCodeText, Theme.danger,
        ].map { hex(over(canvas, $0)) }
        return MermaidPalette(dark: dark, roles: roles, series: series)
    }

    var arguments: [String: Any] {
        var value: [String: Any] = roles
        value["dark"] = dark
        value["series"] = series
        return value
    }
}

/// What a fence shows once its render settled.
enum MermaidOutcome {
    /// Drawn, with its natural size in points.
    case image(UIImage, CGSize)
    /// Mermaid could not draw it; the fence stays code with this reason.
    case failed(String)
}

/// Which diagrams the reader switched to their source, by source text: every
/// copy of the same diagram follows (desktop `SHOWN_AS_SOURCE`).
@MainActor
@Observable
final class MermaidSourceToggles {
    static let shared = MermaidSourceToggles()
    private(set) var sources: Set<String> = []

    func showsSource(_ source: String) -> Bool { sources.contains(source) }

    func set(_ source: String, showsSource: Bool) {
        if showsSource {
            sources.insert(source)
        } else {
            sources.remove(source)
        }
    }
}

@MainActor
final class MermaidRenderer: NSObject, WKNavigationDelegate {
    static let shared = MermaidRenderer()

    /// How long one render may take, launching WebKit's processes included.
    /// Tests raise it: a cold WebContent launch has taken 23s on a loaded CI
    /// runner (WorkspaceDiffWebView.renderDeadline).
    static var renderDeadline: Duration = .seconds(15)
    /// The web view closes after this long without a render.
    static var idleTimeout: Duration = .seconds(60)
    /// Settled diagrams kept, least recently used evicted first.
    static let maxCached = 48

    struct Key: Hashable {
        var source: String
        var palette: MermaidPalette
        var scale: CGFloat
    }

    static var resourceURL: URL? {
        Bundle.main.url(forResource: "MermaidRenderer", withExtension: "bundle")?
            .appendingPathComponent("index.html")
    }

    private var cache: [Key: (outcome: MermaidOutcome, used: UInt64)] = [:]
    private var clock: UInt64 = 0
    private var inflight: [Key: Task<MermaidOutcome, Never>] = [:]
    /// The end of the render queue: each render waits for the one before.
    private var tail: Task<Void, Never>?
    private var webView: WKWebView?
    private var loading: [CheckedContinuation<Bool, Never>] = []
    private var loaded = false
    private var idle: Task<Void, Never>?

    /// A settled outcome for `key`, if there is one. A drawing of the same
    /// source in another palette or at another scale stands in, so an
    /// appearance switch never flashes the source.
    func cached(_ key: Key) -> MermaidOutcome? {
        if let entry = cache[key] {
            clock += 1
            cache[key]?.used = clock
            return entry.outcome
        }
        return cache.first { other, entry in
            guard other.source == key.source, case .image = entry.outcome else { return false }
            return true
        }?.value.outcome
    }

    /// Whether `key` has its own settled outcome (not a stand-in).
    func isSettled(_ key: Key) -> Bool { cache[key] != nil }

    /// Draw `key`, or join the render already underway.
    func render(_ key: Key) async -> MermaidOutcome {
        if let entry = cache[key] {
            clock += 1
            cache[key]?.used = clock
            return entry.outcome
        }
        if let running = inflight[key] { return await running.value }
        let previous = tail
        let task = Task { @MainActor [weak self] () -> MermaidOutcome in
            await previous?.value
            guard let self else { return .failed("The diagram renderer closed") }
            return await self.draw(key)
        }
        inflight[key] = task
        tail = Task { _ = await task.value }
        let outcome = await task.value
        inflight[key] = nil
        store(key, outcome)
        return outcome
    }

    private func store(_ key: Key, _ outcome: MermaidOutcome) {
        clock += 1
        cache[key] = (outcome, clock)
        guard cache.count > Self.maxCached else { return }
        let ages = cache.values.map(\.used).sorted()
        let cutoff = ages[cache.count - Self.maxCached - 1]
        cache = cache.filter { $0.value.used > cutoff }
    }

    private func draw(_ key: Key) async -> MermaidOutcome {
        idle?.cancel()
        defer { scheduleIdleClose() }
        guard let webView = await ready() else { return .failed("The diagram renderer did not start") }
        let input: [String: Any] = [
            "source": key.source, "palette": key.palette.arguments, "scale": Double(key.scale),
        ]
        let result = await withDeadline(Self.renderDeadline) {
            try? await webView.callAsyncJavaScript(
                "return await window.cypherMermaid.render(input)",
                arguments: ["input": input], in: nil, contentWorld: .page)
        }
        guard case .some(.some(let value)) = result else {
            // Timed out or the page failed: start the next render fresh.
            close()
            return .failed(result == nil ? "Drawing the diagram timed out" : "The diagram renderer failed")
        }
        return Self.outcome(of: value, scale: key.scale)
    }

    nonisolated static func outcome(of value: Any, scale: CGFloat) -> MermaidOutcome {
        guard let body = value as? [String: Any] else { return .failed("The diagram renderer failed") }
        if let error = body["error"] as? String { return .failed(error) }
        guard let png = (body["png"] as? String).flatMap({ Data(base64Encoded: $0) }),
            let width = body["width"] as? Double, let height = body["height"] as? Double,
            width > 0, height > 0, let raster = UIImage(data: png)
        else { return .failed("The diagram renderer failed") }
        // Pixels per point: the device scale, unless the page capped it.
        let pixelScale = max(raster.size.width / width, 0.1)
        let image = raster.cgImage.map { UIImage(cgImage: $0, scale: pixelScale, orientation: .up) } ?? raster
        return .image(image, CGSize(width: width, height: height))
    }

    // MARK: Web view

    private func ready() async -> WKWebView? {
        if loaded, let webView { return webView }
        if webView == nil {
            guard let url = Self.resourceURL else { return nil }
            let configuration = WKWebViewConfiguration()
            configuration.websiteDataStore = .nonPersistent()
            configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
            configuration.suppressesIncrementalRendering = true
            let view = WKWebView(frame: CGRect(x: 0, y: 0, width: 1024, height: 768), configuration: configuration)
            view.navigationDelegate = self
            webView = view
            view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
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
            try? await Task.sleep(for: MermaidRenderer.idleTimeout)
            guard !Task.isCancelled, let self, self.inflight.isEmpty else { return }
            self.close()
        }
    }

    /// Drop the web view; the next render starts a fresh one. Navigating away
    /// releases the page's scratch DOM even if WebKit keeps the process.
    private func close() {
        guard let view = webView else { return }
        webView = nil
        loaded = false
        view.navigationDelegate = nil
        view.stopLoading()
        view.loadHTMLString("", baseURL: nil)
        let waiting = loading
        loading = []
        waiting.forEach { $0.resume(returning: false) }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard webView === self.webView else { return }
        finishLoading(true)
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        guard webView === self.webView else { return }
        finishLoading(false)
    }
    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error
    ) {
        guard webView === self.webView else { return }
        finishLoading(false)
    }
    func webView(
        _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        let allowed =
            navigationAction.targetFrame?.isMainFrame == true
            && navigationAction.navigationType == .other
            && navigationAction.request.url == Self.resourceURL
        decisionHandler(allowed ? .allow : .cancel)
    }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        guard webView === self.webView else { return }
        close()
    }
}

/// `work`'s result, or nil when `limit` passes first. The work is not
/// cancelled (a web view call cannot be); its late result is dropped.
@MainActor
private func withDeadline<T>(_ limit: Duration, _ work: @escaping @MainActor () async -> T) async -> T? {
    let once = ResumeOnce<T?>()
    return await withCheckedContinuation { continuation in
        once.continuation = continuation
        Task { @MainActor in once.resume(await work()) }
        Task { @MainActor in
            try? await Task.sleep(for: limit)
            once.resume(nil)
        }
    }
}

/// A continuation resumed by whichever caller comes first.
@MainActor
private final class ResumeOnce<Value> {
    var continuation: CheckedContinuation<Value, Never>?
    func resume(_ value: sending Value) {
        continuation?.resume(returning: value)
        continuation = nil
    }
}

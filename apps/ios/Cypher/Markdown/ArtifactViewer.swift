// Artifact viewer — the desktop's Artifact surface (crates/ui/src/artifacts.rs)
// as a full-screen page: the artifact live in a web view, its source a menu
// pick away, and Copy. A named page follows its revisions (ArtifactShelf):
// one landing while it is open reloads it in place.
//
// Pages load as text with no base URL into a non-persistent store, so they
// can't read the app's files or keep state between openings; scripts and
// stylesheets from the web load, and clicked web links open in Safari.

import SwiftUI
import UIKit
import WebKit

struct ArtifactViewer: View {
    private let opened: ArtifactPage
    /// The chat's named pages; nil shows `opened` as it was.
    private let shelf: ArtifactShelf?
    @Environment(\.dismiss) private var dismiss
    @State private var source: Bool

    init(page: ArtifactPage, shelf: ArtifactShelf? = nil) {
        self.opened = page
        self.shelf = shelf
        _source = State(initialValue: page.showsSource)
    }

    /// The page's newest version, read from the shelf so a revision
    /// re-renders it.
    private var page: ArtifactPage { shelf?.latest(opened) ?? opened }

    var body: some View {
        let page = self.page
        NavigationStack {
            ZStack {
                // Kept under the source so going back finds the page as it was.
                ArtifactWebView(html: Self.document(for: page))
                    .ignoresSafeArea(edges: .bottom)
                    .accessibilityHidden(source)
                if source {
                    ScrollView(.vertical) {
                        HighlightedCodeView(language: nil, code: page.code)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .background(Theme.bg.ignoresSafeArea())
                    .accessibilityIdentifier("artifact-source")
                }
            }
            .background(Theme.bg.ignoresSafeArea())
            .navigationTitle(page.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Close", systemImage: "xmark") { dismiss() }
                        .accessibilityIdentifier("artifact-close")
                }
                ToolbarItem(placement: .primaryAction) {
                    Menu {
                        Picker("View", selection: $source) {
                            Label("Preview", systemImage: "safari").tag(false)
                            Label("Source", systemImage: "chevron.left.forwardslash.chevron.right").tag(true)
                        }
                        Button("Copy source", systemImage: "doc.on.doc") {
                            UIPasteboard.general.string = page.code
                        }
                    } label: {
                        Image(systemName: source ? "chevron.left.forwardslash.chevron.right" : "safari")
                            .font(.system(size: 17, weight: .regular))
                            .frame(width: 22, height: 22)
                    }
                    .accessibilityLabel("Page options")
                    .accessibilityValue(source ? "Source" : "Preview")
                    .accessibilityIdentifier("artifact-options")
                }
            }
        }
    }

    /// The document the web view loads: HTML as written, an SVG centered on
    /// a page of its own with room around it (artifacts.rs `page_html`).
    static func document(for page: ArtifactPage) -> String {
        switch page.kind {
        case .html:
            return page.code
        case .svg:
            return """
                <!doctype html><html><head><meta charset="utf-8">\
                <meta name="viewport" content="width=device-width,initial-scale=1">\
                <style>html,body{margin:0;height:100%}\
                body{box-sizing:border-box;padding:24px;\
                display:flex;align-items:center;justify-content:center}\
                svg{max-width:100%;max-height:100%}</style></head><body>\(page.code)</body></html>
                """
        }
    }
}

/// One artifact page in a web view: loaded once, reloaded if WebKit's
/// content process dies.
struct ArtifactWebView: UIViewRepresentable {
    let html: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        // Pages assume a browser's white until they paint their own.
        view.backgroundColor = .white
        view.allowsLinkPreview = false
        view.accessibilityIdentifier = "artifact-web"
        view.navigationDelegate = context.coordinator
        context.coordinator.load(html, into: view)
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        context.coordinator.load(html, into: view)
    }

    static func dismantleUIView(_ view: WKWebView, coordinator: Coordinator) {
        view.navigationDelegate = nil
        view.stopLoading()
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate {
        private var loaded: String?

        func load(_ html: String, into view: WKWebView) {
            guard html != loaded else { return }
            loaded = html
            view.loadHTMLString(html, baseURL: nil)
        }

        /// Web links the user taps go to Safari; everything else (the page
        /// itself, anchors, scripts) stays in the view.
        func webView(
            _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
        ) {
            let url = navigationAction.request.url
            let scheme = url?.scheme?.lowercased() ?? ""
            let external =
                ["http", "https", "mailto"].contains(scheme)
                && (navigationAction.navigationType == .linkActivated
                    || navigationAction.targetFrame == nil)
            if external, let url {
                UIApplication.shared.open(url)
            }
            decisionHandler(external ? .cancel : .allow)
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            guard let html = loaded else { return }
            webView.loadHTMLString(html, baseURL: nil)
        }
    }
}

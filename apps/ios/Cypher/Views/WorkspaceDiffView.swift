import SwiftUI
import WebKit

struct WorkspaceDiffView: View {
    let path: String
    let patch: String
    let partial: Bool
    var entries: [WorkspaceDiffEntry]? = nil
    var loadSources: ((String) async throws -> WorkspaceDiffSources)? = nil
    @Environment(\.colorScheme) private var colorScheme
    @State private var split = false
    @State private var status: DiffRendererStatus = .loading
    @State private var revision = 0

    var body: some View {
        VStack(spacing: 0) {
            if partial {
                Label("Diff may be incomplete", systemImage: "exclamationmark.triangle")
                    .font(Theme.sans(11)).foregroundStyle(Theme.warning)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(Theme.warning.opacity(0.06))
            }
            ZStack {
                let oversized = entries == nil && patch.utf8.count > 512 * 1024
                if !oversized && status != .failed && status != .tooLarge {
                    WorkspaceDiffWebView(path: path, patch: patch, split: split,
                                         dark: colorScheme == .dark,
                                         entries: entries, loadSources: loadSources) { status = $0 }
                        .id(revision)
                }
                if !oversized && status == .loading {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Theme.bg)
                } else if oversized || status == .failed || status == .tooLarge {
                    ContentUnavailableView {
                        Label(oversized || status == .tooLarge ? "Diff too large" : "Diff unavailable",
                              systemImage: "doc.text")
                    } actions: {
                        if status == .failed { Button("Retry") { status = .loading; revision += 1 } }
                        NavigationLink("Raw patch") {
                            WorkspaceDocumentView(title: path, text: patch, partial: partial)
                        }
                    }
                    .background(Theme.bg)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bg)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Picker("Layout", selection: $split) {
                        Label("Unified", systemImage: "rectangle").tag(false)
                        Label("Side by side", systemImage: "rectangle.split.2x1").tag(true)
                    }
                    Button("Copy patch", systemImage: "doc.on.doc") {
                        UIPasteboard.general.string = patch
                    }
                    NavigationLink {
                        WorkspaceDocumentView(title: path, text: patch, partial: partial)
                    } label: {
                        Label("Raw patch", systemImage: "doc.plaintext")
                    }
                } label: {
                    Image(systemName: split ? "rectangle.split.2x1" : "rectangle")
                        .font(.system(size: 18, weight: .regular))
                        .frame(width: 22, height: 22)
                }
                .accessibilityLabel("Diff options")
                .accessibilityValue(split ? "Side by side" : "Unified")
                .accessibilityIdentifier("workspace-diff-options")
            }
        }
    }
}

enum DiffRendererStatus: Equatable {
    case loading, rendered, failed, tooLarge
}

struct WorkspaceSourceDocument: Equatable {
    let text: String
    var wrap: Bool = false
}

/// The only document this WebView may navigate to is our bundled shell.
/// Repository text crosses as JS arguments, never HTML or executable source.
struct WorkspaceDiffWebView: UIViewRepresentable {
    let path: String
    let patch: String
    let split: Bool
    let dark: Bool
    var entries: [WorkspaceDiffEntry]? = nil
    var loadSources: ((String) async throws -> WorkspaceDiffSources)? = nil
    var source: WorkspaceSourceDocument? = nil
    var onStatus: (DiffRendererStatus) -> Void

    static var resourceURL: URL? {
        Bundle.main.url(forResource: "DiffRenderer", withExtension: "bundle")?
            .appendingPathComponent("index.html")
    }

    func makeCoordinator() -> Coordinator { Coordinator(onStatus: onStatus) }
    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.userContentController.add(context.coordinator, name: "diffRenderer")
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.isOpaque = false
        view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.allowsLinkPreview = false
        view.accessibilityIdentifier = source == nil ? "workspace-diff-web" : "workspace-source-web"
        view.navigationDelegate = context.coordinator
        context.coordinator.webView = view
        if let url = Self.resourceURL {
            view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
        } else {
            Task { @MainActor in context.coordinator.report(.failed) }
        }
        return view
    }
    func updateUIView(_ view: WKWebView, context: Context) {
        context.coordinator.onStatus = onStatus
        context.coordinator.loadSources = source == nil ? loadSources : nil
        context.coordinator.update(path: path, patch: patch, split: split, dark: dark, entries: entries, source: source)
    }
    static func dismantleUIView(_ view: WKWebView, coordinator: Coordinator) {
        coordinator.stop()
        view.configuration.userContentController.removeScriptMessageHandler(forName: "diffRenderer")
        view.navigationDelegate = nil
        view.stopLoading()
        // Navigation disposes the old document's worker and releases its code,
        // AST and selection state even when WebKit retains a process for reuse.
        view.loadHTMLString("", baseURL: nil)
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        weak var webView: WKWebView?
        var onStatus: (DiffRendererStatus) -> Void
        var loadSources: ((String) async throws -> WorkspaceDiffSources)?
        private var ready = false
        private var stopped = false
        private var pending: Input?
        private var activeID = ""
        private var sentID = ""
        private var deadline: Task<Void, Never>?
        private var sourceTasks: [String: Task<Void, Never>] = [:]

        struct Input: Equatable {
            var path: String
            var patch: String
            var split: Bool
            var dark: Bool
            var entries: [WorkspaceDiffEntry]?
            var source: WorkspaceSourceDocument?
        }
        init(onStatus: @escaping (DiffRendererStatus) -> Void) { self.onStatus = onStatus }

        func update(path: String, patch: String, split: Bool, dark: Bool,
                    entries: [WorkspaceDiffEntry]? = nil, source: WorkspaceSourceDocument? = nil) {
            let input = Input(path: path, patch: entries == nil && source == nil ? patch : "",
                              split: split, dark: dark, entries: entries, source: source)
            guard pending != input, !stopped else { return }
            cancelSourceTasks()
            pending = input
            activeID = UUID().uuidString
            let id = activeID
            deadline?.cancel()
            deadline = Task { [weak self] in
                try? await Task.sleep(for: .seconds(15))
                guard !Task.isCancelled, let self, self.activeID == id else { return }
                self.report(.failed)
            }
            // Do not publish SwiftUI state during updateUIView.
            Task { [weak self] in
                guard let self, !self.stopped, self.activeID == id else { return }
                self.onStatus(.loading)
                self.renderIfReady()
            }
        }

        private func renderIfReady() {
            guard ready, !stopped, let pending, let webView else { return }
            let id = activeID
            guard sentID != id else { return }
            sentID = id
            var payload: [String: Any] = [
                "id": id, "path": pending.path, "patch": pending.patch,
                "split": pending.split, "dark": pending.dark,
                "canLoadContext": loadSources != nil,
            ]
            if let entries = pending.entries {
                payload["files"] = entries.prefix(250).map(\.arguments)
                payload["omittedFiles"] = max(0, entries.count - 250)
            }
            if let source = pending.source {
                payload["sourceText"] = source.text
                payload["wrap"] = source.wrap
                payload["canLoadContext"] = false
            }
            let arguments: [String: Any] = ["input": payload]
            webView.callAsyncJavaScript("await window.cypherDiff.render(input)",
                                       arguments: arguments, in: nil, in: .page) { [weak self] result in
                guard let self, !self.stopped, self.activeID == id else { return }
                if case .failure = result { self.report(.failed) }
            }
        }
        func report(_ status: DiffRendererStatus) {
            guard !stopped else { return }
            if status != .loading { deadline?.cancel() }
            if status == .failed || status == .tooLarge {
                // Ignore late ready/rendered callbacks while SwiftUI removes
                // the failed WebView and its worker from the hierarchy.
                activeID = ""
                sentID = ""
            }
            onStatus(status)
        }
        func stop() {
            stopped = true
            activeID = ""
            pending = nil
            deadline?.cancel()
            deadline = nil
            cancelSourceTasks()
            loadSources = nil
        }

        private func cancelSourceTasks() {
            sourceTasks.values.forEach { $0.cancel() }
            sourceTasks.removeAll()
        }

        /// JS supplies an index, never a filesystem path or an RPC method.
        private func requestContext(_ body: [String: String]) {
            guard let token = body["token"], !token.isEmpty, token.utf8.count <= 64, sourceTasks[token] == nil,
                  let index = body["file"].flatMap(Int.init), let pending, pending.source == nil else { return }
            let paths = pending.entries?.prefix(250).map(\.path) ?? [pending.path]
            guard paths.indices.contains(index) else { return }
            guard let loadSources, sourceTasks.count < 2 else {
                respondContext(token: token, code: "unavailable")
                return
            }
            let id = activeID
            sourceTasks[token] = Task { [weak self] in
                do {
                    let sources = try await loadSources(paths[index])
                    guard let self, !Task.isCancelled, !self.stopped, self.activeID == id else { return }
                    self.respondContext(token: token, sources: sources)
                    self.sourceTasks[token] = nil
                } catch {
                    guard let self, !Task.isCancelled, !self.stopped, self.activeID == id else { return }
                    let code: String
                    if case .stale? = error as? WorkspaceDiffContextError { code = "stale" }
                    else { code = "unavailable" }
                    self.respondContext(token: token, code: code)
                    self.sourceTasks[token] = nil
                }
            }
        }
        private func respondContext(token: String, code: String? = nil, sources: WorkspaceDiffSources? = nil) {
            guard !stopped else { return }
            var value: [String: Any] = ["id": activeID, "token": token]
            if let code { value["error"] = code }
            if let sources {
                value["oldText"] = sources.oldText.map { $0 as Any } ?? NSNull()
                value["newText"] = sources.newText.map { $0 as Any } ?? NSNull()
            }
            webView?.callAsyncJavaScript("window.cypherDiff.contextResult(value)",
                arguments: ["value": value], in: nil, in: .page, completionHandler: nil)
        }
        func userContentController(_ userContentController: WKUserContentController,
                                   didReceive message: WKScriptMessage) {
            guard !stopped, message.frameInfo.isMainFrame,
                  message.frameInfo.request.url == WorkspaceDiffWebView.resourceURL,
                  let body = message.body as? [String: String], let event = body["event"] else { return }
            if event == "ready" {
                ready = true
                renderIfReady()
            } else if body["id"] == activeID {
                switch event {
                case "rendered": report(.rendered)
                case "large": report(.tooLarge)
                case "error": report(.failed)
                case "context": requestContext(body)
                default: break
                }
            }
        }
        func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                     decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            let allowed = navigationAction.targetFrame?.isMainFrame == true
                && navigationAction.navigationType == .other
                && navigationAction.request.url == WorkspaceDiffWebView.resourceURL
            decisionHandler(allowed ? .allow : .cancel)
        }
        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            guard (error as NSError).code != NSURLErrorCancelled else { return }
            report(.failed)
        }
        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                     withError error: Error) {
            guard (error as NSError).code != NSURLErrorCancelled else { return }
            report(.failed)
        }
        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            ready = false
            report(.failed)
        }
    }
}

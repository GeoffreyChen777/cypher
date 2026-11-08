import XCTest
import SwiftUI
import WebKit
@testable import Cypher

@MainActor
final class WorkspaceDiffWebTests: XCTestCase {
    private let patch = """
        diff --git a/Example.swift b/Example.swift
        --- a/Example.swift
        +++ b/Example.swift
        @@ -1,2 +1,2 @@
        -let value = 1
        +let value = 2
         let end = true

        """

    /// Every wait below outlasts a cold WebKit process launch, which has taken
    /// 23s on a loaded CI runner. The view's own deadline must too, or it
    /// gives up on a render the test is still waiting for.
    private static let launchBudget: Duration = .seconds(60)
    private var savedDeadline = WorkspaceDiffWebView.renderDeadline

    override func setUp() async throws {
        savedDeadline = WorkspaceDiffWebView.renderDeadline
        WorkspaceDiffWebView.renderDeadline = Self.launchBudget
    }

    override func tearDown() async throws {
        WorkspaceDiffWebView.renderDeadline = savedDeadline
    }

    /// Polls `script` until it returns true. The renderer reports `rendered`
    /// once its rows are mounted; their highlighting, layout and scroll
    /// position settle over the frames after that.
    private func eventually(
        _ web: WKWebView, _ script: String, timeout: Duration = .seconds(5)
    ) async throws -> Bool {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if (try await web.evaluateJavaScript(script)) as? Bool == true { return true }
            try await Task.sleep(for: .milliseconds(50))
        }
        return false
    }

    @MainActor
    private final class Fixture {
        let window: UIWindow
        let previous: UIWindow?
        let host: UIHostingController<WorkspaceDiffWebView>
        var status: DiffRendererStatus = .loading
        init(
            patch: String, entries: [WorkspaceDiffEntry]? = nil,
            loadSources: ((String) async throws -> WorkspaceDiffSources)? = nil,
            source: WorkspaceSourceDocument? = nil, focusFile: Int? = nil,
            comment: ((String) -> Void)? = nil
        ) throws {
            let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
            previous = scene.windows.first(where: \.isKeyWindow)
            window = UIWindow(windowScene: scene)
            host = UIHostingController(
                rootView: WorkspaceDiffWebView(
                    path: "Example.swift", patch: patch, split: false, dark: false,
                    entries: entries, loadSources: loadSources, source: source, focusFile: focusFile,
                    comment: comment, onStatus: { _ in }))
            host.rootView.onStatus = { [weak self] in self?.status = $0 }
            window.rootViewController = host
            window.makeKeyAndVisible()
        }
        func close() {
            window.isHidden = true
            window.rootViewController = nil
            previous?.makeKey()
        }
        func update(patch: String? = nil, split: Bool? = nil, dark: Bool? = nil, source: WorkspaceSourceDocument? = nil)
        {
            let old = host.rootView
            status = .loading
            host.rootView = WorkspaceDiffWebView(
                path: old.path, patch: patch ?? old.patch,
                split: split ?? old.split, dark: dark ?? old.dark,
                entries: old.entries, loadSources: old.loadSources, source: source ?? old.source,
                comment: old.comment, onStatus: old.onStatus
            )
        }
        func web(in view: UIView? = nil) -> WKWebView? {
            let view = view ?? host.view!
            if let web = view as? WKWebView { return web }
            return view.subviews.lazy.compactMap { self.web(in: $0) }.first
        }
        func wait() async throws -> WKWebView {
            try await settle()
            XCTAssertEqual(status, .rendered)
            return try XCTUnwrap(web())
        }
        /// Waits out `.loading` for as long as the view itself would.
        func settle() async throws {
            let deadline = ContinuousClock.now + WorkspaceDiffWebView.renderDeadline + .seconds(1)
            while status == .loading, ContinuousClock.now < deadline {
                try await Task.sleep(for: .milliseconds(100))
            }
        }
        func waitForHighlight(in web: WKWebView, file: Bool = false) async throws {
            for _ in 0..<50 {
                let count =
                    try await web.evaluateJavaScript(
                        "window.cypherDiff.inspect().\(file ? "cachedFiles" : "cachedDiffs")") as? Int
                if (count ?? 0) > 0 { return }
                try await Task.sleep(for: .milliseconds(100))
            }
            XCTFail("Worker did not produce the highlighted diff")
        }
    }

    func testOfflineWorkerRendersStructuredDiffAndSwitchesLayoutAndTheme() async throws {
        XCTAssertNotNil(WorkspaceDiffWebView.resourceURL)
        let fixture = try Fixture(patch: patch)
        defer { fixture.close() }
        let web = try await fixture.wait()
        try await fixture.waitForHighlight(in: web)
        XCTAssertFalse(web.configuration.websiteDataStore.isPersistent)
        let info = try await web.evaluateJavaScript("window.cypherDiff.inspect()") as? [String: Any]
        XCTAssertEqual(info?["worker"] as? Bool, true)
        XCTAssertEqual(info?["style"] as? String, "unified")
        let rendered =
            try await web.evaluateJavaScript(
                """
                (() => { const r = document.querySelector('diffs-container').shadowRoot;
                  return { text: r.querySelector('pre').textContent,
                           styled: r.adoptedStyleSheets.length > 0,
                           editable: r.querySelectorAll('textarea,input,[contenteditable=true]').length }; })()
                """) as? [String: Any]
        XCTAssertTrue((rendered?["text"] as? String)?.contains("let value = 2") == true)
        XCTAssertEqual(rendered?["styled"] as? Bool, true)
        XCTAssertEqual(rendered?["editable"] as? Int, 0)
        fixture.update(split: true, dark: true)
        _ = try await fixture.wait()
        let updated = try await web.evaluateJavaScript("window.cypherDiff.inspect().style")
        XCTAssertEqual(updated as? String, "split")
        let scheme = try await web.evaluateJavaScript("document.documentElement.style.colorScheme")
        let containers = try await web.evaluateJavaScript("document.querySelectorAll('diffs-container').length")
        XCTAssertEqual(scheme as? String, "dark")
        XCTAssertEqual(containers as? Int, 1)
    }

    func testRepositoryTextIsDataAndNetworkAccessIsBlocked() async throws {
        let unsafe = patch.replacingOccurrences(
            of: "+let value = 2",
            with: #"+let value = "</script><img src='https://example.invalid/x' onerror='window.PWNED=1'>""#)
        let fixture = try Fixture(patch: unsafe)
        defer { fixture.close() }
        let web = try await fixture.wait()
        let result =
            try await web.evaluateJavaScript(
                """
                (() => { const r = document.querySelector('diffs-container').shadowRoot;
                  return { injected: !!window.PWNED, elements: r.querySelectorAll('img,iframe,script').length,
                           literal: r.querySelector('pre').textContent.includes('</script><img') }; })()
                """) as? [String: Any]
        XCTAssertEqual(result?["injected"] as? Bool, false)
        XCTAssertEqual(result?["elements"] as? Int, 0)
        XCTAssertEqual(result?["literal"] as? Bool, true)
        let blocked: String? = try await withCheckedThrowingContinuation { continuation in
            web.callAsyncJavaScript(
                """
                return await new Promise(resolve => {
                  document.addEventListener('securitypolicyviolation',
                    event => resolve(event.violatedDirective), { once: true });
                  fetch('https://example.invalid/blocked').catch(() => {});
                  setTimeout(() => resolve('timeout'), 2000);
                });
                """, arguments: [:], in: nil, in: .page
            ) { continuation.resume(with: $0.map { $0 as? String }) }
        }
        XCTAssertEqual(blocked, "connect-src")
        web.load(URLRequest(url: URL(string: "https://example.invalid/navigation")!))
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(web.url, WorkspaceDiffWebView.resourceURL)
    }

    func testLargeDiffVirtualizesRowsAndCleansUpBeforeNextFile() async throws {
        let lines = (1...3_000).map { "+let value\($0) = \($0)" }.joined(separator: "\n")
        let large =
            "diff --git a/Example.swift b/Example.swift\n--- /dev/null\n+++ b/Example.swift\n@@ -0,0 +1,3000 @@\n\(lines)\n"
        let fixture = try Fixture(patch: large)
        defer { fixture.close() }
        let web = try await fixture.wait()
        try await fixture.waitForHighlight(in: web)
        let nodes =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelectorAll('*').length") as? Int
        print("[diff renderer] 3000-line fixture: \(nodes ?? -1) mounted shadow DOM elements")
        XCTAssertLessThan(try XCTUnwrap(nodes), 2_000, "Only a viewport of token spans should be mounted")
        _ = try await web.evaluateJavaScript("window.scrollTo(0, document.documentElement.scrollHeight)")
        var reachedEnd = false
        for _ in 0..<30 {
            reachedEnd =
                (try await web.evaluateJavaScript(
                    "document.querySelector('diffs-container').shadowRoot.querySelector('pre').textContent.includes('let value3000 = 3000')"
                ) as? Bool) == true
            if reachedEnd { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(reachedEnd, "Virtualized scrolling must reveal the final line, not truncate the document")
        let bottomNodes =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelectorAll('*').length") as? Int
        XCTAssertLessThan(try XCTUnwrap(bottomNodes), 2_000)
        fixture.update(patch: patch)
        _ = try await fixture.wait()
        let text =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelector('pre').textContent") as? String
        XCTAssertTrue(text?.contains("let value = 2") == true)
        XCTAssertFalse(text?.contains("value3000") == true)
    }

    func testOversizedPatchFailsExplicitlyInsteadOfParsingATruncatedHunk() async throws {
        let fixture = try Fixture(patch: "diff --git a/x b/x\n+" + String(repeating: "x", count: 512 * 1024))
        defer { fixture.close() }
        try await fixture.settle()
        XCTAssertEqual(fixture.status, .tooLarge)
    }

    func testLatestInputWinsWhileThePageAndWorkerAreStarting() async throws {
        let fixture = try Fixture(patch: patch)
        defer { fixture.close() }
        for value in 10...17 {
            fixture.update(
                patch: patch.replacingOccurrences(
                    of: "+let value = 2",
                    with: "+let value = \(value)"))
            try await Task.sleep(for: .milliseconds(20))
        }
        let web = try await fixture.wait()
        try await fixture.waitForHighlight(in: web)
        let text =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelector('pre').textContent") as? String
        XCTAssertTrue(text?.contains("let value = 17") == true)
        XCTAssertFalse(text?.contains("let value = 10") == true)
    }

    func testCitedLinesOpenSelectedAndInView() async throws {
        let text = (1...300).map { "let value\($0) = \($0)" }.joined(separator: "\n") + "\n"
        let fixture = try Fixture(patch: "", source: .init(text: text, lines: 200...202))
        defer { fixture.close() }
        let web = try await fixture.wait()
        let focus =
            try await web.evaluateJavaScript("JSON.stringify(window.cypherDiff.inspect().focusLines)") as? String
        XCTAssertEqual(focus, #"{"start":200,"end":202}"#)
        let scrolled = try await web.evaluateJavaScript("window.scrollY") as? Double
        XCTAssertGreaterThan(scrolled ?? 0, 1_000, "the cited lines are scrolled into view")
        var selected = 0
        for _ in 0..<30 {
            selected =
                (try await web.evaluateJavaScript(
                    "document.querySelector('diffs-container').shadowRoot.querySelectorAll('[data-selected-line]').length"
                ) as? Int) ?? 0
            if selected > 0 { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertGreaterThan(selected, 0)
    }

    /// Pierre's whole-file window, centred on 50-line slices, dropped every
    /// row past line 50 while the viewport's middle was still above it; the
    /// cited lines of a short file scroll it there.
    func testCitedLinesNearTheEndOfAShortFileKeepEveryRow() async throws {
        let text = (1...60).map { "let value\($0) = \($0)" }.joined(separator: "\n") + "\n"
        let fixture = try Fixture(patch: "", source: .init(text: text, lines: 44...45))
        defer { fixture.close() }
        let web = try await fixture.wait()
        let lastRow = """
            Math.max(...[...document.querySelector('diffs-container').shadowRoot
              .querySelectorAll('[data-line]')].map(row => +row.getAttribute('data-line')))
            """
        var last = 0
        for _ in 0..<30 {
            last = (try await web.evaluateJavaScript(lastRow) as? Int) ?? 0
            if last >= 60 { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertGreaterThanOrEqual(last, 60)
        let scrolled = try await web.evaluateJavaScript("window.scrollY") as? Double
        XCTAssertGreaterThan(scrolled ?? 0, 0)
    }

    func testCommentTakesTheSelectedCodeWithoutLineNumbers() async throws {
        let text = (1...30).map { "let value\($0) = \($0)" }.joined(separator: "\n") + "\n"
        var quotes: [String] = []
        let fixture = try Fixture(patch: "", source: .init(text: text), comment: { quotes.append($0) })
        defer { fixture.close() }
        let web = try await fixture.wait()
        _ = try await web.evaluateJavaScript(
            """
            (() => { const lines = [...document.querySelector('diffs-container').shadowRoot
                .querySelectorAll('[data-content] [data-line]')];
              document.getSelection().setBaseAndExtent(lines[2], 0, lines[4], lines[4].childNodes.length);
              return true; })()
            """)
        let view = try XCTUnwrap(web as? WorkspaceWebView)
        XCTAssertTrue(view.commentsEnabled)
        view.commentOnSelection?()
        for _ in 0..<20 where quotes.isEmpty { try await Task.sleep(for: .milliseconds(100)) }
        XCTAssertEqual(quotes, ["let value3 = 3\nlet value4 = 4\nlet value5 = 5"])
        let cleared = try await web.evaluateJavaScript("document.getSelection().toString()") as? String
        XCTAssertEqual(cleared, "", "the selection goes once commented on")
        // Nothing selected: no comment.
        view.commentOnSelection?()
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(quotes.count, 1)
    }

    func testARoundsFileOpensItsCard() async throws {
        let (_, snapshot, entries) = try await demo()
        let fixture = try Fixture(patch: snapshot.patch, entries: entries, focusFile: 1)
        defer { fixture.close() }
        let web = try await fixture.wait()
        let focus = try await web.evaluateJavaScript("window.cypherDiff.inspect().focusFile") as? Int
        XCTAssertEqual(focus, 1)
    }

    private func demo() async throws -> (WorkspaceBrowserSession, WorkspaceChanges, [WorkspaceDiffEntry]) {
        let model = AppModel()
        model.enterDemoMode()
        let chat = try XCTUnwrap(model.chat(id: "chat-tabs"))
        let session = WorkspaceBrowserSession(model: model, chat: chat)
        let snapshot = try await session.changes()
        let patches = snapshot.patches()
        let entries = snapshot.files.map {
            WorkspaceDiffEntry(
                path: $0.path, patch: patches[$0.path], additions: $0.additions,
                deletions: $0.deletions, binary: $0.binary)
        }
        return (session, snapshot, entries)
    }

    func testContinuousListExpandsSnapshotContextOnDemandAndReleasesItOnCollapse() async throws {
        let (session, snapshot, entries) = try await demo()
        var requested: [String] = []
        let fixture = try Fixture(
            patch: snapshot.patch, entries: entries,
            loadSources: { path in
                requested.append(path)
                return try await session.diffSources(snapshot: snapshot, path: path)
            })
        defer { fixture.close() }
        let web = try await fixture.wait()
        try await fixture.waitForHighlight(in: web)
        let count = try await web.evaluateJavaScript("document.querySelectorAll('.file-header').length")
        XCTAssertEqual(count as? Int, 2)
        XCTAssertTrue(requested.isEmpty, "Do not preload full repository files")
        _ = try await web.evaluateJavaScript(
            """
            window.webkit.messageHandlers.diffRenderer.postMessage({
              event: 'context', id: window.cypherDiff.inspect().id, token: 'bad', file: '999999'
            }); true
            """)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(requested.isEmpty, "The bridge must not authorize unknown file indices")
        let text = "document.querySelector('diffs-container').shadowRoot.querySelector('pre').textContent"
        let before = try await web.evaluateJavaScript(text) as? String
        XCTAssertFalse(before?.contains("let value2 = 2") == true)
        _ = try await web.evaluateJavaScript(
            """
            document.querySelector('diffs-container').shadowRoot.querySelector('[data-expand-button]').click()
            """)
        var expanded = false
        for _ in 0..<60 {
            expanded = ((try await web.evaluateJavaScript(text)) as? String)?.contains("let value2 = 2") == true
            if expanded { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(expanded, "Collapsed unchanged prefix should display its real source")
        XCTAssertEqual(requested, ["Sources/Example.swift"])
        _ = try await web.evaluateJavaScript(
            """
            document.querySelector('diffs-container').shadowRoot.querySelector('[data-expand-button]').click()
            """)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(requested.count, 1, "Other gaps reuse the same validated source pair")
        _ = try await web.evaluateJavaScript("document.querySelector('.file-header').click()")
        let bytes = try await web.evaluateJavaScript("window.cypherDiff.inspect().retainedSourceBytes")
        XCTAssertEqual(bytes as? Int, 0)
        let collapsed = try await web.evaluateJavaScript(
            "document.querySelector('.file-header').getAttribute('aria-expanded')")
        XCTAssertEqual(collapsed as? String, "false")
        _ = try await web.evaluateJavaScript("document.querySelector('.file-header').click()")
        try await Task.sleep(for: .milliseconds(300))
        let again = try await web.evaluateJavaScript(text) as? String
        XCTAssertFalse(again?.contains("let value2 = 2") == true, "Reopened file starts folded")
    }

    func testStaleContextLeavesTheDiffIntactAndRequestsRefresh() async throws {
        let (_, snapshot, entries) = try await demo()
        let fixture = try Fixture(
            patch: snapshot.patch, entries: entries,
            loadSources: { _ in throw WorkspaceDiffContextError.stale })
        defer { fixture.close() }
        let web = try await fixture.wait()
        _ = try await web.evaluateJavaScript(
            "document.querySelector('diffs-container').shadowRoot.querySelector('[data-expand-button]').click()")
        var notice = ""
        for _ in 0..<40 {
            notice =
                (try await web.evaluateJavaScript("document.querySelector('.context-notice').textContent")) as? String
                ?? ""
            if notice.contains("Refresh") { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(notice.contains("Refresh"))
        XCTAssertEqual(fixture.status, .rendered, "One failed expansion should not blank the whole list")
        let text =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelector('pre').textContent") as? String
        XCTAssertFalse(text?.contains("let value2 = 2") == true)
        XCTAssertTrue(text?.contains("Hello from Cypher") == true)
    }

    func testRenameOnlyCanRevealUnchangedCode() async throws {
        let patch =
            "diff --git a/Old.swift b/New.swift\nsimilarity index 100%\nrename from Old.swift\nrename to New.swift\n"
        let fixture = try Fixture(
            patch: patch,
            entries: [
                .init(path: "New.swift", patch: patch, additions: 0, deletions: 0, binary: false)
            ],
            loadSources: { path in
                XCTAssertEqual(path, "New.swift")
                return WorkspaceDiffSources(
                    diffChecksum: "rename", oldText: "let unchanged = 1\n",
                    newText: "let unchanged = 1\n", binary: false, truncated: false, stale: false)
            })
        defer { fixture.close() }
        let web = try await fixture.wait()
        _ = try await web.evaluateJavaScript("document.querySelector('.context-notice button').click()")
        var visible = false
        for _ in 0..<50 {
            visible =
                (try await web.evaluateJavaScript(
                    """
                    document.querySelector('diffs-container')?.shadowRoot?.querySelector('pre')?.textContent.includes('unchanged') ?? false
                    """)) as? Bool ?? false
            if visible { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        XCTAssertTrue(visible)
        let editable = try await web.evaluateJavaScript(
            """
            document.querySelector('diffs-container').shadowRoot.querySelectorAll('textarea,input,[contenteditable=true]').length
            """)
        XCTAssertEqual(editable as? Int, 0)
    }

    private func capture(_ web: WKWebView, _ name: String) async throws {
        let image: UIImage = try await withCheckedThrowingContinuation { continuation in
            web.takeSnapshot(with: nil) { image, error in
                if let image {
                    continuation.resume(returning: image)
                } else {
                    continuation.resume(throwing: error ?? NSError(domain: "Snapshot", code: 1))
                }
            }
        }
        let attachment = XCTAttachment(image: image)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testPresentationGeometryAndContextStates() async throws {
        let (_, snapshot, entries) = try await demo()
        let fixture = try Fixture(
            patch: snapshot.patch, entries: entries,
            loadSources: { _ in
                try await Task.sleep(for: .milliseconds(1_500))
                throw WorkspaceDiffContextError.stale
            })
        defer { fixture.close() }
        let web = try await fixture.wait()
        let geometry = """
            (() => {
              const h = document.querySelector('.file-header').getBoundingClientRect();
              const icon = document.querySelector('.file-disclosure').getBoundingClientRect();
              const symbol = document.querySelector('.file-symbol').getBoundingClientRect();
              return [h.height, icon.x + icon.width / 2, icon.y + icon.height / 2,
                symbol.y + symbol.height / 2, h.y + h.height / 2,
                document.querySelector('.file-header .file-stat').getBoundingClientRect().right,
                document.querySelector('.changes-summary .file-stat').getBoundingClientRect().right];
            })()
            """
        let expanded = try await web.evaluateJavaScript(geometry) as! [Double]
        XCTAssertEqual(expanded[2], expanded[4], accuracy: 0.5)
        XCTAssertEqual(expanded[3], expanded[4], accuracy: 0.5)
        XCTAssertEqual(expanded[5], expanded[6], accuracy: 0.5, "Summary and file counts share a trailing column")
        _ = try await web.evaluateJavaScript("document.querySelector('.file-header').click()")
        try await Task.sleep(for: .milliseconds(200))
        let collapsed = try await web.evaluateJavaScript(geometry) as! [Double]
        XCTAssertEqual(expanded, collapsed, "Rotating the same SVG must preserve its center and row height")
        let emptyHeight =
            try await web.evaluateJavaScript("document.querySelector('.file-body').getBoundingClientRect().height")
            as! Double
        XCTAssertEqual(emptyHeight, 0, "Collapsed files must not retain blank padding")
        _ = try await web.evaluateJavaScript("document.querySelector('.file-header').click()")
        // The reopened file's rows mount over the next frames.
        let expandable = try await eventually(
            web, "!!document.querySelector('diffs-container')?.shadowRoot?.querySelector('[data-expand-button]')")
        XCTAssertTrue(expandable, "The reopened file shows its folded context")
        _ = try await web.evaluateJavaScript(
            "document.querySelector('diffs-container').shadowRoot.querySelector('[data-expand-button]').click()")
        let noticeState = "document.querySelector('.context-notice').dataset.state"
        let loading = try await eventually(web, "\(noticeState) === 'loading'", timeout: .seconds(1))
        XCTAssertTrue(loading, "The notice shows while the 1.5s load is under way")
        try await capture(web, "context-loading")
        let warning = try await eventually(web, "\(noticeState) === 'warning'")
        XCTAssertTrue(warning, "A stale load leaves a warning")
        try await capture(web, "context-stale")
        let centered = try await web.evaluateJavaScript(
            """
            (() => { const n = document.querySelector('.context-notice').getBoundingClientRect();
              const i = document.querySelector('.notice-icon').getBoundingClientRect();
              return Math.abs((n.y + n.height / 2) - (i.y + i.height / 2)) < 0.5; })()
            """)
        XCTAssertEqual(centered as? Bool, true)
    }

    func testFileStatesAndLongPathsStayWithinTheViewport() async throws {
        let entries: [WorkspaceDiffEntry] = [
            .init(path: "Assets/Brand.png", patch: nil, additions: 0, deletions: 0, binary: true),
            .init(
                path: "Sources/Networking/Transport/AReallyLongConnectionCoordinator.swift",
                patch: nil, additions: 12_340, deletions: 321, binary: false),
        ]
        let fixture = try Fixture(patch: "", entries: entries)
        defer { fixture.close() }
        let web = try await fixture.wait()
        for dark in [false, true] {
            if dark {
                fixture.update(dark: true)
                _ = try await fixture.wait()
            }
            let fits = try await web.evaluateJavaScript(
                """
                [...document.querySelectorAll('.file-header')].every(h => {
                  const r = h.getBoundingClientRect(), title = h.querySelector('.file-name').getBoundingClientRect(),
                    stats = h.querySelector('.file-stat').getBoundingClientRect();
                  return r.right <= innerWidth && title.right <= stats.left;
                })
                """)
            XCTAssertEqual(fits as? Bool, true)
            let extensionVisible = try await web.evaluateJavaScript(
                """
                (() => { const suffix = document.querySelectorAll('.file-extension')[1];
                  return suffix.textContent === '.swift' && suffix.getBoundingClientRect().width >= 20; })()
                """)
            XCTAssertEqual(extensionVisible as? Bool, true, "Truncating a long filename preserves its extension")
            try await capture(web, dark ? "dark-file-states" : "light-file-states")
        }
    }

    func testSourceReaderHighlightsAndReusesWorkerAcrossWrapAndTheme() async throws {
        let source = WorkspaceBrowserSession.readerExample
        var requested = false
        let fixture = try Fixture(
            patch: "",
            loadSources: { _ in
                requested = true
                throw WorkspaceDiffContextError.unavailable
            }, source: .init(text: source))
        defer { fixture.close() }
        let web = try await fixture.wait()
        try await fixture.waitForHighlight(in: web, file: true)
        // The worker caches its tokens before the rows repaint with them.
        let painted = try await eventually(
            web,
            "document.querySelector('diffs-container').shadowRoot.querySelectorAll('[data-content] span[style]').length > 0"
        )
        XCTAssertTrue(painted, "The worker's highlighting must reach the rows")
        XCTAssertEqual(web.accessibilityIdentifier, "workspace-source-web")
        let info = try await web.evaluateJavaScript("window.cypherDiff.inspect()") as! [String: Any]
        XCTAssertEqual(info["mode"] as? String, "file")
        let detail =
            try await web.evaluateJavaScript(
                """
                (() => { const r = document.querySelector('diffs-container').shadowRoot;
                  const line = r.querySelector('[data-content] [data-line]');
                  const range = document.createRange(); range.selectNodeContents(line);
                  return { text: range.toString(), selectable: getComputedStyle(line).webkitUserSelect,
                    numbers: r.querySelectorAll('[data-column-number]').length,
                    styled: r.querySelectorAll('[data-content] span[style]').length,
                    editable: r.querySelectorAll('textarea,input,[contenteditable=true]').length,
                    changes: r.querySelectorAll('[data-line-type^="change-"]').length };
                })()
                """) as! [String: Any]
        // Native long-press/Copy is covered in UI tests; document.getSelection()
        // does not expose a native Shadow DOM selection uniformly in WebKit.
        XCTAssertTrue((detail["text"] as? String)?.contains("import Foundation") == true)
        XCTAssertEqual(detail["selectable"] as? String, "text")
        XCTAssertTrue((detail["numbers"] as? Int ?? 0) > 0)
        XCTAssertTrue((detail["styled"] as? Int ?? 0) > 0)
        XCTAssertEqual(detail["editable"] as? Int, 0)
        XCTAssertEqual(detail["changes"] as? Int, 0)
        _ = try await web.evaluateJavaScript(
            """
            window.webkit.messageHandlers.diffRenderer.postMessage({
              event:'context', id:window.cypherDiff.inspect().id, token:'reader-test', file:'0'
            }); true
            """)
        fixture.update(dark: true, source: .init(text: source, wrap: true))
        _ = try await fixture.wait()
        let updated = try await web.evaluateJavaScript("window.cypherDiff.inspect()") as! [String: Any]
        XCTAssertEqual(updated["generation"] as? Int, info["generation"] as? Int)
        XCTAssertEqual(updated["wrap"] as? Bool, true)
        XCTAssertFalse(requested, "A source reader cannot request another file's context")
        let overflow = try await web.evaluateJavaScript(
            "document.querySelector('diffs-container').shadowRoot.querySelector('pre').dataset.overflow")
        XCTAssertEqual(overflow as? String, "wrap")
    }

    func testSourceVirtualizationPreservesReadingPositionAndTreatsMarkupAsText() async throws {
        let text = (1...500).map {
            "let value\($0) = \"A long line that wraps across the viewport without changing the source.\""
        }.joined(separator: "\n")
        let fixture = try Fixture(patch: "", source: .init(text: text))
        defer { fixture.close() }
        let web = try await fixture.wait()
        let visibleLine = """
            (() => { const rows = [...document.querySelector('diffs-container').shadowRoot.querySelectorAll('[data-content] [data-line]')];
              return Number(rows.find(r => r.getBoundingClientRect().top >= 0)?.dataset.lineIndex ?? -1); })()
            """
        // `rendered` can arrive before the virtualizer has sized the document,
        // when a scroll to 4000 would clamp to the top.
        let tall = try await eventually(web, "document.documentElement.scrollHeight > 4000 + innerHeight")
        XCTAssertTrue(tall, "The virtualized file must take its full height")
        _ = try await web.evaluateJavaScript("window.scrollTo(0, 4000); true")
        let scrolled = try await eventually(web, "\(visibleLine) > 100")
        XCTAssertTrue(scrolled, "Scrolling must mount the rows it reaches")
        let before = try await web.evaluateJavaScript(visibleLine) as? Int ?? -1
        fixture.update(source: .init(text: text, wrap: true))
        _ = try await fixture.wait()
        // Rows reflow over the frames after the wrap; the anchor holds through them.
        let held = try await eventually(web, "Math.abs(\(visibleLine) - \(before)) <= 2")
        let after = try await web.evaluateJavaScript(visibleLine) as? Int ?? -1
        XCTAssertTrue(held, "Wrapping must keep line \(before) in view, not \(after)")
        XCTAssertGreaterThan(before, 100)
        XCTAssertEqual(Double(before), Double(after), accuracy: 2)
        let count =
            try await web.evaluateJavaScript(
                "document.querySelector('diffs-container').shadowRoot.querySelectorAll('[data-content] [data-line]').length"
            ) as? Int ?? 500
        XCTAssertLessThanOrEqual(count, 150)
        fixture.update(source: .init(text: "</script><img src='https://example.invalid/a' onerror='alert(1)'>\n你好 🌿"))
        _ = try await fixture.wait()
        let safe = try await web.evaluateJavaScript(
            """
            (() => { const r = document.querySelector('diffs-container').shadowRoot;
              return r.querySelectorAll('img,iframe,script').length === 0 &&
                r.querySelector('pre').textContent.includes('</script><img'); })()
            """)
        XCTAssertEqual(safe as? Bool, true)
    }

    func testSourceRendererFailureFallsBackWithoutLosingText() async throws {
        let source = WorkspaceBrowserSession.readerExample
        let hosted = try HostedWindow(WorkspaceSourceView(path: "Example.swift", text: source, partial: true))
        defer { hosted.close() }
        let host = hosted.host
        var web: WKWebView?
        let deadline = ContinuousClock.now + WorkspaceDiffWebView.renderDeadline
        while ContinuousClock.now < deadline {
            web = firstSubview(WKWebView.self, in: host.view)
            if let web, let state = try? await web.evaluateJavaScript("window.cypherDiff?.inspect().status"),
                state as? String == "rendered"
            {
                break
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        let loaded = try XCTUnwrap(web)
        _ = try await loaded.evaluateJavaScript(
            """
            window.webkit.messageHandlers.diffRenderer.postMessage({
              event:'error', id:window.cypherDiff.inspect().id
            }); true
            """)
        for _ in 0..<50 {
            if firstSubview(UITextView.self, in: host.view) != nil { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        let plain = try XCTUnwrap(firstSubview(UITextView.self, in: host.view))
        XCTAssertEqual(plain.text, source)
        XCTAssertFalse(plain.isEditable)
        XCTAssertTrue(plain.isSelectable)
        XCTAssertNil(firstSubview(WKWebView.self, in: host.view))
    }
}

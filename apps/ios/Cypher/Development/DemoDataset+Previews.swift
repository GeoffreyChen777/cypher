// Demo chat for previews in a reply (`-demo -route chat:chat-previews`): a
// pull request, an issue and a commit ref with their cards, and one ref the
// host doesn't know; file previews of a CSV, a TSV, a PDF, a video, an HTML
// page and an SVG in the demo checkout (WorkspaceBrowserSession's demo
// files), one missing file, and an image captioned once.

import UIKit

extension DemoDataset {
    static let previewsChatId = "chat-previews"

    /// What the demo host says about a ref (`ResolveGitRef`).
    static func gitRefResolution(_ ref: GitRef) -> GitRefResolution {
        let now = Int64(Date().timeIntervalSince1970)
        switch ref {
        case .issue(_, 71):
            return .pullRequest(
                GitPullPreview(
                    repo: "mvp-lab/cypher", number: 71, title: "Relay: re-seal links after a host re-joins",
                    state: .open, author: "changrui", headBranch: "cypher/relay-links", baseBranch: "main",
                    url: "https://github.com/mvp-lab/cypher/pull/71",
                    checks: GitChecksSummary(state: .pending, passed: 9, failed: 0, pending: 2, skipped: 3),
                    changedFiles: 6, additions: 214, deletions: 37))
        case .issue(_, 64):
            return .issue(
                GitIssuePreview(
                    repo: "mvp-lab/cypher", number: 64, title: "Couldn't load models on iPhone after the Mac slept",
                    state: .closed, stateReason: "completed", author: "changrui",
                    url: "https://github.com/mvp-lab/cypher/issues/64"))
        case .commit(_, let sha) where "4f9c2e1b7a".hasPrefix(sha) || sha.hasPrefix("4f9c2e1"):
            return .commit(
                GitCommitPreview(
                    sha: "4f9c2e1b7a3d5e6f8091a2b3c4d5e6f708192a3b", shortSha: "4f9c2e1",
                    subject: "Transcript: keep the veil off finished replies", author: "Changrui",
                    authorTime: now - 3 * 3600, filesChanged: 3, insertions: 48, deletions: 12,
                    url: "https://github.com/mvp-lab/cypher/commit/4f9c2e1b7a3d5e6f8091a2b3c4d5e6f708192a3b"))
        default:
            return .notFound(url: ref.webURL(checkoutRepo: "mvp-lab/cypher")?.absoluteString)
        }
    }

    static func previewsTranscript(now: Int64) -> [MessageEntry] {
        let reply = """
            PR #71 carries the relay fix; it closes #64. The veil change landed in 4f9c2e1, and `4f9c2e1` \
            reads the same in code. #9999 isn't on GitHub, so it has no card.

            I timed the relay round trips after each re-join. The raw numbers:

            `notes/latency.csv`

            The medians per region, tab-separated:

            notes/regions.tsv

            The write-up and a recording of the reconnect:

            `notes/report.pdf`

            <notes/demo.mp4>

            The status page prototype and the topology diagram:

            `notes/prototype.html`

            `notes/diagram.svg`

            The run log was cleaned up, so it can't show:

            `notes/old-run.csv`

            The ![app icon](notes/icon.png) didn't change, so the sentence says its name once. On its own an \
            image keeps its caption:

            ![App icon, 96 px](notes/icon.png)
            """
        return [
            MessageEntry(
                id: "m1", role: .user,
                parts: [.text(id: "t0", text: "Summarise the relay work.")],
                createdAt: now - 1_600_000, deviceId: "ios-demo", status: .complete),
            MessageEntry(
                id: "m2", role: .assistant, parts: [.text(id: "t0", text: reply)],
                createdAt: now - 1_550_000, deviceId: "dev-mac", status: .complete),
        ]
    }

    // MARK: Demo files

    /// `notes/latency.csv`: 24 rows, so the preview shows its first 20.
    static let latencyCSV: String = {
        let regions = ["eu-west", "us-east", "ap-south", "us-west"]
        var lines = ["run,region,p50_ms,p95_ms,rejoined"]
        for run in 1...24 {
            let region = regions[run % regions.count]
            let p50 = 38 + (run * 7) % 23
            let p95 = p50 * 2 + (run * 13) % 31
            lines.append("\(run),\(region),\(p50),\(p95),\(run % 5 == 0 ? "yes" : "no")")
        }
        return lines.joined(separator: "\n") + "\n"
    }()

    /// `notes/regions.tsv`.
    static let regionsTSV = """
        region\tmedian_ms\tnote
        eu-west\t41\tclosest to the relay
        us-east\t58\t
        ap-south\t112\t"two hops, via Singapore"
        us-west\t74\tafter the re-join fix

        """

    /// `notes/prototype.html`.
    static let prototypeHTML = """
        <!doctype html>
        <html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
        <title>Relay status prototype</title>
        <style>
        body{font:16px -apple-system,sans-serif;margin:24px;color:#222}
        @media (prefers-color-scheme:dark){body{background:#111;color:#eee}}
        .row{display:flex;justify-content:space-between;padding:10px 0;border-bottom:1px solid #8883}
        .ok{color:#16a34a}.slow{color:#d97706}
        </style></head>
        <body><h1>Relay status</h1>
        <div class="row"><span>eu-west</span><span class="ok">41 ms</span></div>
        <div class="row"><span>us-east</span><span class="ok">58 ms</span></div>
        <div class="row"><span>ap-south</span><span class="slow">112 ms</span></div>
        <div class="row"><span>us-west</span><span class="ok">74 ms</span></div>
        </body></html>

        """

    /// `notes/report.pdf`: five Letter pages drawn here (the preview shows
    /// three, then "2 more pages").
    static func reportPDF() -> Data {
        let page = CGRect(x: 0, y: 0, width: 612, height: 792)
        let title: [NSAttributedString.Key: Any] = [
            .font: UIFont.systemFont(ofSize: 26, weight: .bold), .foregroundColor: UIColor.black,
        ]
        let body: [NSAttributedString.Key: Any] = [
            .font: UIFont.systemFont(ofSize: 13), .foregroundColor: UIColor.darkGray,
        ]
        return UIGraphicsPDFRenderer(bounds: page).pdfData { context in
            context.beginPage()
            ("Relay reconnect report" as NSString).draw(at: CGPoint(x: 56, y: 64), withAttributes: title)
            let text = """
                After a host re-joins, sealed links are re-issued and the phone reconnects \
                within one round trip. Medians per region are below; the full run is in latency.csv.
                """
            (text as NSString).draw(in: CGRect(x: 56, y: 112, width: 500, height: 80), withAttributes: body)
            let medians: [CGFloat] = [41, 58, 112, 74]
            for (index, value) in medians.enumerated() {
                let x = 76 + CGFloat(index) * 120
                UIColor(white: 0.55, alpha: 1).setFill()
                UIBezierPath(
                    roundedRect: CGRect(x: x, y: 460 - value * 2.4, width: 64, height: value * 2.4), cornerRadius: 4
                )
                .fill()
            }
            UIColor(white: 0.8, alpha: 1).setFill()
            UIRectFill(CGRect(x: 56, y: 460, width: 500, height: 1))
            for line in 0..<9 {
                UIColor(white: 0.88, alpha: 1).setFill()
                UIRectFill(CGRect(x: 56, y: 500 + CGFloat(line) * 22, width: line % 3 == 2 ? 320 : 500, height: 9))
            }
            for heading in ["Method", "Appendix A: runs", "Appendix B: regions", "Appendix C: logs"] {
                context.beginPage()
                (heading as NSString).draw(at: CGPoint(x: 56, y: 64), withAttributes: title)
                for line in 0..<20 {
                    UIColor(white: 0.88, alpha: 1).setFill()
                    UIRectFill(
                        CGRect(x: 56, y: 112 + CGFloat(line) * 22, width: line % 4 == 3 ? 280 : 500, height: 9))
                }
            }
        }
    }
}

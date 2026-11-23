// Demo chat for previews in a reply (`-demo -route chat:chat-previews`): a
// pull request, an issue and a commit ref with their cards, and one ref the
// host doesn't know.

import Foundation

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
}

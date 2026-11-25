// The card a git ref in a reply shows (desktop hover card): a pull
// request's title, state, checks, author and head → base; an issue's title,
// state and author; a commit's subject, short sha, author, age and size.
// Neutral ink throughout: state reads as a word and a glyph, not a color.

import SwiftUI
import UIKit

struct GitRefCard: View {
    let resolution: GitRefResolution
    /// The card's own actions (the popover a tap opens); nil in a context
    /// menu preview, whose menu holds them.
    var actions: GitRefCardActions?

    static let width: CGFloat = 320

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch resolution {
            case .pullRequest(let pull): pullBody(pull)
            case .issue(let issue): issueBody(issue)
            case .commit(let commit): commitBody(commit)
            case .notFound, .unavailable: EmptyView()
            }
            if let actions {
                Rectangle().fill(Theme.border).frame(height: 1).padding(.top, 4)
                HStack(spacing: 16) {
                    ForEach(Array(GitRefCardActions.items(for: resolution).enumerated()), id: \.offset) { _, item in
                        Button {
                            actions.perform(item)
                        } label: {
                            Label(item.title, systemImage: item.symbol)
                                .font(Theme.sans(13, weight: .medium))
                                .foregroundStyle(Theme.text)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("git-ref-action-\(item.id)")
                    }
                    Spacer(minLength: 0)
                }
            }
        }
        .padding(14)
        .frame(width: Self.width, alignment: .leading)
        .background(Theme.sheetPanel)
        .accessibilityIdentifier("git-ref-card")
    }

    private func header(symbol: String, _ text: String, trailing: String? = nil) -> some View {
        HStack(spacing: 6) {
            Image(systemName: symbol).font(.system(size: 11, weight: .medium))
            Text(text).font(Theme.mono(11)).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 8)
            if let trailing { Text(trailing).font(Theme.sans(12, weight: .medium)) }
        }
        .foregroundStyle(Theme.textMuted)
    }

    private func title(_ text: String) -> some View {
        Text(text)
            .font(Theme.sans(15, weight: .semibold))
            .foregroundStyle(Theme.text)
            .lineLimit(3)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func detail(_ text: String) -> some View {
        Text(text)
            .font(Theme.sans(12.5))
            .foregroundStyle(Theme.textMuted)
            .lineLimit(2)
            .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder private func pullBody(_ pull: GitPullPreview) -> some View {
        header(
            symbol: GitRefCardText.symbol(pull.state, pull: true), "\(pull.repo) #\(pull.number)",
            trailing: GitRefCardText.state(pull.state, reason: nil))
        title(pull.title)
        detail("\(pull.author) · \(pull.headBranch) → \(pull.baseBranch)")
        if let checks = pull.checks {
            HStack(spacing: 6) {
                Image(systemName: GitRefCardText.checksSymbol(checks.state)).font(.system(size: 11, weight: .semibold))
                Text(GitRefCardText.checks(checks)).font(Theme.sans(12.5))
            }
            .foregroundStyle(Theme.text)
            .accessibilityIdentifier("git-ref-checks")
        }
        if let size = GitRefCardText.size(files: pull.changedFiles, plus: pull.additions, minus: pull.deletions) {
            detail(size)
        }
    }

    @ViewBuilder private func issueBody(_ issue: GitIssuePreview) -> some View {
        header(
            symbol: GitRefCardText.symbol(issue.state, pull: false), "\(issue.repo) #\(issue.number)",
            trailing: GitRefCardText.state(issue.state, reason: issue.stateReason))
        title(issue.title)
        detail("Opened by \(issue.author)")
    }

    @ViewBuilder private func commitBody(_ commit: GitCommitPreview) -> some View {
        header(
            symbol: "smallcircle.filled.circle", "commit \(commit.shortSha)",
            trailing: GitRefCardText.age(commit.authorTime, now: Date()))
        title(commit.subject)
        detail(commit.author)
        if let size = GitRefCardText.size(files: commit.filesChanged, plus: commit.insertions, minus: commit.deletions)
        {
            detail(size)
        }
    }
}

/// What a card's buttons (and its context menu) do.
struct GitRefCardActions {
    var openURL: (URL) -> Void
    var copy: (String) -> Void
    var dismiss: () -> Void

    struct Item: Hashable {
        enum Kind: Hashable {
            case open(URL)
            case copy(String)
        }
        var id: String
        var title: String
        var symbol: String
        var kind: Kind
    }

    /// PR / issue: open on GitHub; commit: open on GitHub when it has a
    /// page, and copy its sha (iOS has no per-commit changes view).
    static func items(for resolution: GitRefResolution) -> [Item] {
        var items: [Item] = []
        if let url = resolution.url {
            items.append(Item(id: "open", title: "Open on GitHub", symbol: "arrow.up.right.square", kind: .open(url)))
        }
        if case .commit(let commit) = resolution {
            items.append(Item(id: "copy", title: "Copy SHA", symbol: "doc.on.doc", kind: .copy(commit.sha)))
        }
        return items
    }

    func perform(_ item: Item) {
        dismiss()
        switch item.kind {
        case .open(let url): openURL(url)
        case .copy(let text): copy(text)
        }
    }
}

/// The words and glyphs a card shows, kept apart for tests.
enum GitRefCardText {
    static func state(_ state: GitRefState, reason: String?) -> String {
        switch state {
        case .open: return "Open"
        case .draft: return "Draft"
        case .merged: return "Merged"
        case .closed: return reason == "not_planned" ? "Closed as not planned" : "Closed"
        }
    }

    static func symbol(_ state: GitRefState, pull: Bool) -> String {
        guard pull else { return state == .closed ? "checkmark.circle" : "circle.circle" }
        switch state {
        case .open, .draft: return "arrow.triangle.pull"
        case .merged: return "arrow.triangle.merge"
        case .closed: return "xmark.circle"
        }
    }

    static func checksSymbol(_ state: GitChecksSummary.State) -> String {
        switch state {
        case .success: "checkmark"
        case .failure: "xmark"
        case .pending: "clock"
        }
    }

    /// `12 passed · 1 failed · 2 pending`, leaving out the zeros.
    static func checks(_ checks: GitChecksSummary) -> String {
        let parts = [
            (checks.passed, "passed"), (checks.failed, "failed"), (checks.pending, "pending"),
            (checks.skipped, "skipped"),
        ]
        .filter { $0.0 > 0 }
        .map { "\($0.0) \($0.1)" }
        if parts.isEmpty { return "No checks" }
        let prefix =
            switch checks.state {
            case .success: "Checks passed"
            case .failure: "Checks failed"
            case .pending: "Checks running"
            }
        return "\(prefix): " + parts.joined(separator: " · ")
    }

    /// `3 files · +40 −12`; nil when nothing is known.
    static func size(files: Int?, plus: Int?, minus: Int?) -> String? {
        var parts: [String] = []
        if let files { parts.append(files == 1 ? "1 file" : "\(files) files") }
        if let plus, let minus { parts.append("+\(plus) −\(minus)") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// `just now`, `5 min ago`, `3 h ago`, `2 days ago`, then the date.
    static func age(_ unixSeconds: Int64, now: Date) -> String {
        let seconds = max(0, Int(now.timeIntervalSince1970) - Int(unixSeconds))
        switch seconds {
        case ..<60: return "just now"
        case ..<3600: return "\(seconds / 60) min ago"
        case ..<86_400: return "\(seconds / 3600) h ago"
        case ..<(86_400 * 30):
            let days = seconds / 86_400
            return days == 1 ? "1 day ago" : "\(days) days ago"
        default:
            let date = Date(timeIntervalSince1970: TimeInterval(unixSeconds))
            return date.formatted(.dateTime.year().month(.abbreviated).day())
        }
    }
}

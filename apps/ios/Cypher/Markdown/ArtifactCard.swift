// Artifact card — the chat card of crates/ui/src/markdown/artifact.rs
// (`render_card`): a page the model wrote, shown as a card on the code
// block's plate instead of its source. Tapping it opens the page full screen
// (ArtifactViewer). While its fence streams it reads "Writing…" and doesn't
// open: a half-written page shows nothing useful. A named page opens at its
// newest version (ArtifactShelf); a patch's card reads how its edits went and
// opens the page it updated.

import SwiftUI

/// A page to show: what a card (or an HTML fence's Preview) opens.
struct ArtifactPage: Identifiable, Hashable {
    let id = UUID()
    var kind: ArtifactKind
    let title: String
    var code: String
    /// The fence's `title=`: a later artifact (or patch) under the same name
    /// is this page revised (ArtifactShelf). nil for an unnamed page.
    var name: String? = nil
    /// Open on the source rather than the page (the demo rig's
    /// `-sheet artifact-source`).
    var showsSource = false
}

struct ArtifactCard: View {
    let kind: ArtifactKind
    let title: String?
    let code: String
    let closed: Bool
    /// Tagged `artifact-patch`: `code` is edits to the page `title` names.
    var patch = false
    /// The row is still streaming: an unclosed fence is still being written.
    var streaming = false

    @Environment(AppModel.self) private var model
    @Environment(ArtifactShelf.self) private var shelf: ArtifactShelf?
    @Environment(\.transcriptEntry) private var entry
    @State private var page: ArtifactPage?

    private var writing: Bool { !closed && streaming }
    private var displayTitle: String {
        ArtifactFence.displayTitle(kind: kind, title: title, code: code)
    }
    private var status: (subtitle: String, warns: Bool, opens: Bool) {
        if patch {
            let outcome = writing ? nil : entry.flatMap { shelf?.outcome(entry: $0.entryId, code: code) }
            return Self.patchStatus(writing: writing, hunks: ArtifactPatch.hunks(code).count, outcome: outcome)
        }
        let count = max(code.split(separator: "\n", omittingEmptySubsequences: false).count, 1)
        let lines = count == 1 ? "1 line" : "\(count) lines"
        return (writing ? "Writing… · \(lines)" : "\(kind.label) · \(lines)", false, !writing)
    }

    /// A patch card's subtitle, whether it warns, and whether it opens: only
    /// a patch that found its page has one to open (artifact.rs
    /// `patch_subtitle`).
    nonisolated static func patchStatus(
        writing: Bool, hunks: Int, outcome: PatchOutcome?
    ) -> (subtitle: String, warns: Bool, opens: Bool) {
        let edits = { (n: Int) in n == 1 ? "1 edit" : "\(n) edits" }
        if writing { return ("Updating… · \(edits(hunks))", false, false) }
        guard let outcome else { return ("Update · \(edits(hunks))", false, false) }
        if !outcome.foundPage { return ("No earlier version to update", true, false) }
        if outcome.applied == outcome.total && outcome.total > 0 {
            return ("Updated · \(edits(outcome.total))", false, true)
        }
        if outcome.applied == 0 {
            return ("Update didn't apply · 0 of \(edits(outcome.total))", true, true)
        }
        return ("Updated · \(outcome.applied) of \(edits(outcome.total)) applied", true, true)
    }

    var body: some View {
        let status = self.status
        Button {
            open()
        } label: {
            HStack(spacing: 12) {
                LineIconView(patch ? .pen : .code, size: 16, color: Theme.textMuted)
                    .frame(width: 34, height: 34)
                    .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 1) {
                    Text(displayTitle)
                        .font(Theme.sans(MD.textSize, weight: .medium, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text(status.subtitle)
                        .font(Theme.sans(12, relativeTo: .caption))
                        .foregroundStyle(status.warns ? Theme.warning : Theme.textMuted)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                if status.opens {
                    HStack(spacing: 4) {
                        Text("Open")
                            .font(Theme.sans(12, weight: .medium, relativeTo: .caption))
                        LineIconView(.chevronRight, size: 12, color: Theme.textMuted)
                    }
                    .foregroundStyle(Theme.textMuted)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(whiteAlpha(0.035), in: RoundedRectangle(cornerRadius: Theme.panelRadius))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.panelRadius)
                    .strokeBorder(whiteAlpha(0.06), lineWidth: 1)
            )
            .contentShape(RoundedRectangle(cornerRadius: Theme.panelRadius))
        }
        .buttonStyle(.plain)
        .disabled(!status.opens)
        .accessibilityLabel("\(displayTitle), \(status.subtitle)")
        .accessibilityHint(status.opens ? "Opens the page" : "")
        .accessibilityIdentifier("artifact-card")
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0, shelf: shelf) }
        .onAppear {
            // Screenshot rig: `-demo -sheet artifact` (or `artifact-source`)
            // opens the first card that shows.
            guard status.opens, let sheet = model.launchSheet, sheet.hasPrefix("artifact") else { return }
            model.launchSheet = nil
            open(showingSource: sheet == "artifact-source")
        }
    }

    private func open(showingSource: Bool = false) {
        let page = ArtifactPage(kind: kind, title: displayTitle, code: code, name: title, showsSource: showingSource)
        self.page = shelf?.latest(page) ?? page
    }
}

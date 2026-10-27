// Artifact card — the chat card of crates/ui/src/markdown/artifact.rs
// (`render_card`): a page the model wrote, shown as a card on the code
// block's plate instead of its source. Tapping it opens the page full screen
// (ArtifactViewer). While its fence streams it reads "Writing…" and doesn't
// open: a half-written page shows nothing useful.

import SwiftUI

/// A page to show: what a card (or an HTML fence's Preview) opens.
struct ArtifactPage: Identifiable, Hashable {
    let id = UUID()
    let kind: ArtifactKind
    let title: String
    let code: String
    /// Open on the source rather than the page (the demo rig's
    /// `-sheet artifact-source`).
    var showsSource = false
}

struct ArtifactCard: View {
    let kind: ArtifactKind
    let title: String?
    let code: String
    let closed: Bool
    /// The row is still streaming: an unclosed fence is still being written.
    var streaming = false

    @Environment(AppModel.self) private var model
    @State private var page: ArtifactPage?

    private var writing: Bool { !closed && streaming }
    private var displayTitle: String {
        ArtifactFence.displayTitle(kind: kind, title: title, code: code)
    }
    private var subtitle: String {
        let count = max(code.split(separator: "\n", omittingEmptySubsequences: false).count, 1)
        let lines = count == 1 ? "1 line" : "\(count) lines"
        return writing ? "Writing… · \(lines)" : "\(kind.label) · \(lines)"
    }

    var body: some View {
        Button {
            open()
        } label: {
            HStack(spacing: 12) {
                LineIconView(.code, size: 16, color: Theme.textMuted)
                    .frame(width: 34, height: 34)
                    .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 8))
                VStack(alignment: .leading, spacing: 1) {
                    Text(displayTitle)
                        .font(Theme.sans(MD.textSize, weight: .medium, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(Theme.sans(12, relativeTo: .caption))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                if !writing {
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
        .disabled(writing)
        .accessibilityLabel("\(displayTitle), \(subtitle)")
        .accessibilityHint(writing ? "" : "Opens the page")
        .accessibilityIdentifier("artifact-card")
        .fullScreenCover(item: $page) { ArtifactViewer(page: $0) }
        .onAppear {
            // Screenshot rig: `-demo -sheet artifact` (or `artifact-source`)
            // opens the first card that shows.
            guard !writing, let sheet = model.launchSheet, sheet.hasPrefix("artifact") else { return }
            model.launchSheet = nil
            open(showingSource: sheet == "artifact-source")
        }
    }

    private func open(showingSource: Bool = false) {
        page = ArtifactPage(kind: kind, title: displayTitle, code: code, showsSource: showingSource)
    }
}

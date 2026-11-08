// The round's changed files, closing the round's last answer
// (transcript/turn_changes.rs): a quiet header in the tool group's voice —
// chevron tile, "6 files changed", the line counts and a Review button —
// over the files, hung off a guide rail like tool chips. The header folds
// the list; Review opens the round's whole diff and each file opens it at
// that file.

import SwiftUI

struct TurnChangesCard: View {
    let changes: TurnChanges
    let open: Bool
    let toggle: () -> Void
    @Environment(\.openWorkspace) private var openWorkspace

    private static let rowHeight: CGFloat = 30
    /// Under the header's chevron tile, less half the 1pt rail.
    private static let railX: CGFloat = 8.5

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if open {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(changes.files.prefix(TurnChanges.cardFiles), id: \.path) { file in
                        railed { fileRow(file) }
                    }
                    if changes.hiddenFiles > 0 {
                        railed {
                            Button {
                                openWorkspace?(.round(changes, focus: nil))
                            } label: {
                                Text(changes.hiddenFiles == 1 ? "1 more file" : "\(changes.hiddenFiles) more files")
                                    .font(Theme.sans(11.5))
                                    .foregroundStyle(Theme.textFaint)
                                    .padding(.horizontal, 6)
                                    .frame(maxWidth: .infinity, minHeight: Self.rowHeight, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(PressWashButtonStyle(cornerRadius: 5))
                            .disabled(openWorkspace == nil)
                        }
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("turn-changes")
    }

    private var header: some View {
        HStack(spacing: 8) {
            Button(action: toggle) {
                HStack(spacing: 8) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(Theme.textMuted)
                        .rotationEffect(.degrees(open ? 90 : 0))
                        .frame(width: 18, height: 18)
                        .background(whiteAlpha(0.06), in: RoundedRectangle(cornerRadius: 5))
                    Text(changes.label)
                        .font(Theme.sans(12))
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                    ChangeCounts(additions: changes.additions, deletions: changes.deletions)
                    Spacer(minLength: 0)
                }
                .frame(height: 26)
                .contentShape(Rectangle())
            }
            .buttonStyle(PressWashButtonStyle(cornerRadius: 6))
            .accessibilityLabel("\(changes.label), \(changes.additions) added, \(changes.deletions) removed")
            .accessibilityHint(open ? "Hides the files" : "Shows the files")
            .accessibilityIdentifier("turn-changes-toggle")
            Button("Review") { openWorkspace?(.round(changes, focus: nil)) }
                .font(Theme.sans(11.5))
                .foregroundStyle(Theme.textFaint)
                .buttonStyle(.plain)
                .padding(.horizontal, 6)
                .frame(minHeight: 26)
                .contentShape(Rectangle())
                .disabled(openWorkspace == nil)
                .accessibilityHint("Opens the files this round changed")
                .accessibilityIdentifier("turn-changes-review")
        }
    }

    private func fileRow(_ file: WorkspaceChange) -> some View {
        let slash = file.path.lastIndex(of: "/")
        let name = slash.map { String(file.path[file.path.index(after: $0)...]) } ?? file.path
        let dir = slash.map { String(file.path[..<$0]) } ?? ""
        let deleted = file.status == "deleted"
        return Button {
            openWorkspace?(.round(changes, focus: file.path))
        } label: {
            HStack(spacing: 6) {
                Text(name)
                    .font(Theme.sans(12))
                    .foregroundStyle(deleted ? Theme.textMuted : Theme.text.opacity(0.85))
                    .strikethrough(deleted)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .layoutPriority(1)
                if let tag = TurnChanges.statusTag(file.status) {
                    Text(tag.word)
                        .font(Theme.sans(10.5))
                        .foregroundStyle(
                            tag.added == true
                                ? Theme.success.opacity(0.75)
                                : tag.added == false ? Theme.danger.opacity(0.75) : Theme.textFaint)
                }
                Text(dir)
                    .font(Theme.sans(11))
                    .foregroundStyle(Theme.textFaint)
                    .lineLimit(1)
                    .truncationMode(.head)
                Spacer(minLength: 0)
                ChangeCounts(additions: file.additions, deletions: file.deletions, binary: file.binary)
            }
            .padding(.horizontal, 6)
            .frame(minHeight: Self.rowHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 5))
        .disabled(openWorkspace == nil)
        .accessibilityLabel("\(file.path), \(file.additions) added, \(file.deletions) removed")
        .accessibilityHint("Opens this file's changes")
        .accessibilityIdentifier("turn-changes-file")
    }

    /// A row hung off the guide rail under the header's chevron.
    private func railed(@ViewBuilder _ content: () -> some View) -> some View {
        content()
            .padding(.leading, 20)
            .overlay(alignment: .leading) {
                Rectangle()
                    .fill(whiteAlpha(0.08))
                    .frame(width: 1)
                    .padding(.leading, Self.railX)
                    .allowsHitTesting(false)
            }
    }
}

/// `+12 −3` in small mono, each side only when non-zero; muted so the
/// numbers sit behind the names.
private struct ChangeCounts: View {
    let additions: Int
    let deletions: Int
    var binary = false

    var body: some View {
        HStack(spacing: 5) {
            if binary { Text("bin").foregroundStyle(Theme.textFaint) }
            if additions > 0 { Text("+\(additions)").foregroundStyle(Theme.success.opacity(0.75)) }
            if deletions > 0 { Text("−\(deletions)").foregroundStyle(Theme.danger.opacity(0.75)) }
        }
        .font(Theme.mono(10.5))
        .monospacedDigit()
        .fixedSize()
    }
}

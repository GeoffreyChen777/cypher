// The strip under each settled message — the desktop's hover strip
// (transcript render.rs `render_timestamp_strip`): the date, the model that
// answered, then fork, restart and copy. A phone has no hover, so it always
// shows, in the same quiet tone; where the desktop explains with a tooltip,
// tapping explains with a popover.

import SwiftUI
import UIKit

/// What a session's message strips may do beyond copying. nil in a side
/// chat's transcript: like the desktop's embedded one, it offers neither
/// fork nor restart.
struct TranscriptMessageActions {
    /// Why fork / restart can't run right now (transcript.rs `fork_gate` /
    /// `rewind_gate`, less the per-row newest-entry rule); nil = they can.
    var forkBlocked: String?
    var rewindBlocked: String?
    /// The entry whose fork / restart is in flight: its button spins.
    var forking: String?
    var rewinding: String?
    var fork: (TranscriptEntryContext) -> Void
    /// Asks before it deletes anything (the desktop's arm-then-confirm).
    var rewind: (TranscriptEntryContext) -> Void
}

extension EnvironmentValues {
    @Entry var transcriptMessageActions: TranscriptMessageActions? = nil
}

struct MessageStrip: View {
    @Environment(\.transcriptMessageActions) private var actions
    let row: TranscriptRow
    /// No entry follows this one: restarting here would delete nothing.
    let isLastEntry: Bool
    /// The entry's clipboard text, assembled only when copy is tapped.
    let copyText: () -> String?

    @State private var copied = false
    @State private var copyReset: Task<Void, Never>?

    var body: some View {
        HStack(spacing: 0) {
            if let at = row.timestamp {
                Text(MessageStripFormat.timestamp(at))
                    .fixedSize()
                    .padding(.trailing, 2)
            }
            if let label = row.answered {
                Text("·").padding(.horizontal, 4)
                AnsweredModelLabel(label: label)
            }
            if let actions, row.role != .system {
                forkButton(actions)
                rewindButton(actions)
            }
            StripButton(
                icon: copied ? .check : .copy,
                tint: copied ? Theme.success : Theme.textMuted,
                label: copied ? "Copied" : "Copy message",
                blocked: row.copyable ? nil : "No text to copy",
                action: copy)
        }
        .font(Theme.sans(12, relativeTo: .caption))
        .foregroundStyle(Theme.textMuted.opacity(0.7))
        .lineLimit(1)
        // The buttons' hit areas overhang the content column so the date sits
        // on the text's (or bubble's) edge, like the desktop's flush label.
        .padding(.trailing, row.role == .user ? -6 : 0)
        .frame(height: 26)
        .frame(maxWidth: .infinity, alignment: row.role == .user ? .trailing : .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("message-strip-\(row.entryId)")
        .onDisappear { copyReset?.cancel() }
    }

    private var entry: TranscriptEntryContext {
        TranscriptEntryContext(entryId: row.entryId, role: row.role, settled: row.settled)
    }

    private var unsettled: String? {
        row.settled ? nil : "Wait for this message to finish."
    }

    /// gates.rs `rewind_gate`'s tail rule.
    private var lastEntryBlock: String? {
        isLastEntry ? "Nothing to remove after the last message." : nil
    }

    private func forkButton(_ actions: TranscriptMessageActions) -> some View {
        StripButton(
            icon: .gitBranch, tint: Theme.textMuted,
            // gates.rs `fork_tooltip`.
            label: row.role == .user ? "Fork before this message" : "Fork after this response",
            blocked: actions.forkBlocked ?? unsettled,
            pending: actions.forking == row.entryId
        ) {
            actions.fork(entry)
        }
    }

    private func rewindButton(_ actions: TranscriptMessageActions) -> some View {
        StripButton(
            icon: .restart, tint: Theme.textMuted,
            // gates.rs `rewind_tooltip`, unarmed.
            label: row.role == .user
                ? "Restart from here — deletes this message and everything after it"
                : "Restart from here — deletes everything after this response",
            blocked: actions.rewindBlocked ?? lastEntryBlock ?? unsettled,
            pending: actions.rewinding == row.entryId
        ) {
            actions.rewind(entry)
        }
    }

    private func copy() {
        guard let text = copyText() else { return }
        UIPasteboard.general.string = text
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        copied = true
        copyReset?.cancel()
        copyReset = Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1200))
            guard !Task.isCancelled else { return }
            copied = false
        }
    }
}

@MainActor
enum MessageStripFormat {
    /// rows.rs `format_timestamp`: "Jul 1, 3:45 PM" in local time — short
    /// month, day, 12-hour clock without a leading zero.
    static func timestamp(_ ms: Int64, in timeZone: TimeZone = .current) -> String {
        formatter.timeZone = timeZone
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }

    private static let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "MMM d, h:mm a"
        return formatter
    }()
}

/// The answering model after the date: in the strip's tone, or in the
/// warning tone when another model answered — tap for what was asked for.
private struct AnsweredModelLabel: View {
    let label: AnsweredLabel
    @State private var explaining = false

    var body: some View {
        if let swap = label.substituted {
            Button {
                explaining = true
            } label: {
                HStack(spacing: 4) {
                    LineIconView(.dangerTriangle, size: 12, color: Theme.warning.opacity(0.85))
                    Text(label.text).truncationMode(.middle)
                }
                .foregroundStyle(Theme.warning.opacity(0.85))
                .frame(height: 26)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Answered by \(label.text)")
            .accessibilityHint(swap)
            .popover(isPresented: $explaining) { StripTip(text: swap) }
        } else {
            Text(label.text)
                .truncationMode(.middle)
                .accessibilityLabel("Answered by \(label.text)")
        }
    }
}

/// One of the strip's icon buttons. A blocked one stays visible, dimmed,
/// and says why when tapped (the desktop's disabled-reason tooltip).
private struct StripButton: View {
    let icon: LineIcon
    let tint: Color
    let label: String
    var blocked: String?
    var pending = false
    let action: () -> Void

    @State private var explaining = false

    var body: some View {
        Button {
            if blocked != nil {
                explaining = true
            } else {
                action()
            }
        } label: {
            ZStack {
                if pending {
                    ProgressView().controlSize(.mini)
                } else {
                    LineIconView(icon, size: 13, color: blocked == nil ? tint : tint.opacity(0.3))
                }
            }
            .frame(width: 30, height: 26)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(pending)
        .accessibilityLabel(label)
        .accessibilityHint(blocked ?? "")
        .popover(isPresented: $explaining) { StripTip(text: blocked ?? "") }
    }
}

private struct StripTip: View {
    let text: String

    var body: some View {
        Text(text)
            .font(Theme.sans(13, relativeTo: .footnote))
            .foregroundStyle(Theme.textMuted)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: 280)
            .presentationCompactAdaptation(.popover)
    }
}

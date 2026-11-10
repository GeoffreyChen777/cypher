// The rows the board's overview and its list pages share, so an item reads
// the same wherever it is listed (items.rs, needs.rs `need_row`, work.rs
// `running_row` / `settled_row`): a small mark in a leading gutter, the
// title, one muted line and a quiet trailing note; agent-written text as
// markdown; and the one way a page runs an action and says why it failed.

import SwiftUI

/// A list row: a mark in the leading gutter, the title, a muted sub-line,
/// and a trailing note. `extra` sits under the sub-line (a step bar).
struct BoardRow<Mark: View, Extra: View>: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    @ScaledMetric(relativeTo: .body) private var markLift: CGFloat = 5.5
    let title: String
    var sub: String?
    var subColor: Color = Theme.textMuted
    var trailing: String?
    var trailingColor: Color = Theme.textFaint
    var titleLines = 1
    @ViewBuilder var mark: Mark
    @ViewBuilder var extra: Extra

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            mark
                .frame(width: 18)
                .alignmentGuide(.firstTextBaseline) { [markLift] in $0[VerticalAlignment.center] + markLift }
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(title)
                        .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                        .foregroundStyle(Theme.text)
                        .lineLimit(typeSize.isAccessibilitySize ? titleLines + 1 : titleLines)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let trailing {
                        Text(trailing)
                            .font(Theme.sans(13, relativeTo: .subheadline))
                            .foregroundStyle(trailingColor)
                            .monospacedDigit()
                            .fixedSize()
                    }
                }
                if let sub, !sub.isEmpty {
                    Text(sub)
                        .font(Theme.sans(13, relativeTo: .subheadline))
                        .foregroundStyle(subColor)
                        .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                }
                extra
            }
        }
        .padding(.vertical, 2)
    }
}

extension BoardRow where Extra == EmptyView {
    init(
        title: String, sub: String? = nil, subColor: Color = Theme.textMuted, trailing: String? = nil,
        trailingColor: Color = Theme.textFaint, titleLines: Int = 1, @ViewBuilder mark: () -> Mark
    ) {
        self.init(
            title: title, sub: sub, subColor: subColor, trailing: trailing, trailingColor: trailingColor,
            titleLines: titleLines, mark: mark, extra: { EmptyView() })
    }
}

/// A row's glyph mark.
struct BoardGlyph: View {
    let symbol: String
    var color: Color = Theme.textMuted

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: 14, weight: .medium))
            .foregroundStyle(color)
            .accessibilityHidden(true)
    }
}

/// A work item's mark: a spinner while it runs, then how it ended
/// (items.rs `work_mark`).
struct WorkMark: View {
    let work: BotWork

    var body: some View {
        switch work.phase {
        case .running, .starting, .stopping:
            MiniSpinner(cellSize: 2.2).foregroundStyle(Theme.statusWorking)
                .accessibilityLabel("Running")
        default:
            BoardGlyph(symbol: work.phaseSymbol, color: work.phase.tone.color)
        }
    }
}

/// A step's state mark (items.rs `step_mark`).
struct StepMark: View {
    let state: StepState

    var body: some View {
        Group {
            switch state {
            case .pending:
                Circle().strokeBorder(Theme.textFaint.opacity(0.8), lineWidth: 1).frame(width: 10, height: 10)
            case .running:
                MiniSpinner(cellSize: 2.0).foregroundStyle(Theme.statusWorking)
            case .asking:
                BoardGlyph(symbol: "questionmark.circle", color: Theme.warning)
            case .done:
                BoardGlyph(symbol: "checkmark", color: Theme.success)
            case .failed:
                BoardGlyph(symbol: "xmark.circle", color: Theme.danger)
            case .skipped:
                Capsule().fill(Theme.textFaint).frame(width: 9, height: 1.5)
            }
        }
        .frame(width: 18, height: 18)
        .accessibilityElement()
        .accessibilityLabel(state.status.text)
    }
}

/// A thin step bar: done steps of all.
struct BoardProgressBar: View {
    let done: Int
    let total: Int

    var body: some View {
        GeometryReader { geo in
            Capsule().fill(Theme.text.opacity(0.08))
                .overlay(alignment: .leading) {
                    Capsule().fill(Theme.success.opacity(0.8))
                        .frame(width: total > 0 ? geo.size.width * CGFloat(done) / CGFloat(total) : 0)
                }
        }
        .frame(height: 3)
        .accessibilityElement()
        .accessibilityLabel("\(done) of \(total) steps done")
    }
}

// MARK: - Item rows

/// A Needs you row: what it is, what it needs, and when.
struct NeedRowView: View {
    let row: BoardNeedRow

    var body: some View {
        BoardRow(title: row.title, sub: row.sub, trailing: row.at.map { BotTime.ago($0) }) {
            BoardGlyph(symbol: row.symbol, color: Theme.warning)
        }
    }
}

/// A running item: its title, the current step over a step bar, and the
/// time it has run.
struct WorkRunningRow: View {
    let work: BotWork
    var now: Int64 = nowMs()

    var body: some View {
        let (done, total) = work.progress
        BoardRow(
            title: singleLine(work.title),
            sub: work.phase == .stopping ? "Stopping…" : work.currentStepLine,
            trailing: BotTime.span(now - (work.startedAt ?? work.createdAt))
        ) {
            WorkMark(work: work)
        } extra: {
            BoardProgressBar(done: done, total: total)
                .padding(.top, 5)
                .padding(.bottom, 2)
        }
    }
}

/// A waiting or ended item: its mark, title, one line, and when.
struct WorkSettledRow: View {
    let work: BotWork
    var now: Int64 = nowMs()

    var body: some View {
        BoardRow(
            title: singleLine(work.title),
            sub: work.state == .proposed ? work.phaseLine(now: now) : work.resultLine(now: now),
            subColor: work.phase == .awaiting ? Theme.warning : Theme.textMuted,
            trailing: BotTime.ago(work.endedAt ?? work.createdAt, now: now)
        ) {
            WorkMark(work: work)
        }
    }
}

/// A work item's row, running or not.
struct WorkRow: View {
    let work: BotWork

    var body: some View {
        if work.state == .running {
            WorkRunningRow(work: work)
        } else {
            WorkSettledRow(work: work)
        }
    }
}

// MARK: - Markdown

/// Agent-written markdown (a request, a summary, a step note, a result) in
/// the transcript's blocks, at a page's reading size.
struct BotMarkdown: View {
    let text: String
    var cacheKey = "bot-md"

    var body: some View {
        let blocks = MarkdownParser.parse(text)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { ix, top in
                MarkdownBlockView(block: top.block, cacheKey: "\(cacheKey)/\(ix)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Actions

/// Runs a page's actions one at a time and keeps why the last one failed.
@MainActor
@Observable
final class BotActionRunner {
    /// An action is on its way.
    var busy = false
    var error: String?

    func run(_ action: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true
        Task {
            defer { busy = false }
            do {
                try await action()
            } catch {
                self.error = (error as? BotActionError)?.message ?? error.localizedDescription
            }
        }
    }
}

extension View {
    /// The runner's failure as an alert.
    func botActionAlert(_ runner: BotActionRunner) -> some View {
        alert(
            "Couldn't do that",
            isPresented: Binding(get: { runner.error != nil }, set: { if !$0 { runner.error = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(runner.error ?? "")
        }
    }
}

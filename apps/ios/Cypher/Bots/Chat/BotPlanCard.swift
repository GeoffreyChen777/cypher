// The plan card (transcript/bot_plan.rs; docs/design/agent-bots.md §6.1,
// §6.6): a workspace's `propose_plan` call in its chat, drawn from the
// plan's record rather than the call. One card through the plan's life: its
// numbered steps with Cancel / Edit / Approve while it waits, live marks,
// notes and sessions with Stop while it runs, the result once it ends.

import SwiftUI

struct BotPlanCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    @Environment(\.seedComposer) private var seedComposer
    let callId: String
    let chatId: String
    /// The call ended: a missing record is gone, not still being written.
    let resolved: Bool

    @State private var busy = false
    @State private var error: String?
    @State private var confirmingStop = false

    var body: some View {
        if let work = model.botRecords.plan(forCall: callId, chatId: chatId) {
            // Live durations tick while it runs.
            TimelineView(.periodic(from: .now, by: work.phase == .running ? 30 : 3_600)) { _ in
                card(work, now: nowMs())
            }
        } else {
            BotCardSurface {
                PlanHead(
                    title: "Plan", subtitle: resolved ? "This plan is no longer available." : "Writing the plan…",
                    tone: Theme.textMuted, status: nil)
            }
        }
    }

    private func card(_ work: BotWork, now: Int64) -> some View {
        let phase = work.phase
        let places = work.steps.map { step in step.project.flatMap(place) }
        // Where it works, named once when every step shares the project.
        let common = places.first.flatMap { $0 }.flatMap { first in places.allSatisfy { $0 == first } ? first : nil }
        var subtitle = "Plan · \(work.steps.count) step\(work.steps.count == 1 ? "" : "s")"
        if let common { subtitle += " · \(common)" }
        return BotCardSurface {
            PlanHead(
                title: work.title, subtitle: subtitle, tone: Self.tone(phase),
                status: PlanStatus(work: work, phase: phase, now: now))
            BotCardRule()
            VStack(alignment: .leading, spacing: 12) {
                if let summary = work.summary?.trimmingCharacters(in: .whitespacesAndNewlines), !summary.isEmpty {
                    Text(Self.inline(summary))
                        .font(Theme.sans(14, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if phase == .running || phase == .stopping {
                    let (done, total) = work.progress
                    HStack(spacing: 10) {
                        ProgressView(value: Double(done), total: Double(max(total, 1)))
                            .tint(Theme.accent)
                        Text("\(done) of \(total)")
                            .font(Theme.sans(11.5))
                            .foregroundStyle(Theme.textMuted)
                            .monospacedDigit()
                            .fixedSize()
                    }
                    .accessibilityElement(children: .combine)
                }
                steps(work, phase: phase, places: common == nil ? places : [], now: now)
                if phase.ended, let result = work.result?.trimmingCharacters(in: .whitespacesAndNewlines),
                    !result.isEmpty
                {
                    BotMarkdown(text: result, cacheKey: "plan-\(work.id)")
                        .padding(.horizontal, 12)
                        .padding(.vertical, 9)
                        .background(Theme.text.opacity(0.04), in: RoundedRectangle(cornerRadius: 9))
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            if phase.awaiting || phase.running {
                BotCardRule()
                actions(work, phase: phase)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("plan-card")
        .confirmationDialog("Stop the work and its sessions?", isPresented: $confirmingStop, titleVisibility: .visible) {
            Button("Stop", role: .destructive) { act { try await $0.stopWork(workId: work.id) } }
        } message: {
            Text("Stop ends the work and every session it started.")
        }
    }

    // MARK: Steps

    /// While the plan waits each step is numbered with its detail; once it
    /// runs, its mark, note and sessions show.
    private func steps(_ work: BotWork, phase: PlanPhase, places: [String?], now: Int64) -> some View {
        let numbered = [PlanPhase.awaiting, .starting, .cancelling, .cancelled].contains(phase)
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(work.steps.enumerated()), id: \.element.id) { ix, step in
                let last = ix == work.steps.count - 1
                HStack(alignment: .top, spacing: 10) {
                    VStack(spacing: 3) {
                        PlanStepMark(state: step.state, number: ix + 1, numbered: numbered)
                        if !last {
                            Rectangle().fill(Theme.border).frame(width: 1).frame(maxHeight: .infinity)
                        }
                    }
                    .frame(width: 20)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(Self.inline(step.title))
                            .font(Theme.sans(14, relativeTo: .subheadline))
                            .foregroundStyle(step.state == .skipped ? Theme.textMuted : Theme.text)
                            .fixedSize(horizontal: false, vertical: true)
                        if numbered, let detail = step.detail?.trimmingCharacters(in: .whitespacesAndNewlines),
                            !detail.isEmpty
                        {
                            Text(Self.inline(detail))
                                .font(Theme.sans(12.5, relativeTo: .footnote))
                                .foregroundStyle(Theme.textMuted)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        let place = ix < places.count ? places[ix] : nil
                        let note = numbered ? nil : step.note
                        if place != nil || note != nil {
                            Text([place, note].compactMap { $0 }.joined(separator: " · "))
                                .font(Theme.sans(12, relativeTo: .footnote))
                                .foregroundStyle(Theme.textFaint)
                                .lineLimit(2)
                        }
                        if !numbered {
                            ForEach(step.sessions, id: \.self) { sessionId in
                                if let chat = model.chat(id: sessionId) { sessionRow(chat) }
                            }
                        }
                    }
                    .padding(.bottom, last ? 0 : 12)
                    Spacer(minLength: 0)
                    if !numbered, let span = step.span(now: now) {
                        Text(span)
                            .font(Theme.sans(11.5))
                            .foregroundStyle(Theme.textFaint)
                            .monospacedDigit()
                            .padding(.top, 1)
                    }
                }
                .accessibilityElement(children: .contain)
            }
        }
    }

    /// A session a step started: its status dot and title; a tap opens it.
    private func sessionRow(_ chat: Chat) -> some View {
        let indicator = model.indicator(for: chat)
        let color: Color =
            switch indicator {
            case .working: Theme.accent
            case .awaitingInput: Theme.warning
            case .errored: Theme.danger
            case .completed: Theme.success
            case .idle: Theme.textFaint
            }
        return Button {
            openChat?(chat.id)
        } label: {
            HStack(spacing: 7) {
                Circle().fill(color).frame(width: 6, height: 6)
                Text(chat.displayTitle)
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.textFaint)
            }
            .frame(minHeight: 28)
            .contentShape(Rectangle())
        }
        .buttonStyle(PressWashButtonStyle(cornerRadius: 6))
        .disabled(openChat == nil)
        .accessibilityHint("Opens the session")
    }

    // MARK: Actions

    private func actions(_ work: BotWork, phase: PlanPhase) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(error ?? (phase.awaiting ? "Nothing runs until you approve." : "Stop ends the work and every session it started."))
                .font(Theme.sans(12.5, relativeTo: .footnote))
                .foregroundStyle(error == nil ? Theme.textMuted : Theme.danger)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                if phase.awaiting {
                    BotCardButton(title: "Cancel", role: .secondary, busy: false) {
                        act { try await $0.resolvePlan(workId: work.id, approve: false) }
                    }
                    BotCardButton(title: "Edit", role: .secondary, busy: false) { edit(work) }
                    BotCardButton(title: "Approve", role: .primary, busy: busy) {
                        act { try await $0.resolvePlan(workId: work.id, approve: true) }
                    }
                } else {
                    BotCardButton(title: "Stop", role: .destructive, busy: busy) { confirmingStop = true }
                }
            }
            .disabled(busy)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    /// Edit: cancel the proposal and start a reply asking for the change,
    /// so the workspace proposes a revised plan (shell/bots.rs
    /// `edit_bot_plan`).
    private func edit(_ work: BotWork) {
        act { try await $0.resolvePlan(workId: work.id, approve: false) }
        let title = work.title.split(whereSeparator: \.isNewline).joined(separator: " ")
        seedComposer?("Change the plan “\(title)”: ")
    }

    private func act(_ action: @escaping (any WorkspaceData) async throws -> Void) {
        guard !busy else { return }
        busy = true
        error = nil
        Task { @MainActor in
            defer { busy = false }
            do {
                try await action(try model.workspaceData())
                UINotificationFeedbackGenerator().notificationOccurred(.success)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    /// Agent-written lines with their inline marks (`code`, **bold**).
    static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }

    /// "cypher · MacBook Pro": a step's project and its device.
    private func place(_ spaceId: String) -> String? {
        guard let space = model.spaces.first(where: { $0.id == spaceId }) else { return nil }
        return "\(space.displayName) · \(model.deviceName(space.deviceId))"
    }

    /// The head tile's colour: amber while it waits, the accent while it
    /// works, green done, red failed (bot_plan.rs `phase_tone`).
    static func tone(_ phase: PlanPhase) -> Color {
        switch phase {
        case .awaiting: Theme.warning
        case .starting, .running, .stopping: Theme.accent
        case .done: Theme.success
        case .failed: Theme.danger
        case .cancelling, .stopped, .cancelled: Theme.textMuted
        }
    }
}

extension BotWork {
    /// The plan card's status at the head's right: "Awaiting approval",
    /// "Running · 12m", "Done · 18m" (bot_plan.rs `phase_status`).
    func cardStatus(now: Int64 = nowMs()) -> String {
        let took = startedAt.map { BotTime.span((endedAt ?? now) - $0) }
        let withTime = { (word: String) in took.map { "\(word) · \($0)" } ?? word }
        switch phase {
        case .awaiting: return "Awaiting approval"
        case .starting: return "Starting…"
        case .cancelling: return "Cancelling…"
        case .running: return withTime("Running")
        case .stopping: return "Stopping…"
        case .done: return withTime("Done")
        case .failed: return withTime("Failed")
        case .stopped: return withTime("Stopped")
        case .cancelled: return "Cancelled"
        }
    }
}

/// The plan card's head: the tile in the phase's colour, the title over its
/// sub-line, and the status at the right — or under them when the row is
/// too narrow to hold all three whole (a phone).
private struct PlanHead: View {
    let title: String
    let subtitle: String
    let tone: Color
    let status: PlanStatus?

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            BotCardTile(symbol: "checklist", tint: tone)
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 8) {
                    lines.fixedSize(horizontal: true, vertical: false)
                    Spacer(minLength: 4)
                    status
                }
                VStack(alignment: .leading, spacing: 4) {
                    lines
                    status
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }

    private var lines: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title)
                .font(Theme.sans(15, weight: .semibold, relativeTo: .subheadline))
                .foregroundStyle(Theme.text)
                .lineLimit(3)
            Text(subtitle)
                .font(Theme.sans(12, relativeTo: .footnote))
                .foregroundStyle(Theme.textMuted)
                .lineLimit(2)
        }
    }
}

private struct PlanStatus: View {
    let work: BotWork
    let phase: PlanPhase
    let now: Int64

    var body: some View {
        HStack(spacing: 5) {
            switch phase {
            case .awaiting: Circle().fill(Theme.warning).frame(width: 6, height: 6)
            case .running: MiniSpinner(cellSize: 1.8)
            case .done: Image(systemName: "checkmark").font(.system(size: 10, weight: .bold))
            case .failed: Image(systemName: "exclamationmark.triangle").font(.system(size: 10, weight: .semibold))
            default: EmptyView()
            }
            Text(work.cardStatus(now: now))
                .font(Theme.sans(12, weight: .medium, relativeTo: .footnote))
                .lineLimit(1)
        }
        .foregroundStyle(color)
        .fixedSize()
        .accessibilityIdentifier("plan-status")
    }

    private var color: Color {
        switch phase {
        case .awaiting: Theme.warning
        case .running: Theme.text
        case .done: Theme.success
        case .failed: Theme.danger
        default: Theme.textMuted
        }
    }
}

/// A step's mark: its number while the plan waits, then how it went.
private struct PlanStepMark: View {
    let state: StepState
    let number: Int
    let numbered: Bool

    var body: some View {
        Group {
            if numbered {
                Text("\(number)")
                    .font(Theme.sans(10.5, weight: .semibold))
                    .foregroundStyle(Theme.textMuted)
                    .frame(width: 19, height: 19)
                    .overlay(Circle().strokeBorder(Theme.borderStrong, lineWidth: 1))
            } else {
                switch state {
                case .pending:
                    Circle().strokeBorder(Theme.textFaint, lineWidth: 1).frame(width: 9, height: 9)
                case .running:
                    MiniSpinner(cellSize: 1.8).foregroundStyle(Theme.textMuted)
                case .asking:
                    Image(systemName: "questionmark.circle").foregroundStyle(Theme.warning)
                case .done:
                    Image(systemName: "checkmark").foregroundStyle(Theme.success)
                case .failed:
                    Image(systemName: "xmark").foregroundStyle(Theme.danger)
                case .skipped:
                    Capsule().fill(Theme.textFaint).frame(width: 8, height: 1.5)
                }
            }
        }
        .font(.system(size: 12, weight: .semibold))
        .frame(width: 20, height: 20)
        .accessibilityLabel(numbered ? "Step \(number)" : "Step \(number), \(state.status.text)")
    }
}

// MARK: - Shared card pieces

/// A transcript card's surface: a soft fill and hairline border, like the
/// desktop's plan and routine cards.
struct BotCardSurface<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
        .padding(.vertical, 4)
    }
}

/// A hairline between a card's sections.
struct BotCardRule: View {
    var body: some View {
        Rectangle().fill(Theme.border).frame(height: 0.5).padding(.horizontal, 16)
    }
}

/// A card head's glyph tile, in the item's colour.
struct BotCardTile: View {
    let symbol: String
    let tint: Color

    var body: some View {
        RoundedRectangle(cornerRadius: 8, style: .continuous)
            .fill(tint.opacity(0.14))
            .frame(width: 30, height: 30)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(tint)
            }
            .accessibilityHidden(true)
    }
}

/// A card's compact button: filled for its main decision, a soft wash
/// otherwise.
struct BotCardButton: View {
    enum Role { case primary, secondary, destructive }
    let title: String
    let role: Role
    var busy = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Text(title).opacity(busy ? 0 : 1)
                if busy { ProgressView().controlSize(.small).tint(role == .primary ? Theme.bg : Theme.text) }
            }
            .font(Theme.sans(14, weight: .semibold, relativeTo: .subheadline))
            .foregroundStyle(role == .primary ? Theme.bg : role == .destructive ? Theme.danger : Theme.text)
            .padding(.horizontal, 16)
            .frame(minHeight: 34)
            .background(role == .primary ? Theme.text : Theme.text.opacity(0.08), in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("card-\(title.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

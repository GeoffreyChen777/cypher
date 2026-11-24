// The workspace chat's notices (transcript/bot_plan.rs; docs/design/
// agent-bots.md §13.1). The chat only announces: a plan card says what is
// proposed, with Details / Reject / Approve while it waits and the decision
// once made; a result card says a plan or a routine's run ended and how;
// a routine card says the agent saved a routine. Steps, progress and
// results live on the board, which the cards open.

import SwiftUI

struct BotPlanCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotPage) private var openPage
    let callId: String
    let chatId: String
    /// The call ended: a missing record is gone, not still being written.
    let resolved: Bool

    @State private var busy = false
    @State private var error: String?

    var body: some View {
        if let work = model.botRecords.plan(forCall: callId, chatId: chatId) {
            card(work)
        } else {
            BotCardSurface {
                NoticeHead(
                    symbol: "checklist", tint: Theme.textMuted, title: "Plan",
                    subtitle: resolved ? "This plan is no longer available." : "Writing the plan…")
            }
        }
    }

    private func card(_ work: BotWork) -> some View {
        let places = work.steps.map { step in step.project.flatMap(place) }
        // Where it works, named when every step shares the project.
        let common = places.first.flatMap { $0 }.flatMap { first in places.allSatisfy { $0 == first } ? first : nil }
        let workspaceLimit = model.bot(id: work.botId)?.plans.timeLimit ?? PlanSettings.defaultTimeLimit
        let subtitle = BotWork.planLine(
            steps: work.steps.count, place: common, limit: work.ownLimit(workspace: workspaceLimit))
        let details: () -> Void = { openPage?(.workItem(work.id)) }
        return BotCardSurface {
            if let decision = work.decisionWord {
                // Decided: the decision and the way in, never the progress.
                Button(action: details) {
                    NoticeHead(symbol: "checklist", tint: Theme.textMuted, title: work.title, subtitle: subtitle) {
                        HStack(spacing: 4) {
                            Image(systemName: decision.approved ? "checkmark" : "xmark")
                                .font(.system(size: 10, weight: .bold))
                            Text(decision.word)
                                .font(Theme.sans(12, weight: .medium, relativeTo: .footnote))
                        }
                        .foregroundStyle(Theme.textMuted)
                        .fixedSize()
                        .accessibilityIdentifier("plan-status")
                        NoticeChevron()
                    }
                }
                .buttonStyle(.plain)
                .disabled(openPage == nil)
                .accessibilityHint("Opens the plan")
            } else {
                NoticeHead(symbol: "checklist", tint: Theme.warning, title: work.title, subtitle: subtitle) {
                    Text("Awaiting approval")
                        .font(Theme.sans(12, weight: .medium, relativeTo: .footnote))
                        .foregroundStyle(Theme.warning)
                        .fixedSize()
                        .accessibilityIdentifier("plan-status")
                }
                if let summary = work.summary?.trimmingCharacters(in: .whitespacesAndNewlines), !summary.isEmpty {
                    Text(Self.inline(summary))
                        .font(Theme.sans(14, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text.opacity(0.85))
                        .lineLimit(2)
                        .padding(.horizontal, 14)
                        .padding(.bottom, 10)
                }
                BotCardRule()
                VStack(alignment: .leading, spacing: 8) {
                    if let error {
                        Text(error)
                            .font(Theme.sans(12.5, relativeTo: .footnote))
                            .foregroundStyle(Theme.danger)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    HStack(spacing: 8) {
                        Button(action: details) {
                            HStack(spacing: 3) {
                                Text("Details")
                                NoticeChevron()
                            }
                            .font(Theme.sans(14, weight: .medium, relativeTo: .subheadline))
                            .foregroundStyle(Theme.textMuted)
                            .frame(minHeight: 34)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(openPage == nil)
                        .accessibilityIdentifier("card-details")
                        Spacer(minLength: 0)
                        BotCardButton(title: "Reject", role: .secondary, busy: false) {
                            act { try await $0.resolvePlan(workId: work.id, approve: false) }
                        }
                        BotCardButton(title: "Approve", role: .primary, busy: busy) {
                            act { try await $0.resolvePlan(workId: work.id, approve: true) }
                        }
                    }
                    .disabled(busy)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("plan-card")
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
}

extension BotWork {
    /// How a decided plan reads on its card: the owner's decision, never
    /// the run's progress; nil while it waits (bot_plan.rs `decision_word`).
    var decisionWord: (word: String, approved: Bool)? {
        switch phase {
        case .awaiting: nil
        case .cancelling, .cancelled: ("Rejected", false)
        default: ("Approved", true)
        }
    }

    /// A plan card's one quiet line: "Plan · 4 steps · web-api · Studio",
    /// and its time limit when it asked for its own (bot_plan.rs `plan_sub`).
    static func planLine(steps: Int, place: String?, limit: Int? = nil) -> String {
        var line = "Plan · \(steps) step\(steps == 1 ? "" : "s")"
        if let place { line += " · \(place)" }
        if let limit { line += " · up to \(PlanSettings.limitWords(limit))" }
        return line
    }
}

// MARK: - Result notice

/// A result notice's card: what ended, how, and the way to its details
/// (bot_plan.rs `render_bot_post`). A plan's opens the plan's page while
/// its record is kept; anything else the notice in full.
struct BotPostCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotPage) private var openPage
    let entryId: String
    let from: BotPost

    var body: some View {
        if let notice = BotPostNotice(from) {
            Button {
                openPage?(page)
            } label: {
                BotCardSurface {
                    NoticeHead(
                        symbol: notice.symbol, tint: notice.status.tone.color, title: notice.title,
                        subtitle: notice.subtitle
                    ) {
                        BotStatusText(notice.status, size: 12.5)
                            .fixedSize()
                        NoticeChevron()
                    }
                }
            }
            .buttonStyle(.plain)
            .disabled(openPage == nil)
            .accessibilityHint("Opens the result")
            .accessibilityIdentifier("post-card")
        }
    }

    private var page: BotPage {
        if case .work(let workId, _, _, _, _) = from, model.data?.work(id: workId) != nil {
            return .workItem(workId)
        }
        return .post(entryId: entryId)
    }
}

/// What a result notice's card says (bot_plan.rs `render_bot_post`).
struct BotPostNotice: Equatable {
    var symbol: String
    var title: String
    var subtitle: String
    var status: BotStatusLabel

    /// nil for a source this app doesn't know.
    init?(_ from: BotPost, now: Int64 = nowMs()) {
        switch from {
        case .routine(_, let name, let outcome, let runAt, _):
            symbol = "clock"
            title = name
            subtitle = "Routine · \(BotTime.past(runAt, now: now))"
            status = outcome == .failed ? BotStatusLabel("Failed", .failed) : BotStatusLabel("Done", .done)
        case .work(_, let title, let outcome, let startedAt, let endedAt):
            symbol = "checklist"
            self.title = title
            subtitle =
                startedAt.map { "Plan · took \(BotTime.span(endedAt - $0))" }
                ?? "Plan · \(BotTime.past(endedAt, now: now))"
            status =
                switch outcome {
                case .failed: BotStatusLabel("Failed", .failed)
                case .stopped: BotStatusLabel("Stopped", .muted)
                default: BotStatusLabel("Done", .done)
                }
        case .unknown:
            return nil
        }
    }
}

// MARK: - Saved routine

/// A routine as its `save_routine` answer names it: `Saved routine
/// "<name>" (id <id>): …` (bot_plan.rs `parse_saved_routine`).
struct SavedRoutine: Equatable {
    /// Where the doc's tool part keeps the answer on its call's fields.
    static let outputField = "output"

    var name: String
    var id: String

    init?(output: String?) {
        guard let output else { return nil }
        let text = output.drop(while: \.isWhitespace)
        let lead = "Saved routine \""
        guard text.hasPrefix(lead) else { return nil }
        let rest = text.dropFirst(lead.count)
        guard let close = rest.range(of: "\" (id "), let end = rest[close.upperBound...].firstIndex(of: ")") else {
            return nil
        }
        let name = rest[..<close.lowerBound].trimmingCharacters(in: .whitespaces)
        let id = rest[close.upperBound..<end].trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, !id.isEmpty, !id.contains(where: \.isWhitespace) else { return nil }
        self.name = name
        self.id = id
    }
}

/// A saved routine's card (bot_plan.rs `render_bot_routine`): its name over
/// where and when it runs, and the way to its page. Read from the routine's
/// record; one that is gone keeps the name the call gave.
struct BotRoutineCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotPage) private var openPage
    let name: String
    let routineId: String

    var body: some View {
        if let routine = model.data?.routine(id: routineId) {
            Button {
                openPage?(.routine(routineId))
            } label: {
                BotCardSurface {
                    NoticeHead(
                        symbol: "clock", tint: Theme.textMuted, title: routine.name, subtitle: subtitle(routine)
                    ) {
                        HStack(spacing: 4) {
                            Image(systemName: "checkmark").font(.system(size: 10, weight: .bold))
                            Text("Saved").font(Theme.sans(12, weight: .medium, relativeTo: .footnote))
                        }
                        .foregroundStyle(Theme.textMuted)
                        .fixedSize()
                        NoticeChevron()
                    }
                }
            }
            .buttonStyle(.plain)
            .disabled(openPage == nil)
            .accessibilityHint("Opens the routine")
            .accessibilityIdentifier("routine-card")
        } else {
            BotCardSurface {
                NoticeHead(symbol: "clock", tint: Theme.textMuted, title: name, subtitle: "Routine · no longer here")
            }
        }
    }

    /// "Routine · Daily 09:00 · in web-api".
    private func subtitle(_ routine: BotRoutine) -> String {
        var line = "Routine · \(routine.schedule.describe)"
        if let spaceId = routine.target.spaceId,
            let space = model.spaces.first(where: { $0.id == spaceId })
        {
            line += " · in \(space.displayName)"
        }
        return line
    }
}

// MARK: - Notice pieces

/// A notice's head: a small tile in its tone, the title over one quiet
/// line, and whatever closes the row (a status, a way in).
struct NoticeHead<Trailing: View>: View {
    let symbol: String
    let tint: Color
    let title: String
    let subtitle: String
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            BotCardTile(symbol: symbol, tint: tint)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(Theme.sans(15, weight: .medium, relativeTo: .subheadline))
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
                Text(subtitle)
                    .font(Theme.sans(12, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            trailing
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

extension NoticeHead where Trailing == EmptyView {
    init(symbol: String, tint: Color, title: String, subtitle: String) {
        self.init(symbol: symbol, tint: tint, title: title, subtitle: subtitle) { EmptyView() }
    }
}

/// The small chevron of a notice that opens its details.
struct NoticeChevron: View {
    var body: some View {
        Image(systemName: "chevron.right")
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(Theme.textFaint)
            .accessibilityHidden(true)
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
            // At a large text size the label outgrows the 34pt: keep room
            // around it.
            .padding(.vertical, 6)
            .frame(minHeight: 34)
            .background(role == .primary ? Theme.text : Theme.text.opacity(0.08), in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("card-\(title.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

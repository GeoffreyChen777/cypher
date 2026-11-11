// A workspace's work (work.rs): every plan and orchestration in three lists
// (running, proposed, finished), and one in full: its status, the request,
// the plan as one flow on one rail (each step's state, duration, note and
// sessions, ending in the result), its times; Approve / Cancel while it
// waits, Stop while it runs, Delete once it ended, and its orchestration
// session read-only.

import SwiftUI

struct WorkListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var showActivity = false

    var body: some View {
        let lists = BoardWorkLists(model.data?.work(of: botId) ?? [])
        BotPageList(title: "Work") {
            section("Running", lists.running)
            section("Proposed", lists.proposed)
            section("Finished", lists.finished)
            if !lists.isEmpty {
                Section {
                    NavigationLink(value: BotPage.activity) {
                        BoardRow(title: "View all activity") { BoardGlyph(symbol: "waveform.path.ecg") }
                    }
                    .accessibilityIdentifier("work-all-activity")
                }
                .groupedRowStyle()
            }
        }
        .overlay {
            if lists.isEmpty {
                BotEmptyState(
                    symbol: "list.bullet.indent", title: "No work yet",
                    hint: "Ask it for something that takes a few steps; it proposes a plan first.",
                    action: ("View all activity", { showActivity = true }))
            }
        }
        .navigationDestination(isPresented: $showActivity) { BotActivityPage(botId: botId) }
    }

    @ViewBuilder
    private func section(_ title: String, _ items: [BotWork]) -> some View {
        if !items.isEmpty {
            Section {
                ForEach(items) { work in
                    NavigationLink(value: BotPage.workItem(work.id)) { WorkRow(work: work) }
                        .accessibilityIdentifier("work-\(work.id)")
                }
            } header: {
                ListSectionHeader(title: title)
            }
            .groupedRowStyle()
        }
    }
}

// MARK: - One item

struct WorkItemPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let botId: String
    let workId: String
    @State private var actions = BotActionRunner()
    @State private var confirmStop = false
    @State private var confirmDelete = false

    var body: some View {
        if let work = model.data?.work(id: workId) {
            page(work)
        } else {
            BotEmptyState(
                symbol: "list.bullet.indent", title: "Work gone", hint: "It was deleted on another device.")
        }
    }

    private func page(_ work: BotWork) -> some View {
        let phase = work.phase
        let threadListed = work.threadChatId.flatMap { model.chat(id: $0) } != nil
        return BotPageList(title: "Work") {
            Section {
                BotPageHeader(title: work.title) {
                    BotHeaderGlyph(symbol: work.phaseSymbol, tint: headerTint(phase))
                }
            }
            .groupedRowStyle()
            Section {
                BotFact(status: work.status())
                if let space = work.commonProject.flatMap(spaceRow) {
                    NavigationLink(value: BotPage.project(space.id)) {
                        BotFact(label: "Project", value: "\(space.displayName) · \(model.deviceName(space.deviceId))")
                    }
                }
                times(work)
            }
            .groupedRowStyle()
            if work.showsRequest {
                Section {
                    BotMarkdown(text: work.instruction, cacheKey: "work-request-\(work.id)")
                        .padding(.vertical, 4)
                } header: {
                    ListSectionHeader(title: "Request")
                }
                .groupedRowStyle()
            }
            if !work.steps.isEmpty {
                Section {
                    if let summary = work.summary, !summary.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        BotMarkdown(text: summary, cacheKey: "work-summary-\(work.id)")
                            .foregroundStyle(Theme.textMuted)
                            .padding(.vertical, 4)
                    }
                    WorkFlow(work: work)
                        .padding(.vertical, 6)
                } header: {
                    ListSectionHeader(title: "Plan")
                }
                .groupedRowStyle()
            }
            Section {
                NavigationLink(value: BotPage.activity) {
                    BoardRow(title: "View all activity") { BoardGlyph(symbol: "waveform.path.ecg") }
                }
            }
            .groupedRowStyle()
        }
        .toolbar {
            if threadListed, let thread = work.threadChatId {
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink(value: BotThreadLink(chatId: thread)) {
                        Label("Open its session", systemImage: "bubble.left.and.text.bubble.right")
                    }
                    .accessibilityIdentifier("work-thread")
                }
            }
            if phase.ended {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Delete", systemImage: "trash", role: .destructive) { confirmDelete = true }
                        .accessibilityIdentifier("work-delete")
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            if phase.awaiting {
                BotDecisionBar(
                    note: "Nothing runs until you approve.",
                    decisions: [
                        BotDecision(title: "Cancel plan", role: .destructive) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: work.id, approve: false) }
                        },
                        BotDecision(title: "Approve", role: .primary) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: work.id, approve: true) }
                        },
                    ], busy: actions.busy)
            } else if phase.running {
                BotDecisionBar(
                    note: "Stop ends the work and every session it started.",
                    decisions: [BotDecision(title: "Stop", role: .destructive) { confirmStop = true }],
                    busy: actions.busy)
            }
        }
        .confirmationDialog("Stop this work?", isPresented: $confirmStop, titleVisibility: .visible) {
            Button("Stop", role: .destructive) {
                actions.run { try await model.workspaceData().stopWork(workId: work.id) }
            }
        } message: {
            Text("Every session it started stops too.")
        }
        .confirmationDialog("Delete this work?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                actions.run {
                    try await model.workspaceData().deleteWork(workId: work.id)
                    dismiss()
                }
            }
        } message: {
            Text("Its background session goes too; the sessions it started in projects stay.")
        }
        .botActionAlert(actions)
    }

    private func headerTint(_ phase: PlanPhase) -> Color {
        switch phase {
        case .running, .starting, .stopping: Theme.statusWorking
        case .cancelling, .stopped, .cancelled: Theme.textMuted
        default: phase.tone.color
        }
    }

    private func spaceRow(_ id: String) -> Space? {
        model.spaces.first { $0.id == id }
    }

    /// When it was proposed, decided, started and ended, and how long it
    /// took or has run (work.rs `WorkStatusTip`).
    @ViewBuilder
    private func times(_ work: BotWork) -> some View {
        BotFact(label: "Proposed", value: BotTime.past(work.createdAt))
        if let decision = work.decision, work.startedAt == nil {
            BotFact(label: decision.approve ? "Approved" : "Cancelled", value: BotTime.past(decision.at))
        }
        if let start = work.startedAt {
            BotFact(label: "Started", value: BotTime.past(start))
            if let end = work.endedAt {
                BotFact(label: "Ended", value: BotTime.past(end))
                BotFact(label: "Took", value: BotTime.span(end - start))
            }
        }
    }
}

// MARK: - The flow

/// The plan as one flow on one rail (work.rs `work_flow`): each step with
/// its state, duration and details (its project when the steps differ, its
/// detail while the plan waits or its note once it runs, the sessions it
/// started), then the result the last step leads to. The rail fills in as
/// steps finish; a step's title folds its details.
struct WorkFlow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let work: BotWork
    @State private var folded: Set<Int> = []
    @State private var copied = false

    var body: some View {
        let numbered = [PlanPhase.awaiting, .starting, .cancelling, .cancelled].contains(work.phase)
        let common = work.commonProject
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(work.steps.enumerated()), id: \.offset) { ix, step in
                node(segment: segment(step.state)) {
                    if numbered {
                        Text("\(ix + 1)")
                            .font(Theme.sans(11, weight: .semibold, relativeTo: .caption))
                            .foregroundStyle(Theme.textMuted)
                            .frame(width: 18, height: 18)
                            .overlay(Circle().strokeBorder(Theme.text.opacity(0.16), lineWidth: 1))
                    } else {
                        StepMark(state: step.state)
                    }
                } content: {
                    stepContent(ix, step, numbered: numbered, showProject: common == nil)
                }
            }
            node(segment: nil) {
                Image(systemName: "flag")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(resultTone)
                    .frame(width: 18, height: 18)
            } content: {
                resultContent
            }
        }
    }

    /// One node: its mark over the segment to the next node, beside its
    /// content.
    private func node<Mark: View, Content: View>(
        segment: Color?, @ViewBuilder mark: () -> Mark, @ViewBuilder content: () -> Content
    ) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 0) {
                mark()
                    .padding(.top, 1)
                if let segment {
                    Capsule().fill(segment).frame(width: 1.5).frame(maxHeight: .infinity)
                        .padding(.top, 4)
                }
            }
            .frame(width: 18)
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, segment == nil ? 0 : 14)
        }
    }

    /// A segment past a finished step is filled; the rest stay a hairline.
    private func segment(_ state: StepState) -> Color {
        state == .done ? Theme.success.opacity(0.5) : Theme.text.opacity(0.12)
    }

    @ViewBuilder
    private func stepContent(_ ix: Int, _ step: WorkStep, numbered: Bool, showProject: Bool) -> some View {
        let space = showProject ? step.project.flatMap { id in model.spaces.first { $0.id == id } } : nil
        let detail = (numbered ? step.detail : step.note).flatMap {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0
        }
        let foldable = space != nil || detail != nil || !step.sessions.isEmpty
        let isFolded = foldable && folded.contains(ix)
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(Motion.fadeQuick) {
                    if !folded.insert(ix).inserted { folded.remove(ix) }
                }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(step.title)
                        .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                        .foregroundStyle(step.state == .skipped ? Theme.textMuted : Theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .multilineTextAlignment(.leading)
                    if !numbered, let span = step.span() {
                        Text(span)
                            .font(Theme.sans(13, relativeTo: .subheadline))
                            .foregroundStyle(Theme.textFaint)
                            .monospacedDigit()
                    }
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                        .rotationEffect(.degrees(isFolded ? -90 : 0))
                        .opacity(foldable ? 1 : 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .disabled(!foldable)
            .accessibilityLabel("Step \(ix + 1): \(step.title), \(numbered ? "planned" : step.state.status.text)")
            .accessibilityHint(foldable ? (isFolded ? "Shows its details" : "Hides its details") : "")
            if !isFolded {
                if let space {
                    Label {
                        Text("\(space.displayName) · \(model.deviceName(space.deviceId))")
                    } icon: {
                        Image(systemName: "folder")
                    }
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textMuted)
                    .labelStyle(.titleAndIcon)
                }
                if let detail {
                    BotMarkdown(text: detail, cacheKey: "work-step-\(work.id)-\(ix)")
                        .foregroundStyle(step.state == .asking ? Theme.warning : Theme.textMuted)
                }
                ForEach(step.sessions, id: \.self) { chatId in
                    sessionRow(chatId)
                }
            }
        }
    }

    /// A session a step started: its status, title, and a tap that opens
    /// it (work.rs `flow_session`).
    @ViewBuilder
    private func sessionRow(_ chatId: String) -> some View {
        if let chat = model.chat(id: chatId) {
            let status = model.indicator(for: chat)
            Button {
                openChat?(chatId)
            } label: {
                HStack(spacing: 8) {
                    SessionStatusMark(indicator: status, cellSize: 2.0)
                        .frame(width: 12)
                    Text(singleLine(plainMarkdown(chat.displayTitle)))
                        .font(Theme.sans(14, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let word = Self.sessionWord(status) {
                        Text(word.text)
                            .font(Theme.sans(12, relativeTo: .caption))
                            .foregroundStyle(word.color)
                    }
                    Image(systemName: "arrow.up.forward")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(Theme.text.opacity(0.04), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .contentShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.borderless)
            .accessibilityLabel("Open session \(chat.displayTitle)")
            .accessibilityIdentifier("work-session-\(chatId)")
        } else {
            Text("A session not on this device yet")
                .font(Theme.sans(13, relativeTo: .subheadline))
                .foregroundStyle(Theme.textFaint)
        }
    }

    /// A session's state in a word; none while it sits idle (items.rs
    /// `session_word`).
    static func sessionWord(_ status: ChatIndicator) -> (text: String, color: Color)? {
        switch status {
        case .working: ("Working", Theme.textMuted)
        case .awaitingInput: ("Needs input", Theme.warning)
        case .errored: ("Failed", Theme.danger)
        case .completed: ("Done", Theme.textFaint)
        case .idle: nil
        }
    }

    private var resultTone: Color {
        switch work.phase {
        case .done: Theme.success
        case .failed: Theme.danger
        case let phase where phase.ended: Theme.textMuted
        default: Theme.textFaint
        }
    }

    @ViewBuilder
    private var resultContent: some View {
        let ended = work.phase.ended
        let result = work.result.flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 }
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text("Result")
                    .font(Theme.sans(15, weight: .semibold, relativeTo: .body))
                    .foregroundStyle(ended ? Theme.text : Theme.textMuted)
                Spacer(minLength: 8)
                if ended, let result {
                    Button {
                        UIPasteboard.general.string = result
                        copied = true
                        Task {
                            try? await Task.sleep(for: .seconds(1.5))
                            copied = false
                        }
                    } label: {
                        Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                            .font(Theme.sans(13, weight: .medium, relativeTo: .subheadline))
                    }
                    .buttonStyle(.borderless)
                    .tint(Theme.textMuted)
                    .accessibilityIdentifier("work-result-copy")
                }
            }
            if ended, let result {
                BotMarkdown(text: result, cacheKey: "work-result-\(work.id)")
            } else {
                Text(ended ? work.phaseLine() : "Reported when the last step ends")
                    .font(Theme.sans(14, relativeTo: .body))
                    .foregroundStyle(ended ? Theme.textMuted : Theme.textFaint)
            }
        }
    }
}

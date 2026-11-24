// A workspace's work (work.rs): every plan and orchestration in three lists
// (running, proposed, finished), and one in full. Its page says each thing
// once: how it stands (its times on a tap) and its title; the result once
// it ended; then the steps on one rail (each step's state, duration and
// what it reported). A plan still to approve also quotes what was asked.
// A long plan's row holds previews; the page reads the full text from the
// workspace chat doc (PlanText.swift) and, until it is here, shows the
// preview with a quiet "Loading the full plan…" under the cut text.
// Approve / Edit / Reject while it waits and Stop while it runs sit at the
// foot, in reach; Delete and its coordinator (read-only) in the bar.

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
    @Environment(\.seedComposer) private var seedComposer
    @Environment(\.botOpenChat) private var openChat
    let botId: String
    let workId: String
    @State private var actions = BotActionRunner()
    @State private var confirmStop = false
    @State private var confirmDelete = false
    @State private var copied = false
    /// The workspace chat's store, while the page reads its plan bodies.
    @State private var bodyStore: SessionStore?

    var body: some View {
        if let work = model.data?.work(id: workId) {
            page(work)
                .task(id: BodyWatch(work)) { await watchBodies(BodyWatch(work)) }
        } else {
            BotEmptyState(
                symbol: "list.bullet.indent", title: "Work gone", hint: "It was deleted on another device.")
        }
    }

    /// Edit: reject the proposal, put the start of the reply that asks for
    /// the change in the workspace chat's composer, and go there.
    private func edit(_ work: BotWork) {
        let title = work.title.split(whereSeparator: \.isNewline).joined(separator: " ")
        actions.run {
            try await model.workspaceData().resolvePlan(workId: work.id, approve: false)
            seedComposer?("Change the plan “\(title)”: ")
            openChat(work.chatId)
        }
    }

    /// What the page needs of the workspace chat: its plan bodies, while it
    /// shows a text the row had to cut.
    private struct BodyWatch: Hashable {
        var chatId: String
        var wanted: Bool

        init(_ work: BotWork) {
            chatId = work.chatId
            wanted =
                (work.bodyRev != nil && WorkFlow.showsPlan(work.phase))
                || (work.resultRev != nil && work.phase.ended)
        }
    }

    /// Keep the workspace chat's store warm and its entry for this work read
    /// while the page shows (until the task is cancelled).
    private func watchBodies(_ watch: BodyWatch) async {
        guard watch.wanted, let chat = model.chat(id: watch.chatId),
            let store = model.planBodyStore(for: chat)
        else { return }
        model.sessionOnScreen(chat.id)
        store.watchPlanBody(workId)
        bodyStore = store
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3_600))
        }
        store.unwatchPlanBody(workId)
        model.sessionOffScreen(chat.id)
        model.releaseSessionStore(chatId: chat.id)
    }

    private func page(_ work: BotWork) -> some View {
        let phase = work.phase
        let bot = model.bot(id: work.botId)
        let threadListed = work.threadChatId.flatMap { model.chat(id: $0) } != nil
        let shared = work.commonProject.flatMap(spaceRow)
        let text = PlanText.of(work, bodyStore?.planBodies[work.id])
        let result = phase.ended ? nonEmpty(text.result) : nil
        let showsRequest = phase.awaiting && work.showsRequest
        // "Loading the full plan…" goes under each text shown cut: they can
        // be screens apart.
        let requestCut = showsRequest && text.plan.isPreview && Self.isCut(text.instruction)
        let stepsCut =
            text.plan.isPreview && WorkFlow.showsPlan(phase)
            ? Set(text.details.indices.filter { Self.isCut(text.details[$0]) }) : []
        return BotPageList(title: "Work") {
            // How it stands (its times on a tap), its title, and the one
            // project every step works in. Counts and times are not
            // repeated here: the steps show how far it is.
            Section {
                BotItemHeader(
                    status: work.statusWord, title: work.title, details: work.times(), statusIdentifier: "work-status"
                ) {
                    WorkMark(work: work)
                }
                if let shared {
                    NavigationLink(value: BotPage.project(shared.id)) {
                        Label(model.placeName(shared, for: bot), systemImage: "folder")
                            .font(Theme.sans(15, relativeTo: .body))
                            .foregroundStyle(Theme.textMuted)
                    }
                    .accessibilityIdentifier("work-project")
                }
            }
            .groupedRowStyle()
            // What was asked, while the plan is still to judge against it;
            // after that the title stands for it.
            if showsRequest {
                Section {
                    VStack(alignment: .leading, spacing: 2) {
                        BotQuote(text: text.instruction, cacheKey: "work-request-\(work.id)")
                        if requestCut { PlanLoadingLine(part: .plan) }
                    }
                }
                .groupedRowStyle()
            }
            // What it produced comes before how it got there.
            if let result {
                Section {
                    VStack(alignment: .leading, spacing: 2) {
                        BotMarkdown(text: result, cacheKey: "work-result-\(work.id)")
                            .padding(.vertical, 4)
                        if text.resultSource.isPreview { PlanLoadingLine(part: .result) }
                    }
                } header: {
                    ListSectionHeader(title: "Result") {
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
                .groupedRowStyle()
            }
            if !work.steps.isEmpty {
                Section {
                    // The approach in a sentence, for the decision; once it
                    // runs the steps say it.
                    if phase.awaiting, let summary = nonEmpty(work.summary) {
                        BotMarkdown(text: summary, cacheKey: "work-summary-\(work.id)")
                            .foregroundStyle(Theme.textMuted)
                            .padding(.vertical, 4)
                    }
                    WorkFlow(work: work, shared: shared != nil, details: text.details, loadingSteps: stepsCut)
                        .padding(.vertical, 6)
                } header: {
                    // A heading only where a result stands above the steps.
                    if result != nil { ListSectionHeader(title: "Steps") }
                }
                .groupedRowStyle()
            }
        }
        .toolbar {
            if threadListed, let thread = work.threadChatId {
                ToolbarItem(placement: .topBarTrailing) {
                    // Not a step's session (those open from their steps):
                    // the one that runs the plan and starts them.
                    NavigationLink(value: BotThreadLink(chatId: thread)) {
                        Label("Open the coordinator", systemImage: "point.3.connected.trianglepath.dotted")
                    }
                    .accessibilityHint("The session that runs this plan")
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
                    decisions: [
                        BotDecision(title: "Reject", role: .destructive) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: work.id, approve: false) }
                        },
                        // Reject it and start the reply that asks for the
                        // change (bots.rs `edit_bot_plan`).
                        BotDecision(title: "Edit", enabled: seedComposer != nil) { edit(work) },
                        BotDecision(title: "Approve", role: .primary) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: work.id, approve: true) }
                        },
                    ], busy: actions.busy)
            } else if phase.running {
                // Its time: how long is left, and an hour more while the
                // owner may still add one (work.rs `time_button`).
                let fallback = bot?.plans.timeLimit ?? PlanSettings.defaultTimeLimit
                let left = work.deadlineMs(fallback: fallback).map { max(0, $0 - nowMs()) }
                let canAdd = (work.timeAddedMinutes ?? 0) + 60 <= PlanSettings.maxAddedMinutes
                BotDecisionBar(
                    note: left.map {
                        "\(BotTime.span($0)) left of \(PlanSettings.limitWords(work.limitMinutes(fallback: fallback)))"
                    },
                    decisions: [
                        BotDecision(title: "+1 hour", enabled: canAdd) {
                            actions.run { try await model.workspaceData().addWorkTime(workId: work.id, minutes: 60) }
                        },
                        BotDecision(title: "Stop", role: .destructive) { confirmStop = true },
                    ],
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

    /// A preview the host cut ends in an ellipsis (bot_records.rs `clip`).
    private static func isCut(_ text: String?) -> Bool {
        text?.trimmingCharacters(in: .whitespacesAndNewlines).hasSuffix("…") ?? false
    }

    private func nonEmpty(_ text: String?) -> String? {
        text.flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 }
    }

    private func spaceRow(_ id: String) -> Space? {
        model.spaces.first { $0.id == id }
    }
}

// MARK: - The flow

/// The steps on one rail (work.rs `work_flow`): each with its state, how
/// long it took and what it reported. A step with one session is the way
/// into it; one with several lists them while it is running, asking or
/// failed, and otherwise keeps to a line that a tap opens. A plan still to
/// approve shows every step's description. Where the steps work in
/// different projects each names its own, quietly before its time.
struct WorkFlow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let work: BotWork
    /// The page's head already names the one project.
    var shared = false
    /// Each step's planned detail as the page shows it (`PlanText`: the full
    /// text once its body is here); nil reads the row's.
    var details: [String?]?
    /// Steps whose cut detail says the full plan is still loading.
    var loadingSteps: Set<Int> = []
    /// Steps the owner opened or closed; the rest follow their state.
    @State private var opened: [Int: Bool] = [:]

    /// Whether a step shows its details before anyone tapped it: every step
    /// of a plan still to read, else only one that is under way or went
    /// wrong (work.rs `step_opens`).
    static func stepOpens(_ state: StepState, numbered: Bool) -> Bool {
        numbered || [.running, .asking, .failed].contains(state)
    }

    /// Whether the steps show their planned details (numbered) rather than
    /// what they reported.
    static func showsPlan(_ phase: PlanPhase) -> Bool {
        [PlanPhase.awaiting, .starting, .cancelling, .cancelled].contains(phase)
    }

    var body: some View {
        let numbered = Self.showsPlan(work.phase)
        let last = work.steps.count - 1
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(work.steps.enumerated()), id: \.offset) { ix, step in
                node(segment: ix < last ? segment(step.state) : nil) {
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
                    stepContent(ix, step, numbered: numbered)
                }
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
    private func stepContent(_ ix: Int, _ step: WorkStep, numbered: Bool) -> some View {
        let space = shared ? nil : step.project.flatMap { id in model.spaces.first { $0.id == id } }
        let planned = details.map { $0.indices.contains(ix) ? $0[ix] : nil } ?? step.detail
        let said = (numbered ? planned : step.note).flatMap {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0
        }
        // While it waits, what its sessions run on, when the plan chose it.
        let runs = numbered ? step.sessionChoice.words().map { "Its sessions: \($0)." } : nil
        let detail: String? =
            switch (said, runs) {
            case (let said?, let runs?): "\(said)\n\n\(runs)"
            case (let said, let runs): said ?? runs
            }
        // A step with one session is that session: its row would say the
        // step's title and state again, so the step is the way in.
        let link = step.sessions.count == 1 ? step.sessions.first.flatMap { model.chat(id: $0)?.id } : nil
        let sessions = link == nil ? step.sessions : []
        let foldable = link == nil && (detail != nil || !sessions.isEmpty)
        let byState = Self.stepOpens(step.state, numbered: numbered)
        let isOpen = link != nil ? byState : foldable && (opened[ix] ?? byState)
        let waiting = !numbered && step.state == .pending
        // Where it works and how long it took, in one quiet note.
        let note = [
            space.map { model.placeName($0, for: model.bot(id: work.botId)) },
            numbered ? nil : step.span(),
        ].compactMap { $0 }.joined(separator: " · ")
        VStack(alignment: .leading, spacing: 6) {
            Button {
                if let link {
                    openChat?(link)
                } else {
                    withAnimation(Motion.fadeQuick) { opened[ix] = !isOpen }
                }
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(step.title)
                        .font(Theme.sans(15, weight: .medium, relativeTo: .body))
                        .foregroundStyle(waiting || step.state == .skipped ? Theme.textMuted : Theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .multilineTextAlignment(.leading)
                    if !note.isEmpty {
                        Text(note)
                            .font(Theme.sans(13, relativeTo: .subheadline))
                            .foregroundStyle(Theme.textFaint)
                            .monospacedDigit()
                            .lineLimit(1)
                    }
                    Image(systemName: link != nil ? "arrow.up.forward" : "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.textFaint)
                        .rotationEffect(.degrees(link == nil && !isOpen ? -90 : 0))
                        .opacity(link != nil || foldable ? 1 : 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.borderless)
            .disabled(link == nil && !foldable)
            .accessibilityLabel("Step \(ix + 1): \(step.title), \(numbered ? "planned" : step.state.status.text)")
            .accessibilityHint(
                link != nil ? "Opens its session" : foldable ? (isOpen ? "Hides its details" : "Shows its details") : ""
            )
            .accessibilityIdentifier("work-step-\(ix)")
            if isOpen {
                if numbered, loadingSteps.contains(ix), let said {
                    // The loading line right under the cut text, before
                    // what its sessions run on.
                    BotMarkdown(text: said, cacheKey: "work-step-\(work.id)-\(ix)")
                        .foregroundStyle(Theme.textMuted)
                    PlanLoadingLine(part: .plan)
                    if let runs {
                        BotMarkdown(text: runs, cacheKey: "work-step-\(work.id)-\(ix)-runs")
                            .foregroundStyle(Theme.textMuted)
                    }
                } else if let detail {
                    BotMarkdown(text: detail, cacheKey: "work-step-\(work.id)-\(ix)")
                        .foregroundStyle(step.state == .asking ? Theme.warning : Theme.textMuted)
                }
                ForEach(sessions, id: \.self) { chatId in
                    sessionRow(chatId)
                }
            } else if let detail {
                // Closed, it keeps what it reported to one line.
                Text(singleLine(plainMarkdown(detail)))
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
    }

    /// A session a step started: its state as a mark, its title, and a tap
    /// that opens it (work.rs `flow_session`).
    @ViewBuilder
    private func sessionRow(_ chatId: String) -> some View {
        if let chat = model.chat(id: chatId) {
            Button {
                openChat?(chatId)
            } label: {
                HStack(spacing: 8) {
                    WorkspaceSessionMark(indicator: model.indicator(for: chat))
                        .frame(width: 16)
                    Text(singleLine(plainMarkdown(chat.displayTitle)))
                        .font(Theme.sans(14, relativeTo: .subheadline))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
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
}

/// The quiet line under a preview whose full text is still on its way
/// (plan-body.md §5): the owner may still decide on the preview (D5).
struct PlanLoadingLine: View {
    enum Part { case plan, result }
    let part: Part

    var body: some View {
        Text(part == .plan ? "Loading the full plan…" : "Loading the full result…")
            .font(Theme.sans(13, relativeTo: .subheadline))
            .foregroundStyle(Theme.textFaint)
            .accessibilityIdentifier(part == .plan ? "work-plan-loading" : "work-result-loading")
    }
}

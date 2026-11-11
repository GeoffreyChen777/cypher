// What needs the owner (needs.rs): the Needs you list, most pressing first,
// each row opening the page that settles it and, where the decision is one
// tap, offering it in place (swipe or long press); an action awaiting
// approval; and a session the workspace started that is asking.

import SwiftUI

/// One Needs you row in a list: it opens its page (the workspace's own
/// question opens its chat), and approvals and plans decide in place.
struct NeedsYouRowLink: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let row: BoardNeedRow
    let actions: BotActionRunner

    var body: some View {
        Group {
            if let page = BotPage.settling(row.item) {
                NavigationLink(value: page) { NeedRowView(row: row) }
            } else {
                Button {
                    if case .chat(let chatId) = row.item { openChat?(chatId) }
                } label: {
                    NeedRowView(row: row)
                }
                .tint(Theme.text)
            }
        }
        .accessibilityIdentifier("need-\(row.id)")
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            ForEach(decisions, id: \.title) { decision in
                Button(decision.title, systemImage: decision.symbol, role: decision.role) {
                    actions.run(decision.perform)
                }
                .tint(decision.role == .destructive ? Theme.danger : Theme.success)
            }
        }
        .contextMenu {
            ForEach(decisions.reversed(), id: \.title) { decision in
                Button(decision.title, systemImage: decision.symbol, role: decision.role) {
                    actions.run(decision.perform)
                }
            }
        }
    }

    private struct Decision {
        let title: String
        let symbol: String
        var role: ButtonRole?
        let perform: @MainActor () async throws -> Void
    }

    /// The decisions one tap settles: an approval's and a plan's.
    private var decisions: [Decision] {
        switch row.item {
        case .approval(let id):
            return [
                Decision(title: "Approve", symbol: "checkmark") {
                    try await model.workspaceData().resolveApproval(approvalId: id, approve: true)
                },
                Decision(title: "Decline", symbol: "xmark", role: .destructive) {
                    try await model.workspaceData().resolveApproval(approvalId: id, approve: false)
                },
            ]
        case .plan(let id):
            return [
                Decision(title: "Approve", symbol: "checkmark") {
                    try await model.workspaceData().resolvePlan(workId: id, approve: true)
                },
                Decision(title: "Cancel Plan", symbol: "xmark", role: .destructive) {
                    try await model.workspaceData().resolvePlan(workId: id, approve: false)
                },
            ]
        default:
            return []
        }
    }
}

struct NeedsYouPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var actions = BotActionRunner()

    var body: some View {
        let rows = model.bot(id: botId).flatMap { bot in model.data?.boardNeedRows(bot) } ?? []
        BotPageList(title: "Needs you") {
            ForEach(rows) { row in
                NeedsYouRowLink(row: row, actions: actions)
                    .groupedRowStyle()
            }
        }
        .overlay {
            if rows.isEmpty {
                BotEmptyState(
                    symbol: "checkmark.circle", title: "Nothing needs you",
                    hint: "Plans, approvals, questions and answers to review land here.")
            }
        }
        .botActionAlert(actions)
    }
}

// MARK: - Approval

/// An action a background session waits on: what it is, why it needs you
/// and where it comes from; Decline / Approve at the foot.
struct ApprovalPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let approvalId: String
    @State private var actions = BotActionRunner()

    var body: some View {
        if let approval = model.data?.approval(id: approvalId) {
            page(approval)
        } else {
            BotEmptyState(
                symbol: "checkmark.shield", title: "Approval gone",
                hint: "It was decided or removed on another device.")
        }
    }

    private func page(_ approval: BotApproval) -> some View {
        let work = approval.workId.flatMap { model.data?.work(id: $0) }
        let thread = model.chat(id: approval.threadChatId)
        return BotPageList(title: "Approval") {
            Section {
                BotPageHeader(
                    title: singleLine(approval.action), symbol: "checkmark.shield",
                    tone: approval.awaiting ? .waiting : .muted, lines: nil)
            }
            .groupedRowStyle()
            Section {
                BotFact(status: approval.status)
                    .accessibilityIdentifier("approval-status")
                BotFact(label: "Why", value: approval.reason)
                BotFact(label: "Asked", value: BotTime.past(approval.createdAt))
            }
            .groupedRowStyle()
            Section {
                if let work {
                    NavigationLink(value: BotPage.workItem(work.id)) {
                        BoardRow(title: singleLine(work.title), sub: work.phaseLine()) { WorkMark(work: work) }
                    }
                    .accessibilityIdentifier("approval-work")
                }
                if thread != nil {
                    NavigationLink(value: BotThreadLink(chatId: approval.threadChatId)) {
                        BoardRow(title: "Open session", sub: "Where it asked, read-only") {
                            BoardGlyph(symbol: "bubble.left.and.text.bubble.right")
                        }
                    }
                    .accessibilityIdentifier("approval-thread")
                }
            } header: {
                if work != nil || thread != nil { ListSectionHeader(title: "From") }
            }
            .groupedRowStyle()
        }
        .safeAreaInset(edge: .bottom) {
            if approval.awaiting {
                BotDecisionBar(
                    decisions: [
                        BotDecision(title: "Decline", role: .destructive) {
                            actions.run {
                                try await model.workspaceData().resolveApproval(approvalId: approval.id, approve: false)
                            }
                        },
                        BotDecision(title: "Approve", role: .primary) {
                            actions.run {
                                try await model.workspaceData().resolveApproval(approvalId: approval.id, approve: true)
                            }
                        },
                    ], busy: actions.busy)
            }
        }
        .botActionAlert(actions)
    }
}

// MARK: - Asking

/// A session the workspace started, asking: where it runs and for which
/// work, its question (a single question with options answers here), its
/// latest messages, and the way into the session.
struct AskingPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let botId: String
    let chatId: String
    let workId: String
    @State private var answerError: String?
    @State private var answered: Set<String> = []

    var body: some View {
        if let chat = model.chat(id: chatId) {
            page(chat, store: model.sessionStore(for: chat))
        } else {
            BotEmptyState(
                symbol: "questionmark.circle", title: "Not on this device yet",
                hint: "The session syncs from the device it runs on.")
        }
    }

    private func page(_ chat: Chat, store: SessionStore?) -> some View {
        let asking = model.indicator(for: chat) == .awaitingInput
        let space = chat.spaceId.flatMap { id in model.spaces.first { $0.id == id } }
        let work = model.data?.work(id: workId)
        let request = store?.openInputRequest.flatMap { answered.contains($0.requestId) ? nil : $0 }
        return BotPageList(title: "Asking") {
            Section {
                BotPageHeader(
                    title: singleLine(chat.displayTitle), symbol: "questionmark.circle",
                    tone: asking ? .waiting : .muted)
            }
            .groupedRowStyle()
            Section {
                if let space {
                    BotFact(label: "Project", value: "\(space.displayName) · \(model.deviceName(space.deviceId))")
                }
                if let work {
                    NavigationLink(value: BotPage.workItem(work.id)) {
                        BotFact(label: "From", value: singleLine(work.title))
                    }
                } else {
                    BotFact(label: "From", value: model.bot(id: botId)?.name ?? "The workspace")
                }
                if request == nil {
                    if !answered.isEmpty {
                        BotFact(status: BotStatusLabel("Answer sent", .done))
                    } else if store?.entries.isEmpty == false || !asking {
                        BotFact(status: BotStatusLabel("Nothing to answer right now", .done))
                    } else {
                        BotFact(label: "Status", value: "Loading the session…")
                    }
                }
            }
            .groupedRowStyle()
            if let request, let store {
                questionSection(request, store: store, canAnswer: canAnswer(chat))
            }
            if let store {
                let recent = recentMessages(store.entries)
                if !recent.isEmpty {
                    Section {
                        ForEach(Array(recent.enumerated()), id: \.offset) { _, message in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(message.fromWorkspace ? (model.bot(id: botId)?.name ?? "Workspace") : "Session")
                                    .font(Theme.sans(12, weight: .medium, relativeTo: .caption))
                                    .foregroundStyle(Theme.textMuted)
                                Text(message.text)
                                    .font(Theme.sans(14, relativeTo: .body))
                                    .foregroundStyle(Theme.text)
                                    .lineLimit(6)
                            }
                            .padding(.vertical, 2)
                        }
                    } header: {
                        ListSectionHeader(title: "Latest in the session")
                    }
                    .groupedRowStyle()
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            BotDecisionBar(
                note: request != nil && !simple(request) ? "Answer it in the session." : nil,
                decisions: [BotDecision(title: "Open session", role: .primary) { openChat?(chatId) }])
        }
        .alert(
            "Couldn't answer",
            isPresented: Binding(get: { answerError != nil }, set: { if !$0 { answerError = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(answerError ?? "")
        }
    }

    private typealias InputRequest = (entryId: String, requestId: String, questions: [UserInputQuestion])

    /// One question with options, picked from one: answered here.
    private func simple(_ request: InputRequest?) -> Bool {
        guard let request, request.questions.count == 1, let question = request.questions.first else { return false }
        return question.multiSelect != true && question.permission == nil && !question.options.isEmpty
    }

    @ViewBuilder
    private func questionSection(_ request: InputRequest, store: SessionStore, canAnswer: Bool) -> some View {
        let question = request.questions[0]
        let text = question.question.trimmingCharacters(in: .whitespacesAndNewlines)
        Section {
            Text(text.isEmpty ? question.header : text)
                .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .padding(.vertical, 2)
            if simple(request) {
                ForEach(Array(question.options.enumerated()), id: \.offset) { ix, option in
                    Button {
                        if store.respondInput(
                            requestId: request.requestId,
                            answers: [UserInputAnswer(questionId: question.id, labels: [option])])
                        {
                            answered.insert(request.requestId)
                        } else {
                            answerError = "Couldn't queue your answer. Please retry."
                        }
                    } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Text("\(ix + 1)")
                                .font(Theme.sans(12, weight: .semibold, relativeTo: .caption))
                                .foregroundStyle(Theme.textMuted)
                                .frame(width: 22, height: 22)
                                .background(Theme.text.opacity(0.06), in: RoundedRectangle(cornerRadius: 6))
                            Text(option)
                                .font(Theme.sans(15, relativeTo: .body))
                                .foregroundStyle(Theme.text)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .disabled(!canAnswer)
                    .accessibilityIdentifier("asking-option-\(ix)")
                }
            }
        } header: {
            ListSectionHeader(title: simple(request) ? "Answer" : "Question")
        }
        .groupedRowStyle()
    }

    private func canAnswer(_ chat: Chat) -> Bool {
        chat.config?.harness == "pi" && (model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId)))
    }
}

/// The last `count` messages of a session with text, oldest first, as (sent
/// by the workspace, text): one plain paragraph each, clipped (needs.rs
/// `recent_messages`).
func recentMessages(_ entries: [MessageEntry], count: Int = 3) -> [(fromWorkspace: Bool, text: String)] {
    var out: [(fromWorkspace: Bool, text: String)] = []
    for entry in entries.reversed() where out.count < count {
        let fromWorkspace: Bool
        switch entry.role {
        case .user: fromWorkspace = true
        case .assistant: fromWorkspace = false
        default: continue
        }
        let text = entry.parts.compactMap { part -> String? in
            if case .text(_, let text, _) = part { return text }
            return nil
        }.joined(separator: " ")
        let plain = singleLine(plainMarkdown(text))
        if !plain.isEmpty { out.append((fromWorkspace, clip(plain, max: 280))) }
    }
    return out.reversed()
}

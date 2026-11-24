// What needs the owner (needs.rs), in one place: the Needs you page stacks
// a card per item (NeedsYouCard.swift), most pressing first, each with its
// decision in it: an action or a plan to approve, a permission prompt or a
// question from a session the workspace runs (a plan's worker, a routine),
// answered where it stands. A card's head still opens the item's own page
// for its context (the plan's steps, the session, the drafted answer to
// review), which leads with the same card.

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
                Decision(title: "Reject", symbol: "xmark", role: .destructive) {
                    try await model.workspaceData().resolvePlan(workId: id, approve: false)
                },
            ]
        default:
            return []
        }
    }
}

/// The page: a card per item, most pressing first, each answered in place
/// (needs.rs `render_needs_page`). A card's head opens the item's page.
struct NeedsYouPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var actions = BotActionRunner()
    @State private var answered: Set<String> = []

    var body: some View {
        let rows = model.bot(id: botId).flatMap { bot in model.data?.boardNeedRows(bot) } ?? []
        BotCardPage(title: "Needs you") {
            ForEach(rows) { row in
                NeedsYouItemCard(row: row, actions: actions, answered: $answered)
            }
        }
        .overlay {
            if rows.isEmpty {
                BotEmptyState(
                    symbol: "checkmark.circle", title: "Nothing needs you",
                    hint: "Approvals, permission prompts, questions and answers to review land here.")
            }
        }
        .botActionAlert(actions)
    }
}

/// One item's card on the Needs you page, with its decision (needs.rs
/// `need_content`): an approval's or a plan's buttons, a session's question
/// answered here, the way into the chat or to the answer to review.
struct NeedsYouItemCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let row: BoardNeedRow
    let actions: BotActionRunner
    @Binding var answered: Set<String>

    var body: some View {
        switch row.item {
        case .approval(let id):
            card {
                if let reason = model.data?.approval(id: id)?.reason, !reason.isEmpty {
                    NeedsCardCopy(text: reason)
                }
            } actions: {
                NeedsCardDecisions(
                    refusals: [
                        BotDecision(title: "Decline", role: .destructive) {
                            actions.run {
                                try await model.workspaceData().resolveApproval(approvalId: id, approve: false)
                            }
                        }
                    ],
                    approvals: [
                        BotDecision(title: "Approve", role: .primary) {
                            actions.run {
                                try await model.workspaceData().resolveApproval(approvalId: id, approve: true)
                            }
                        }
                    ], busy: actions.busy)
            }
        case .plan(let id):
            card {
                if let summary = model.data?.work(id: id)?.summary.map(singleLine), !summary.isEmpty {
                    NeedsCardCopy(text: summary, lines: 3)
                }
            } actions: {
                NeedsCardDecisions(
                    refusals: [
                        BotDecision(title: "Reject", role: .destructive) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: id, approve: false) }
                        }
                    ],
                    approvals: [
                        BotDecision(title: "Approve", role: .primary) {
                            actions.run { try await model.workspaceData().resolvePlan(workId: id, approve: true) }
                        }
                    ], busy: actions.busy)
            }
        case .asking(let chatId, _):
            if let chat = model.chat(id: chatId), let store = model.sessionStore(for: chat) {
                AskingCard(
                    chat: chat, store: store, source: row.source, at: row.at, link: link, linkId: linkId,
                    answered: $answered)
            } else {
                card { NeedsCardNote(text: "The session syncs from the device it runs on.") }
            }
        case .chat(let chatId):
            card {
                if !row.sub.isEmpty { NeedsCardCopy(text: row.sub, lines: 3) }
            } actions: {
                NeedsCardDecisions(
                    refusals: [],
                    approvals: [BotDecision(title: "Answer in the chat", role: .primary) { openChat?(chatId) }])
            }
        case .review(let id):
            card {
                EmptyView()
            } actions: {
                HStack {
                    Spacer(minLength: 0)
                    NeedsCardLinkButton(title: "Review the answer", page: .exchange(id))
                }
            }
        }
    }

    private var link: NeedsCardLink? {
        if let page = BotPage.settling(row.item) { return .page(page) }
        if case .chat(let chatId) = row.item { return .perform { openChat?(chatId) } }
        return nil
    }

    private var linkId: String { "need-\(row.id)" }

    private func card<Content: View, Actions: View>(
        @ViewBuilder content: () -> Content, @ViewBuilder actions: () -> Actions
    ) -> some View {
        NeedsYouCard(
            symbol: row.symbol, source: row.source, at: row.at, title: row.title, link: link, linkId: linkId,
            content: content, actions: actions)
    }

    private func card<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        card(content: content, actions: { EmptyView() })
    }
}

// MARK: - A session's question, answered in place

/// The open question of a session the workspace runs, as its card, answered
/// where it stands (needs.rs `asking_content`): a permission prompt titled
/// by its question, with what it asks about and the extension's own
/// options; one question with answers to pick, each a button; anything
/// else as the question and the way into the session.
struct AskingCard: View {
    typealias Request = (entryId: String, requestId: String, questions: [UserInputQuestion])

    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let chat: Chat
    let store: SessionStore
    let source: String
    var at: Int64?
    var link: NeedsCardLink?
    var linkId: String?
    /// On the session's own page: the title in full, and no way into the
    /// session (the page's bar has it).
    var onPage = false
    /// Requests answered from here, until the session says so.
    @Binding var answered: Set<String>
    @State private var answerError: String?

    var body: some View {
        let request = store.openInputRequest.flatMap { answered.contains($0.requestId) ? nil : $0 }
        Group {
            if let request, let question = request.questions.first {
                if request.questions.count == 1, let permission = PermissionRequest(question) {
                    card(title: permission.question) {
                        PermissionPanel(
                            requestId: request.requestId, question: question, request: permission,
                            canRespond: canAnswer, style: .embedded
                        ) { requestId, answers in
                            respond(requestId, answers)
                        }
                    }
                } else {
                    questionCard(request, question)
                }
            } else {
                card(title: singleLine(chat.displayTitle)) {
                    NeedsCardNote(text: status)
                }
            }
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

    /// A question: its headline as the title, its context under it, and
    /// its answers to pick, or the way into the session.
    @ViewBuilder
    private func questionCard(_ request: Request, _ question: UserInputQuestion) -> some View {
        let shown = QuestionPresentation(question)
        let prompt = shown.prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = prompt.isEmpty ? question.header : prompt
        let context = shown.context?.trimmingCharacters(in: .whitespacesAndNewlines)
        if Self.simple(request.questions) {
            NeedsYouCard(
                symbol: "questionmark.circle", source: source, at: at, title: title, titleLines: titleLines,
                link: link, linkId: linkId
            ) {
                if let context, !context.isEmpty { NeedsCardCopy(text: context) }
            } actions: {
                options(request, question)
            }
        } else if onPage {
            card(title: title) {
                VStack(alignment: .leading, spacing: 8) {
                    if let context, !context.isEmpty { NeedsCardCopy(text: context) }
                    NeedsCardNote(text: note(request))
                }
            }
        } else {
            NeedsYouCard(
                symbol: "questionmark.circle", source: source, at: at, title: title, titleLines: titleLines,
                link: link, linkId: linkId
            ) {
                VStack(alignment: .leading, spacing: 8) {
                    if let context, !context.isEmpty { NeedsCardCopy(text: context) }
                    NeedsCardNote(text: note(request))
                }
            } actions: {
                HStack {
                    Spacer(minLength: 0)
                    BotCardButton(title: "Answer in the session", role: .primary) { openChat?(chat.id) }
                        .accessibilityIdentifier("asking-open")
                }
            }
        }
    }

    /// A question's answers as buttons: on one line when they fit, else
    /// one under another.
    private func options(_ request: Request, _ question: UserInputQuestion) -> some View {
        let options = Array(question.options.enumerated())
        func button(_ ix: Int, _ option: String, fill: Bool) -> some View {
            NeedsCardOutlineButton(title: option, fill: fill) {
                respond(request.requestId, [UserInputAnswer(questionId: question.id, labels: [option])])
            }
            .disabled(!canAnswer)
            .accessibilityIdentifier("asking-option-\(ix)")
        }
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) {
                ForEach(options, id: \.offset) { ix, option in button(ix, option, fill: false) }
                Spacer(minLength: 0)
            }
            VStack(spacing: 8) {
                ForEach(options, id: \.offset) { ix, option in button(ix, option, fill: true) }
            }
        }
    }

    private func note(_ request: Request) -> String {
        request.questions.count == 1 ? "This one takes more than a pick." : "It asks several things at once."
    }

    private var titleLines: Int? { onPage ? nil : 3 }

    /// What the card says with no question open.
    private var status: String {
        if !answered.isEmpty { return "Answer sent." }
        if !store.entries.isEmpty || model.indicator(for: chat) != .awaitingInput {
            return "Nothing to answer right now."
        }
        return "Loading the question…"
    }

    private func card<Content: View>(title: String, @ViewBuilder content: () -> Content) -> some View {
        NeedsYouCard(
            symbol: "questionmark.circle", source: source, at: at, title: title, titleLines: titleLines, link: link,
            linkId: linkId, content: content)
    }

    private func respond(_ requestId: String, _ answers: [UserInputAnswer]) {
        if store.respondInput(requestId: requestId, answers: answers) {
            answered.insert(requestId)
        } else {
            answerError = "Couldn't queue your answer. Please retry."
        }
    }

    /// One question answered by one pick here: a permission prompt, or a
    /// question with options to choose one of.
    static func simple(_ questions: [UserInputQuestion]) -> Bool {
        guard questions.count == 1, let question = questions.first else { return false }
        if PermissionRequest(question) != nil { return true }
        return question.multiSelect != true && !question.options.isEmpty
    }

    private var canAnswer: Bool {
        chat.config?.harness == "pi" && (model.demo != nil || (model.connected && model.deviceOnline(chat.deviceId)))
    }
}

// MARK: - Approval

/// An action a background session waits on, as its card: what it is, why
/// it needs you and its state; where it comes from under it; Decline /
/// Approve at the foot.
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
        return BotCardPage(title: "Approval") {
            NeedsYouCard(
                symbol: "checkmark.shield", tint: approval.awaiting ? Theme.warning : Theme.textMuted,
                // Where it comes from is the From card's: the head says what it is.
                source: "Action to approve", at: approval.createdAt, title: singleLine(approval.action), titleLines: nil
            ) {
                VStack(alignment: .leading, spacing: 10) {
                    if !approval.reason.isEmpty { NeedsCardCopy(text: approval.reason, lines: nil) }
                    BotStatusText(approval.status, size: 14)
                        .accessibilityElement(children: .combine)
                        .accessibilityIdentifier("approval-status")
                }
            }
            if work != nil || thread != nil {
                BotCardLabel(title: "From")
                VStack(alignment: .leading, spacing: 0) {
                    if let work {
                        NavigationLink(value: BotPage.workItem(work.id)) {
                            BoardRow(title: singleLine(work.title), sub: work.phaseLine()) { WorkMark(work: work) }
                                .cardLinkRow()
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("approval-work")
                    }
                    if work != nil, thread != nil { CardDivider() }
                    if thread != nil {
                        NavigationLink(value: BotThreadLink(chatId: approval.threadChatId)) {
                            BoardRow(title: "Open session", sub: "Where it asked, read-only") {
                                BoardGlyph(symbol: "bubble.left.and.text.bubble.right")
                            }
                            .cardLinkRow()
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("approval-thread")
                    }
                }
                .needsCard(padding: 0)
            }
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

/// A hairline between the rows of a card.
struct CardDivider: View {
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        Rectangle().fill(Theme.border).frame(height: 1 / displayScale).padding(.leading, NeedsCardMetrics.pad)
    }
}

extension View {
    /// A row of a card that opens something: its padding and a chevron.
    func cardLinkRow() -> some View {
        HStack(spacing: 8) {
            self
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(Theme.textFaint)
                .accessibilityHidden(true)
        }
        .padding(.horizontal, NeedsCardMetrics.pad)
        .padding(.vertical, 10)
        .contentShape(Rectangle())
    }
}

// MARK: - Asking

/// A session the workspace runs, asking: its question as its card (a
/// permission prompt or a single question with options answers here),
/// where it runs and for which work, its latest messages, and the way into
/// the session.
struct AskingPage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openBotChat) private var openChat
    let botId: String
    let chatId: String
    let workId: String?
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
        let space = chat.spaceId.flatMap { id in model.spaces.first { $0.id == id } }
        let work = workId.flatMap { model.data?.work(id: $0) }
        // The work and the project are the facts card's: the head names the
        // session.
        let source = singleLine(chat.displayTitle)
        return BotCardPage(title: "Asking") {
            if let store {
                AskingCard(
                    chat: chat, store: store, source: source, at: chat.lastMessageAt, onPage: true,
                    answered: $answered)
            } else {
                NeedsYouCard(
                    symbol: "questionmark.circle", source: source, title: singleLine(chat.displayTitle),
                    titleLines: nil
                ) {
                    NeedsCardNote(text: "Loading the session…")
                }
            }
            VStack(alignment: .leading, spacing: 0) {
                if let space {
                    BotFact(label: "Project", value: "\(space.displayName) · \(model.deviceName(space.deviceId))")
                        .padding(.horizontal, NeedsCardMetrics.pad)
                        .padding(.vertical, 11)
                    CardDivider()
                }
                if let work {
                    NavigationLink(value: BotPage.workItem(work.id)) {
                        BotFact(label: "From", value: singleLine(work.title)).cardLinkRow()
                    }
                    .buttonStyle(.plain)
                } else {
                    BotFact(label: "From", value: model.bot(id: botId)?.name ?? "The workspace")
                        .padding(.horizontal, NeedsCardMetrics.pad)
                        .padding(.vertical, 11)
                }
            }
            .needsCard(padding: 0)
            if let store {
                let recent = recentMessages(store.entries)
                if !recent.isEmpty {
                    BotCardLabel(title: "Latest in the session")
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(recent.enumerated()), id: \.offset) { ix, message in
                            if ix > 0 { CardDivider() }
                            VStack(alignment: .leading, spacing: 3) {
                                Text(message.fromWorkspace ? (model.bot(id: botId)?.name ?? "Workspace") : "Session")
                                    .font(Theme.sans(12, weight: .medium, relativeTo: .caption))
                                    .foregroundStyle(Theme.textMuted)
                                Text(message.text)
                                    .font(Theme.sans(14, relativeTo: .body))
                                    .foregroundStyle(Theme.text)
                                    .lineLimit(6)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, NeedsCardMetrics.pad)
                            .padding(.vertical, 11)
                        }
                    }
                    .needsCard(padding: 0)
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            BotDecisionBar(
                decisions: [BotDecision(title: "Open session", role: .primary) { openChat?(chatId) }])
        }
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

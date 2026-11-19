// A workspace's Connections (crates/ui/src/shell/bots/network.rs): the
// connected workspaces, one row each; one connection (how it stands, who it
// is and what it can read, its reply mode, the questions between the two,
// Disconnect); one question, which opens as its review while a drafted
// answer waits on the owner; and the connect page (create an invite code
// and see the ones still open, or paste one).

import SwiftUI

// MARK: - List

struct ConnectionListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var connecting = false

    var body: some View {
        let bot = model.bot(id: botId)
        let rows = model.data?.boardConnectionRows(botId) ?? []
        List {
            Section {
                ForEach(rows) { row in
                    NavigationLink(value: BotPage.connection(row.connection.id)) {
                        ConnectionRow(row: row)
                    }
                    .accessibilityIdentifier("connection-row-\(row.connection.id)")
                }
            }
            .groupedRowStyle()
        }
        .botPageStyle(title: "Connections")
        .overlay {
            if rows.isEmpty {
                BotEmptyState(
                    symbol: "person.2", title: "No connections yet",
                    hint:
                        "Invite someone with a code, or use theirs. Connected workspaces can ask \(bot?.name ?? "this workspace") questions, read-only, within what you allow.",
                    action: ("Connect a workspace", { connecting = true }))
            }
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                NavigationLink(value: BotPage.connect) {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("Connect a workspace")
                .accessibilityIdentifier("connections-connect")
            }
        }
        .navigationDestination(isPresented: $connecting) { ConnectPage(botId: botId) }
    }
}

/// The ring of a question in flight: going round, or still once its wait
/// on the other side has gone on (network.rs `flight_ring`). It looks
/// again every few seconds, so the switch needs no change in the data.
struct FlightRing: View {
    /// Whether anything of it still moves at `now`.
    let moving: (Int64) -> Bool

    var body: some View {
        TimelineView(.periodic(from: .now, by: 5)) { context in
            ProgressRing(
                state: moving(Int64(context.date.timeIntervalSince1970 * 1000)) ? .running : .waiting)
        }
    }
}

/// A connected workspace (network.rs `render_network_card`): its state as
/// its mark (the amber paper plane when an answer waits for review, the
/// ring while a question is on its way, else the person's photo), who it
/// is with their workspace's name quieter beside it, and one quiet note:
/// when the last question was, or that replies are off.
struct ConnectionRow: View {
    @Environment(AppModel.self) private var model
    let row: BoardConnectionRow

    var body: some View {
        let connection = row.connection
        let (who, theirs) = connection.peer.names
        HStack(alignment: .center, spacing: 10) {
            Group {
                switch row.standing {
                case .review: BoardGlyph(symbol: reviewSymbol, color: Theme.warning)
                case .answering, .asking:
                    FlightRing { now in
                        (model.data?.exchanges(of: connection.botId, connectionId: connection.id) ?? [])
                            .contains { $0.moving(now: now) }
                    }
                default: PeerAvatar(peer: connection.peer, size: 18)
                }
            }
            .frame(width: 18)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(who)
                    .font(Theme.sans(16, weight: .medium, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .layoutPriority(1)
                if let theirs {
                    Text("· \(theirs)")
                        .font(Theme.sans(16, relativeTo: .body))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            .lineLimit(1)
            .frame(maxWidth: .infinity, alignment: .leading)
            if let note = connection.note(row.standing) {
                Text(note)
                    .font(Theme.sans(13, relativeTo: .subheadline))
                    .foregroundStyle(Theme.textFaint)
                    .monospacedDigit()
                    .fixedSize()
            }
        }
        .opacity([.off, .leaving].contains(row.standing) ? 0.55 : 1)
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

/// An open invite on the connect page: when it was made, how long it has
/// left, and Revoke.
private struct InviteRow: View {
    let invite: BotInvite
    let revoke: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            BoardGlyph(symbol: "key")
                .frame(width: 18)
            Text(invite.title())
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(invite.detail())
                .font(Theme.sans(13, relativeTo: .footnote))
                .foregroundStyle(Theme.textFaint)
                .lineLimit(1)
            if invite.revokeRequestedAt == nil {
                Button("Revoke", action: revoke)
                    .buttonStyle(.borderless)
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("invite-revoke-\(invite.id)")
            }
        }
        .padding(.vertical, 2)
    }
}

/// One question on a connection's page (network.rs `exchange_item`): which
/// way it went, the question, and at the right how it stands (the amber
/// paper plane when its answer waits for review, the ring while it is on
/// its way, that it was declined or expired) or just when it was asked.
struct ExchangeRow: View {
    let exchange: BotExchange
    /// Who asked whom, for VoiceOver; the arrow says it to the eye.
    let who: String

    var body: some View {
        let ended = [ExchangeState.declined, .expired].contains(exchange.state)
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: exchange.direction == .incoming ? "arrow.down" : "arrow.up")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 18)
                .accessibilityLabel(who)
            Text(exchange.question.replacingOccurrences(of: "\n", with: " "))
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(ended ? Theme.textMuted : Theme.text)
                .lineLimit(2)
                .frame(maxWidth: .infinity, alignment: .leading)
            Group {
                if exchange.awaitingReview {
                    BoardGlyph(symbol: reviewSymbol, color: Theme.warning)
                        .accessibilityLabel("Answer to review")
                } else if exchange.inFlight {
                    FlightRing { exchange.moving(now: $0) }
                        .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
                } else {
                    Text(exchange.settledNote())
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.textFaint)
                        .monospacedDigit()
                }
            }
            .fixedSize()
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Connection

struct ConnectionPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let connectionId: String
    @State private var confirmRevoke = false
    @State private var runner = BotActionRunner()

    var body: some View {
        let connection = model.data?.connection(id: connectionId)
        let bot = model.bot(id: connection?.botId ?? botId)
        List {
            if let connection, let bot {
                content(connection, bot: bot)
            }
        }
        .botPageStyle(title: "Connection")
        .overlay {
            if connection == nil || bot == nil {
                BotEmptyState(
                    symbol: "person.2", title: "Connection gone", hint: "It may have been removed on another device.")
            }
        }
        .toolbar {
            if let connection, connection.isLive, connection.revokeRequestedAt == nil {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Disconnect", systemImage: "trash", role: .destructive) { confirmRevoke = true }
                        .disabled(runner.busy)
                        .accessibilityIdentifier("connection-revoke")
                }
            }
        }
        .confirmationDialog("Disconnect?", isPresented: $confirmRevoke, titleVisibility: .visible) {
            Button("Disconnect", role: .destructive) {
                runner.run { try await model.workspaceData().revokeConnection(connectionId: connectionId) }
            }
        } message: {
            Text(
                "\(connection?.peer.names.who ?? "It") can no longer ask this workspace anything, and this workspace can no longer ask it. Connecting again takes a new invite."
            )
        }
        .botActionAlert(runner)
    }

    @ViewBuilder
    private func content(_ connection: BotConnection, bot: Bot) -> some View {
        let editable = connection.isLive && connection.revokeRequestedAt == nil
        let exchanges = model.data?.exchanges(of: connection.botId, connectionId: connection.id) ?? []
        let standing = ConnectionStanding(connection, exchanges: exchanges)
        let (who, theirs) = connection.peer.names
        let spaces = model.data?.grantedSpaces(of: bot) ?? []
        let scope = ConnectionReach(scope: connection.scope, bot: bot.scope, granted: spaces.map(\.id))
        let reads =
            "can read "
            + connectionReads(
                scope, names: spaces.filter { scope.spaceIds.contains($0.id) }.map(\.displayName),
                all: spaces.count, bot: bot.name)
        // How it stands (since when on a tap), the person, and one line:
        // their workspace and what its questions can read.
        Section {
            BotItemHeader(
                status: connection.status(standing), title: who,
                details: [("Connected", BotTime.past(connection.createdAt))], statusIdentifier: "connection-status"
            ) {
                switch standing {
                case .review: BoardGlyph(symbol: reviewSymbol, color: Theme.warning)
                case .answering, .asking: FlightRing { now in exchanges.contains { $0.moving(now: now) } }
                case .leaving: ProgressRing(state: .stopping)
                default: PeerAvatar(peer: connection.peer, size: 18)
                }
            } line: {
                Text(theirs.map { "\($0) · \(reads)" } ?? reads)
            }
        }
        .groupedRowStyle()
        replies(connection, editable: editable)
        reach(connection, bot: bot, editable: editable)
        questions(connection, exchanges: Array(exchanges.prefix(recentExchangesShown)), bot: bot)
    }

    @ViewBuilder
    private func replies(_ connection: BotConnection, editable: Bool) -> some View {
        Section {
            if editable {
                Picker(
                    "Replies",
                    selection: Binding(
                        get: { connection.replyMode },
                        set: { save(connection, replyMode: $0, scope: connection.scope) })
                ) {
                    ForEach([ReplyMode.auto, .review, .off], id: \.self) { mode in
                        Text(mode.shortLabel).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
                .accessibilityIdentifier("connection-reply-mode")
            } else {
                BotFact(label: "Replies", value: connection.replyMode.label).groupedRowStyle()
            }
        } footer: {
            if editable { Text(connection.replyMode.detail) }
        }
    }

    @ViewBuilder
    private func reach(_ connection: BotConnection, bot: Bot, editable: Bool) -> some View {
        let spaces = model.data?.grantedSpaces(of: bot) ?? []
        let granted = spaces.map(\.id)
        let reach = ConnectionReach(scope: connection.scope, bot: bot.scope, granted: granted)
        let write = { (next: ConnectionReach) in
            save(connection, replyMode: connection.replyMode, scope: next.scope(bot: bot.scope, granted: granted))
        }
        Section {
            if !spaces.isEmpty {
                Picker(
                    "Projects",
                    selection: Binding(
                        get: { reach.sameProjects },
                        set: { same in
                            write(
                                ConnectionReach(sameProjects: same, spaceIds: granted, transcripts: reach.transcripts))
                        })
                ) {
                    Text("All \(bot.name) reads").tag(true)
                    Text("Only some").tag(false)
                }
                .disabled(!editable)
                .accessibilityIdentifier("connection-projects")
                if !reach.sameProjects {
                    ForEach(spaces) { space in
                        let on = reach.spaceIds.contains(space.id)
                        Button {
                            write(reach.toggling(space.id, granted: granted))
                        } label: {
                            HStack {
                                Label(space.displayName, systemImage: "folder")
                                    .foregroundStyle(Theme.text)
                                Spacer()
                                if on {
                                    Image(systemName: "checkmark")
                                        .font(.system(size: 15, weight: .semibold))
                                        .foregroundStyle(Theme.text)
                                }
                            }
                        }
                        .disabled(!editable)
                        .accessibilityAddTraits(on ? .isSelected : [])
                        .accessibilityIdentifier("connection-project-\(space.id)")
                    }
                }
            }
            Toggle(
                "Read conversations",
                isOn: Binding(
                    get: { reach.transcripts },
                    set: { on in
                        var next = reach
                        next.transcripts = on
                        write(next)
                    })
            )
            .disabled(!editable || !bot.scope.transcripts)
            .accessibilityIdentifier("connection-transcripts")
        } header: {
            ListSectionHeader(title: "Can read")
        } footer: {
            Text(reachFooter(reach, bot: bot, spaces: spaces))
        }
        .groupedRowStyle()
    }

    /// What the head does not say about its reach: how deep it reads. A
    /// peer only ever reads.
    private func reachFooter(_ reach: ConnectionReach, bot: Bot, spaces: [Space]) -> String {
        let text =
            !bot.scope.transcripts
            ? "\(bot.name) itself reads only titles and status."
            : reach.transcripts ? "Message text included." : "Titles and status only."
        return text + " Read-only."
    }

    /// The questions between the two, newest first; one opens its page.
    @ViewBuilder
    private func questions(_ connection: BotConnection, exchanges: [BotExchange], bot: Bot) -> some View {
        Section {
            if exchanges.isEmpty {
                Text("No questions yet.").foregroundStyle(Theme.textMuted)
            }
            ForEach(exchanges) { exchange in
                NavigationLink(value: BotPage.exchange(exchange.id)) {
                    ExchangeRow(exchange: exchange, who: exchange.who(peer: connection.peer, bot: bot.name))
                }
                .accessibilityIdentifier("exchange-row-\(exchange.id)")
            }
        } header: {
            ListSectionHeader(title: "Questions")
        }
        .groupedRowStyle()
    }

    private func save(_ connection: BotConnection, replyMode: ReplyMode, scope: BotScope?) {
        runner.run {
            try await model.workspaceData().updateConnection(
                connectionId: connection.id, replyMode: replyMode, scope: scope)
        }
    }
}

// MARK: - Question

/// One question between workspaces: who asks and how it stands, the
/// question, and its answer. While a drafted answer waits on the owner it
/// opens as the review, the answer in an editable well under the question
/// and Decline / Send at the foot, in reach.
struct ExchangePage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let exchangeId: String
    /// The answer as the owner edits it; seeded with the draft.
    @State private var draft: String?
    @State private var runner = BotActionRunner()
    @FocusState private var editing: Bool

    var body: some View {
        let exchange = model.data?.exchange(id: exchangeId)
        let awaiting = exchange?.awaitingReview == true
        List {
            if let exchange {
                content(exchange, awaiting: awaiting)
            }
        }
        .botPageStyle(title: awaiting ? "Review" : "Question")
        .scrollDismissesKeyboard(.interactively)
        .overlay {
            if exchange == nil {
                BotEmptyState(
                    symbol: "bubble.left.and.text.bubble.right", title: "Question gone",
                    hint: "It may have been removed on another device.")
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let exchange, awaiting {
                BotDecisionBar(
                    decisions: [
                        BotDecision(title: "Decline", role: .destructive) { review(exchange, send: false) },
                        BotDecision(
                            title: "Send", role: .primary,
                            enabled: !(draft ?? exchange.answer ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty
                        ) { review(exchange, send: true) },
                    ],
                    busy: runner.busy)
            }
        }
        .onAppear {
            if draft == nil { draft = exchange?.answer ?? "" }
        }
        .botActionAlert(runner)
    }

    @ViewBuilder
    private func content(_ exchange: BotExchange, awaiting: Bool) -> some View {
        let connection = model.data?.connection(id: exchange.connectionId)
        let botName = model.bot(id: exchange.botId)?.name ?? "This workspace"
        let asked = exchange.direction == .incoming ? "asked" : "was asked"
        // How it stands (its times on a tap), who, and which way it went.
        Section {
            BotItemHeader(
                status: awaiting ? BotStatusLabel("Answer to review", .waiting) : exchange.status,
                title: connection?.peer.names.who ?? "A former connection",
                details: times(exchange), statusIdentifier: "exchange-status"
            ) {
                if awaiting {
                    BoardGlyph(symbol: reviewSymbol, color: Theme.warning)
                } else if exchange.inFlight {
                    FlightRing { exchange.moving(now: $0) }
                } else if let peer = connection?.peer {
                    PeerAvatar(peer: peer, size: 18)
                }
            } line: {
                Text(
                    ([connection?.peer.names.theirs].compactMap { $0 }
                        + ["\(asked) \(BotTime.ago(exchange.askedAt))"]).joined(separator: " · "))
            }
            .accessibilityHint(exchange.who(peer: connection?.peer, bot: botName))
        }
        .groupedRowStyle()
        Section {
            BotQuote(text: exchange.question, cacheKey: "exchange-question-\(exchange.id)")
        }
        .groupedRowStyle()
        if awaiting {
            Section {
                TextEditor(
                    text: Binding(
                        get: { draft ?? exchange.answer ?? "" },
                        set: { draft = $0 })
                )
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .scrollContentBackground(.hidden)
                .frame(minHeight: 120, maxHeight: 320)
                .focused($editing)
                .accessibilityLabel("Answer")
                .accessibilityIdentifier("exchange-answer")
            }
            .groupedRowStyle()
        } else if let answer = (exchange.review?.text ?? exchange.answer),
            !answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            Section {
                MemoryMarkdown(text: answer, cacheKey: "exchange-\(exchange.id)")
                    .padding(.vertical, 4)
            }
            .groupedRowStyle()
        }
    }

    /// When it was asked and answered: what a tap on its status shows.
    private func times(_ exchange: BotExchange) -> [(label: String, value: String)] {
        var rows = [("Asked", BotTime.past(exchange.askedAt))]
        if let answeredAt = exchange.answeredAt {
            rows.append((exchange.direction == .incoming ? "Ended" : "Answered", BotTime.past(answeredAt)))
        }
        return rows
    }

    /// Send (with the text only when it was edited) or decline.
    private func review(_ exchange: BotExchange, send: Bool) {
        let original = (exchange.answer ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let text = (draft ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let edited = send && text != original ? text : nil
        editing = false
        runner.run {
            try await model.workspaceData().reviewExchange(exchangeId: exchange.id, send: send, text: edited)
        }
    }
}

// MARK: - Connect

struct ConnectPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var issued: InviteCreated?
    @State private var copied = false
    @State private var code = ""
    /// Why the pasted text isn't a code (checked before any call).
    @State private var codeProblem: String?
    @State private var runner = BotActionRunner()
    /// Which call the runner is making.
    @State private var pending: Call?
    /// The connection a code just made: opened on top of this page.
    @State private var opened: BotPage?

    private enum Call { case create, redeem }

    private func revoke(_ invite: BotInvite) {
        runner.run { try await model.workspaceData().revokeInvite(inviteId: invite.id) }
    }

    var body: some View {
        let bot = model.bot(id: botId)
        let creating = runner.busy && pending == .create
        let redeeming = runner.busy && pending == .redeem
        let invites = model.data?.openInvites(of: botId) ?? []
        List {
            Section {
                if let issued {
                    issuedCode(issued)
                } else {
                    Button {
                        createInvite()
                    } label: {
                        HStack {
                            Label(creating ? "Creating…" : "Create Invite Code", systemImage: "link.badge.plus")
                            Spacer()
                            if creating { ProgressView().controlSize(.small) }
                        }
                    }
                    .disabled(runner.busy || bot == nil)
                    .accessibilityIdentifier("connect-create-invite")
                }
                // The codes still open: an unused one is an open door.
                ForEach(invites) { invite in
                    InviteRow(invite: invite) { revoke(invite) }
                }
            } header: {
                ListSectionHeader(title: "Invite someone")
            } footer: {
                if let issued { Text(issuedInviteNote(issued)) }
            }
            .groupedRowStyle()
            Section {
                TextField("Paste a code", text: $code)
                    .font(Theme.mono(14))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit(redeem)
                    .onChange(of: code) { codeProblem = nil }
                    .accessibilityIdentifier("connect-code")
                Button {
                    redeem()
                } label: {
                    HStack {
                        Text(redeeming ? "Connecting…" : "Connect")
                        Spacer()
                        if redeeming { ProgressView().controlSize(.small) }
                    }
                }
                .disabled(runner.busy || code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("connect-redeem")
            } header: {
                ListSectionHeader(title: "Use their code")
            } footer: {
                if let codeProblem { Text(codeProblem).foregroundStyle(Theme.danger) }
            }
            .groupedRowStyle()
            Section {
            } footer: {
                Text(
                    "Questions and answers pass through Cypher's servers as plain text until delivered. Likely secrets are removed from answers before they leave, without guarantee."
                )
            }
        }
        .botPageStyle(title: "Connect")
        .scrollDismissesKeyboard(.interactively)
        .navigationDestination(item: $opened) { BotPageView(botId: botId, page: $0) }
        .botActionAlert(runner)
    }

    /// The fresh code in a monospaced well, with Copy and Share.
    private func issuedCode(_ issued: InviteCreated) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(issued.code)
                .font(Theme.mono(14))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(Theme.text.opacity(0.05), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Theme.border, lineWidth: 0.5)
                )
                .accessibilityLabel("Invite code")
                .accessibilityValue(issued.code)
                .accessibilityIdentifier("connect-issued-code")
            HStack(spacing: 10) {
                Button {
                    UIPasteboard.general.string = issued.code
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(2))
                        copied = false
                    }
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                .accessibilityIdentifier("connect-copy")
                ShareLink(item: issued.code) {
                    Label("Share", systemImage: "square.and.arrow.up")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                .accessibilityIdentifier("connect-share")
            }
            .font(Theme.sans(15, weight: .medium, relativeTo: .body))
        }
        .padding(.vertical, 6)
    }

    private func createInvite() {
        pending = .create
        runner.run {
            issued = try await model.workspaceData().createInvite(botId: botId)
        }
    }

    private func redeem() {
        let typed = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !runner.busy else { return }
        guard !typed.isEmpty else {
            codeProblem = "Paste the code you were sent."
            return
        }
        guard splitInviteCode(typed) != nil else {
            codeProblem = "That isn't an invite code. It looks like two parts joined by a dot."
            return
        }
        pending = .redeem
        runner.run {
            let connection = try await model.workspaceData().redeemInvite(botId: botId, code: typed)
            code = ""
            opened = .connection(connection.id)
        }
    }
}

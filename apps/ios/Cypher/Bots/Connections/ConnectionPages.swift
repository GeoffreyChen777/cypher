// A workspace's Connections (crates/ui/src/shell/bots/network.rs): the
// connected workspaces, open invites and recent questions; one connection
// (reply mode, what it may read, its questions, Revoke); one question, which
// opens as its review while a drafted answer waits on the owner; and the
// connect page (create an invite code, or paste one).

import SwiftUI

// MARK: - List

struct ConnectionListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var error: String?

    var body: some View {
        let bot = model.bot(id: botId)
        let connections = model.data?.connections(of: botId) ?? []
        let invites = model.data?.openInvites(of: botId) ?? []
        let exchanges = Array((model.data?.exchanges(of: botId) ?? []).prefix(recentExchangesShown))
        let peers = Dictionary(
            model.botRecords.connections.filter { $0.botId == botId }.map { ($0.id, $0.peer) },
            uniquingKeysWith: { first, _ in first })
        List {
            if connections.isEmpty && invites.isEmpty && exchanges.isEmpty {
                emptyState(bot)
            } else {
                Section {
                    if connections.isEmpty {
                        Text("None yet.").foregroundStyle(Theme.textMuted).groupedRowStyle()
                    }
                    ForEach(connections) { connection in
                        NavigationLink(value: BotPage.connection(connection.id)) {
                            ConnectionRow(connection: connection)
                        }
                        .groupedRowStyle()
                    }
                } header: {
                    Text("Workspaces")
                }
                if !invites.isEmpty {
                    Section("Open invites") {
                        ForEach(invites) { invite in
                            InviteRow(invite: invite) { revoke(invite) }
                                .groupedRowStyle()
                                .swipeActions {
                                    if invite.revokeRequestedAt == nil {
                                        Button("Revoke", role: .destructive) { revoke(invite) }
                                    }
                                }
                        }
                    }
                }
                if !exchanges.isEmpty {
                    Section("Recent questions") {
                        ForEach(exchanges) { exchange in
                            NavigationLink(value: BotPage.exchange(exchange.id)) {
                                ExchangeRow(
                                    exchange: exchange, peer: peers[exchange.connectionId],
                                    botName: bot?.name ?? "This workspace")
                            }
                            .groupedRowStyle()
                        }
                    }
                }
                if let error {
                    Section {
                        Text(error).font(Theme.sans(13, relativeTo: .footnote)).foregroundStyle(Theme.danger)
                    }
                    .listRowBackground(Color.clear)
                }
            }
        }
        .connectionPageStyle(title: "Connections")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                NavigationLink(value: BotPage.connect) {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("Connect a workspace")
                .accessibilityIdentifier("connections-connect")
            }
        }
    }

    private func emptyState(_ bot: Bot?) -> some View {
        ContentUnavailableView {
            Label("No connections yet", systemImage: "globe")
        } description: {
            Text(
                "Connect a workspace: invite someone with a code, or use theirs. Connected workspaces can ask \(bot?.name ?? "this workspace") questions, read-only, within what you allow."
            )
        } actions: {
            NavigationLink("Connect a workspace", value: BotPage.connect)
                .buttonStyle(.glass)
                .accessibilityIdentifier("connections-empty-connect")
        }
        .listRowBackground(Color.clear)
    }

    private func revoke(_ invite: BotInvite) {
        Task {
            do {
                error = nil
                try await model.workspaceData().revokeInvite(inviteId: invite.id)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

/// A connected workspace: its owner's photo, "Ledger · Dana Whitfield",
/// and its reply mode and last question.
struct ConnectionRow: View {
    let connection: BotConnection

    var body: some View {
        HStack(spacing: 12) {
            PeerAvatar(peer: connection.peer, size: 32)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(connection.peer.name)
                        .foregroundStyle(Theme.text)
                        .layoutPriority(1)
                    if !connection.peer.ownerName.isEmpty {
                        Text("· \(connection.peer.ownerName)")
                            .foregroundStyle(Theme.textMuted)
                    }
                }
                .font(Theme.sans(16, relativeTo: .body))
                .lineLimit(1)
                Text(connection.subLine())
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
        .opacity(connection.revokeRequestedAt != nil ? 0.6 : 1)
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

/// An open invite: when it was made, when it expires and its uses, and
/// Revoke.
private struct InviteRow: View {
    let invite: BotInvite
    let revoke: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "link")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 32)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(invite.title())
                    .font(Theme.sans(16, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                Text(invite.detail())
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
            }
            .lineLimit(1)
            Spacer(minLength: 8)
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

/// One question between workspaces: the question, then who asked whom,
/// its state (amber while it waits on you) and when.
struct ExchangeRow: View {
    let exchange: BotExchange
    let peer: PeerBot?
    let botName: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Group {
                if let peer {
                    PeerAvatar(peer: peer, size: 22)
                } else {
                    Image(systemName: exchange.direction == .incoming ? "arrow.down" : "arrow.up")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                }
            }
            .frame(width: 32)
            .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 5 }
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(exchange.question.replacingOccurrences(of: "\n", with: " "))
                    .font(Theme.sans(15, relativeTo: .body))
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
                Text(
                    "\(exchange.who(peer: peer, bot: botName)) · \(Text(exchange.stateWords).foregroundStyle(stateColor)) · \(BotTime.ago(exchange.askedAt))"
                )
                    .font(Theme.sans(13, relativeTo: .footnote))
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    private var stateColor: Color {
        switch exchange.stateTone {
        case .waiting: Theme.warning
        case .muted: Theme.textFaint
        default: Theme.textMuted
        }
    }
}

// MARK: - Connection

struct ConnectionPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let connectionId: String
    @State private var confirmRevoke = false
    @State private var error: String?

    var body: some View {
        let connection = model.data?.connection(id: connectionId)
        let bot = model.bot(id: connection?.botId ?? botId)
        List {
            if let connection, let bot {
                content(connection, bot: bot)
            } else {
                ContentUnavailableView(
                    "No Connection", systemImage: "globe",
                    description: Text("It may have been removed on another device."))
                    .listRowBackground(Color.clear)
            }
        }
        .connectionPageStyle(title: connection?.peer.name ?? "Connection")
        .confirmationDialog("Revoke connection?", isPresented: $confirmRevoke, titleVisibility: .visible) {
            Button("Revoke", role: .destructive) { revoke() }
        } message: {
            Text(
                "\(connection?.peer.name ?? "It") can no longer ask this workspace anything, and this workspace can no longer ask it. Connecting again takes a new invite."
            )
        }
    }

    @ViewBuilder
    private func content(_ connection: BotConnection, bot: Bot) -> some View {
        let editable = connection.isLive && connection.revokeRequestedAt == nil
        Section {
            BotPageHeader(title: connection.peer.name) {
                PeerAvatar(peer: connection.peer, size: 36)
            }
            .groupedRowStyle()
            if !connection.peer.ownerName.isEmpty {
                BotFact(label: "Owner", value: connection.peer.ownerName).groupedRowStyle()
            }
            BotFact(label: "Connected", value: BotTime.past(connection.createdAt)).groupedRowStyle()
            if !connection.isLive {
                BotFact(label: "Status") { BotStatusText("Revoked", tone: .muted) }.groupedRowStyle()
            } else if connection.revokeRequestedAt != nil {
                BotFact(label: "Status") { BotStatusText("Revoking on both sides…", tone: .active) }
                    .groupedRowStyle()
            }
        }
        replies(connection, editable: editable)
        reach(connection, bot: bot, editable: editable)
        questions(connection, bot: bot)
        if let error {
            Section {
                Text(error).font(Theme.sans(13, relativeTo: .footnote)).foregroundStyle(Theme.danger)
            }
            .listRowBackground(Color.clear)
        }
        if editable {
            Section {
                Button("Revoke Connection…", role: .destructive) { confirmRevoke = true }
                    .foregroundStyle(Theme.danger)
                    .groupedRowStyle()
                    .accessibilityIdentifier("connection-revoke")
            }
        }
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
                    Text("Auto").tag(ReplyMode.auto)
                    Text("Review").tag(ReplyMode.review)
                    Text("Off").tag(ReplyMode.off)
                }
                .pickerStyle(.segmented)
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
                .accessibilityIdentifier("connection-reply-mode")
            } else {
                BotFact(label: "Replies", value: connection.replyMode.label).groupedRowStyle()
            }
        } header: {
            Text("Replies")
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
                            write(ConnectionReach(sameProjects: same, spaceIds: granted, transcripts: reach.transcripts))
                        })
                ) {
                    Text("Same as \(bot.name)").tag(true)
                    Text("Only some").tag(false)
                }
                .disabled(!editable)
                .groupedRowStyle()
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
                        .groupedRowStyle()
                        .accessibilityAddTraits(on ? .isSelected : [])
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
            .groupedRowStyle()
            .accessibilityIdentifier("connection-transcripts")
        } header: {
            Text("May read")
        } footer: {
            Text(reachFooter(reach, bot: bot, spaces: spaces))
        }
    }

    /// What its questions may read, said plainly; a peer only ever reads.
    private func reachFooter(_ reach: ConnectionReach, bot: Bot, spaces: [Space]) -> String {
        let names = spaces.filter { reach.spaceIds.contains($0.id) }.map(\.displayName)
        let projects: String
        if spaces.isEmpty {
            projects = "\(bot.name) reads no projects yet."
        } else if names.isEmpty {
            projects = "No projects."
        } else if reach.sameProjects {
            projects = "Same as \(bot.name): \(ListFormatter.localizedString(byJoining: names))."
        } else {
            projects = "Only \(ListFormatter.localizedString(byJoining: names))."
        }
        let text =
            !bot.scope.transcripts
            ? " \(bot.name) itself reads only titles and status."
            : reach.transcripts ? " Message text included." : " Titles and status only."
        return projects + text + " Read-only."
    }

    @ViewBuilder
    private func questions(_ connection: BotConnection, bot: Bot) -> some View {
        let exchanges = Array(
            (model.data?.exchanges(of: connection.botId, connectionId: connection.id) ?? []).prefix(
                recentExchangesShown))
        Section("Recent questions") {
            if exchanges.isEmpty {
                Text("No questions yet.").foregroundStyle(Theme.textMuted).groupedRowStyle()
            }
            ForEach(exchanges) { exchange in
                NavigationLink(value: BotPage.exchange(exchange.id)) {
                    ExchangeRow(exchange: exchange, peer: connection.peer, botName: bot.name)
                }
                .groupedRowStyle()
            }
        }
    }

    private func save(_ connection: BotConnection, replyMode: ReplyMode, scope: BotScope?) {
        Task {
            do {
                error = nil
                try await model.workspaceData().updateConnection(
                    connectionId: connection.id, replyMode: replyMode, scope: scope)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func revoke() {
        Task {
            do {
                error = nil
                try await model.workspaceData().revokeConnection(connectionId: connectionId)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

// MARK: - Question

struct ExchangePage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let exchangeId: String
    /// The answer as the owner edits it; seeded with the draft.
    @State private var draft: String?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        let exchange = model.data?.exchange(id: exchangeId)
        let awaiting = exchange?.awaitingReview == true
        List {
            if let exchange {
                content(exchange, awaiting: awaiting)
            } else {
                ContentUnavailableView(
                    "No Question", systemImage: "bubble.left.and.text.bubble.right",
                    description: Text("It may have been removed on another device."))
                    .listRowBackground(Color.clear)
            }
        }
        .connectionPageStyle(title: awaiting ? "Review answer" : "Question")
        .safeAreaInset(edge: .bottom) {
            if let exchange, awaiting {
                BotDecisionBar(
                    note: error ?? "Edit it before it goes, or decline.",
                    decisions: [
                        BotDecision(title: "Decline", role: .destructive) { review(exchange, send: false) },
                        BotDecision(
                            title: "Send", role: .primary,
                            enabled: !(draft ?? exchange.answer ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty
                        ) { review(exchange, send: true) },
                    ],
                    busy: busy)
            }
        }
        .onAppear {
            if draft == nil { draft = exchange?.answer ?? "" }
        }
    }

    @ViewBuilder
    private func content(_ exchange: BotExchange, awaiting: Bool) -> some View {
        let connection = model.data?.connection(id: exchange.connectionId)
        let botName = model.bot(id: exchange.botId)?.name ?? "This workspace"
        Section {
            BotPageHeader(
                title: awaiting ? "Review answer" : "Question",
                symbol: awaiting ? "person.2" : "bubble.left.and.text.bubble.right",
                tone: awaiting ? .waiting : .muted
            )
            .groupedRowStyle()
            BotFact(label: exchange.direction == .incoming ? "From" : "To") {
                HStack(spacing: 6) {
                    if let peer = connection?.peer { PeerAvatar(peer: peer, size: 20) }
                    Text(
                        "\(connection?.peerLabel ?? "A connected workspace") · \(BotTime.ago(exchange.askedAt))")
                }
            }
            .groupedRowStyle()
            if !awaiting {
                BotFact(status: exchange.status).groupedRowStyle()
            }
            BotFact(label: "Asked", value: BotTime.past(exchange.askedAt)).groupedRowStyle()
            if let answeredAt = exchange.answeredAt {
                BotFact(label: exchange.direction == .incoming ? "Ended" : "Answered", value: BotTime.past(answeredAt))
                    .groupedRowStyle()
            }
        } footer: {
            Text(exchange.who(peer: connection?.peer, bot: botName))
        }
        Section("Question") {
            Text(exchange.question)
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .groupedRowStyle()
        }
        if awaiting {
            Section("Answer") {
                TextEditor(
                    text: Binding(
                        get: { draft ?? exchange.answer ?? "" },
                        set: { draft = $0 })
                )
                .font(Theme.sans(15, relativeTo: .body))
                .foregroundStyle(Theme.text)
                .scrollContentBackground(.hidden)
                .frame(minHeight: 180)
                .groupedRowStyle()
                .accessibilityIdentifier("exchange-answer")
            }
        } else if let answer = (exchange.review?.text ?? exchange.answer), !answer.trimmingCharacters(
            in: .whitespacesAndNewlines
        ).isEmpty {
            Section("Answer") {
                MemoryMarkdown(text: answer, cacheKey: "exchange-\(exchange.id)")
                    .padding(.vertical, 4)
                    .groupedRowStyle()
            }
        }
    }

    /// Send (with the text only when it was edited) or decline.
    private func review(_ exchange: BotExchange, send: Bool) {
        let original = (exchange.answer ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let text = (draft ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let edited = send && text != original ? text : nil
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                try await model.workspaceData().reviewExchange(exchangeId: exchange.id, send: send, text: edited)
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

// MARK: - Connect

struct ConnectPage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    @State private var issued: InviteCreated?
    @State private var creating = false
    @State private var inviteError: String?
    @State private var copied = false
    @State private var code = ""
    @State private var redeeming = false
    @State private var redeemError: String?
    /// The connection a code just made: opened on top of this page.
    @State private var opened: BotPage?

    var body: some View {
        let bot = model.bot(id: botId)
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
                    .disabled(creating || bot == nil)
                    .groupedRowStyle()
                    .accessibilityIdentifier("connect-create-invite")
                }
                if let inviteError {
                    Text(inviteError)
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.danger)
                        .groupedRowStyle()
                }
            } header: {
                Text("Invite a workspace")
            } footer: {
                Text(
                    issued.map { issuedInviteNote($0) }
                        ?? "Send the code to the other person; they connect one of their workspaces with it.")
            }
            Section {
                TextField("Paste a code", text: $code)
                    .font(Theme.mono(14))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit(redeem)
                    .groupedRowStyle()
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
                .disabled(redeeming || code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .groupedRowStyle()
                .accessibilityIdentifier("connect-redeem")
                if let redeemError {
                    Text(redeemError)
                        .font(Theme.sans(13, relativeTo: .footnote))
                        .foregroundStyle(Theme.danger)
                        .groupedRowStyle()
                }
            } header: {
                Text("Connect with a code")
            } footer: {
                Text("Got a code from someone? Connect \(bot?.name ?? "this workspace") to their workspace.")
            }
            Section {
            } footer: {
                Text(
                    "Questions and answers pass through Cypher's servers as plain text until delivered. Answers are scanned for secrets before they leave; that is a safety net, not a guarantee."
                )
            }
        }
        .connectionPageStyle(title: "Connect a workspace")
        .navigationDestination(item: $opened) { BotPageView(botId: botId, page: $0) }
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
        .groupedRowStyle()
    }

    private func createInvite() {
        creating = true
        inviteError = nil
        Task {
            defer { creating = false }
            do {
                issued = try await model.workspaceData().createInvite(botId: botId)
            } catch {
                inviteError = error.localizedDescription
            }
        }
    }

    private func redeem() {
        let typed = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !redeeming else { return }
        guard !typed.isEmpty else {
            redeemError = "Paste the code you were sent."
            return
        }
        guard splitInviteCode(typed) != nil else {
            redeemError = "That isn't an invite code. It looks like two parts joined by a dot."
            return
        }
        redeeming = true
        redeemError = nil
        Task {
            defer { redeeming = false }
            do {
                let connection = try await model.workspaceData().redeemInvite(botId: botId, code: typed)
                code = ""
                opened = .connection(connection.id)
            } catch {
                redeemError = error.localizedDescription
            }
        }
    }
}

// MARK: - Layout

extension View {
    /// A Connections page: inset-grouped on the surface, inline title.
    fileprivate func connectionPageStyle(title: String) -> some View {
        listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Theme.surface.ignoresSafeArea())
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
    }
}

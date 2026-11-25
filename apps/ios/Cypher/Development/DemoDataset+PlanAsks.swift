// `-demo-plan-asks`: one of Atlas's plans with a question open in every way
// docs/design/plan-asks.md §2 lists, for the Needs you screenshots: a
// permission prompt and an `ask_user` question in live step sessions, a
// subagent's question in a child chat under a step session whose own run
// ended, a permission prompt left open across an engine restart (the row
// went idle and stopped refreshing) and one on a Mac mini that went offline
// ("Waiting for its device"). Atlas's other items are settled so the page
// shows only these. Answering one plays the host: the question resolves,
// the session works again and its step is back to Running.

import Foundation

extension DemoDataset {
    static let planAsksWorkId = "work-plan-asks"
    static let planAsksDevice = "dev-mini"

    /// The chats the fixture adds: (step, session, the chat that asks).
    static let planAskChats: [(step: String, session: String, asker: String)] = [
        ("1", "chat-pa-perm", "chat-pa-perm"),
        ("2", "chat-pa-ask", "chat-pa-ask"),
        ("3", "chat-pa-sub", "chat-pa-sub-k1"),
        ("4", "chat-pa-restart", "chat-pa-restart"),
        ("5", "chat-pa-offline", "chat-pa-offline"),
    ]

    func addPlanAsks() {
        let now = nowMs()
        let minute: Int64 = 60_000
        let hour: Int64 = 3_600_000
        // Live rows a year ahead read as fresh, as in the board fixtures.
        let fresh = now + 365 * 86_400_000
        // Settle Atlas's other items: only this plan's questions remain.
        botRecords.approvals = botRecords.approvals.filter { $0.botId != Self.atlasId }
        botRecords.exchanges = botRecords.exchanges.filter { $0.botId != Self.atlasId }
        botRecords.work.removeAll { $0.botId == Self.atlasId && $0.awaitingApproval }
        for (ix, work) in botRecords.work.enumerated() where work.botId == Self.atlasId {
            for step in work.steps.indices where botRecords.work[ix].steps[step].state == .asking {
                botRecords.work[ix].steps[step].state = .running
            }
            for id in work.sessionIds where sessions[id]?.status == .awaitingInput {
                sessions[id]?.status = .working
            }
        }

        devices.append(
            DeviceRow(
                id: Self.planAsksDevice, name: "Mac mini", platform: "macos",
                lastSeenAt: now - 2 * hour, createdAt: now - 30 * 86_400_000))
        let config = ChatConfig(harness: "pi", model: "demo/pi", reasoning: "high", sandbox: "workspace-write")
        func session(_ id: String, _ title: String, device: String = "dev-mac", at: Int64, preview: String) -> Chat {
            var chat = Chat(
                id: id, deviceId: device, title: title, archived: false, cwd: "/Users/dev/cypher",
                branch: "atlas/sync-fix", checkoutId: nil, config: config, lastMessagePreview: preview,
                lastMessageAt: at, createdAt: now - hour, spaceId: "space-cypher", lastSeenAt: at)
            chat.startedBy = ChatStartedBy(botId: Self.atlasId, workId: Self.planAsksWorkId)
            return chat
        }
        var child = Chat(
            id: "chat-pa-sub-k1", deviceId: "dev-mac", title: "reviewer", archived: false, cwd: "/Users/dev/cypher",
            branch: "atlas/sync-fix", checkoutId: nil, config: config, lastMessagePreview: nil,
            lastMessageAt: now - 4 * minute, createdAt: now - 6 * minute, spaceId: "space-cypher",
            lastSeenAt: now - 4 * minute)
        child.child = ChildChat(
            parentChatId: "chat-pa-sub", parentRunId: "run-pa-sub", agent: "reviewer",
            task: "Review the reconnect patch", mode: .async, toolCallId: nil)
        chats += [
            session(
                "chat-pa-perm", "Run the registry migration", at: now - 3 * minute,
                preview: "The migration needs the production database."),
            session(
                "chat-pa-ask", "Pick the staging database", at: now - 2 * minute,
                preview: "Which staging database should the e2e suite use?"),
            session(
                "chat-pa-sub", "Review the reconnect patch", at: now - 40 * minute,
                preview: "Started a reviewer for the patch."),
            child,
            session(
                "chat-pa-restart", "Rebuild the sync index", at: now - 50 * minute,
                preview: "The index lives outside the project."),
            session(
                "chat-pa-offline", "Build the release on the Mac mini", device: Self.planAsksDevice,
                at: now - 2 * hour - 5 * minute, preview: "Signing needs the keychain."),
        ]

        // (a) and (b): live runs, fresh rows.
        sessions["chat-pa-perm"] = SessionRow(
            chatId: "chat-pa-perm", deviceId: "dev-mac", status: .awaitingInput, startedAt: now - 8 * minute,
            updatedAt: fresh,
            asking: SessionAsk(requestId: "pa-perm", askedAt: now - 3 * minute, permission: "escalate"))
        sessions["chat-pa-ask"] = SessionRow(
            chatId: "chat-pa-ask", deviceId: "dev-mac", status: .awaitingInput, startedAt: now - 6 * minute,
            updatedAt: fresh, asking: SessionAsk(requestId: "pa-ask", askedAt: now - 2 * minute))
        // (d′, child part): the step session's run ended 40 minutes ago (its
        // row is stale), while its subagent's child chat asks.
        sessions["chat-pa-sub"] = SessionRow(
            chatId: "chat-pa-sub", deviceId: "dev-mac", status: .idle, startedAt: now - 50 * minute,
            updatedAt: now - 40 * minute)
        sessions["chat-pa-sub-k1"] = SessionRow(
            chatId: "chat-pa-sub-k1", deviceId: "dev-mac", status: .awaitingInput, startedAt: now - 6 * minute,
            updatedAt: fresh, asking: SessionAsk(requestId: "pa-sub", askedAt: now - 4 * minute))
        // (e1): open across an engine restart: recovery set the row idle and
        // nothing refreshes it, but the question is still answerable.
        sessions["chat-pa-restart"] = SessionRow(
            chatId: "chat-pa-restart", deviceId: "dev-mac", status: .idle, startedAt: now - hour,
            updatedAt: now - 48 * minute,
            asking: SessionAsk(requestId: "pa-restart", askedAt: now - 50 * minute, permission: "access"))
        // (f): its device went offline with the question open.
        sessions["chat-pa-offline"] = SessionRow(
            chatId: "chat-pa-offline", deviceId: Self.planAsksDevice, status: .awaitingInput,
            startedAt: now - 2 * hour - 20 * minute, updatedAt: now - 2 * hour,
            asking: SessionAsk(requestId: "pa-offline", askedAt: now - 2 * hour - 5 * minute, permission: "escalate"))

        func step(_ id: String, _ title: String, session: String, note: String, started: Int64) -> WorkStep {
            WorkStep(
                id: id, title: title, project: "space-cypher", state: .asking, sessions: [session], note: note,
                startedAt: started)
        }
        botRecords.work.append(
            BotWork(
                id: Self.planAsksWorkId, botId: Self.atlasId, title: "Ship the sync fix",
                instruction: "Migrate the registry, prove the fix on staging, review it and build the release.",
                summary: "Five steps in parallel; each one is waiting on you.",
                steps: [
                    step(
                        "1", "Run the registry migration", session: "chat-pa-perm", note: "Permission prompt",
                        started: now - 8 * minute),
                    step(
                        "2", "Pick the staging database", session: "chat-pa-ask", note: "Asking a question",
                        started: now - 6 * minute),
                    step(
                        "3", "Review the reconnect patch", session: "chat-pa-sub", note: "Its reviewer asks",
                        started: now - 50 * minute),
                    step(
                        "4", "Rebuild the sync index", session: "chat-pa-restart", note: "Open across a restart",
                        started: now - hour),
                    step(
                        "5", "Build the release on the Mac mini", session: "chat-pa-offline",
                        note: "Waiting for its device", started: now - 2 * hour - 20 * minute),
                ],
                state: .running, chatId: "chat-atlas", toolCallId: "call-plan-asks",
                threadChatId: "chat-atlas-thread-flaky", createdAt: now - 2 * hour - 30 * minute,
                startedAt: now - 2 * hour - 25 * minute,
                decision: PlanDecision(approve: true, at: now - 2 * hour - 26 * minute)))
    }

    /// Plays the host once a question of the fixture is answered (`requestId`
    /// empty: answered on another device): the question resolves, its chat
    /// works again, its step is back to Running.
    func answerPlanAsk(chatId: String, requestId: String) {
        guard let ask = Self.planAskChats.first(where: { $0.asker == chatId }) else { return }
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 700_000_000)
            guard let self else { return }
            let store = self.sessionStore(for: chatId)
            store.setEntries(
                store.fedEntries.map { entry in
                    var entry = entry
                    entry.parts = entry.parts.map { part in
                        // The one answered (any, for an answer from elsewhere).
                        if case .input(let id, let rid, let questions, false) = part,
                            requestId.isEmpty || rid == requestId
                        {
                            return .input(id: id, requestId: rid, questions: questions, resolved: true)
                        }
                        return part
                    }
                    return entry
                })
            let fresh = nowMs() + 365 * 86_400_000
            for id in Set([chatId, ask.session]) {
                guard var row = self.sessions[id] else { continue }
                row.asking = nil
                row.status = .working
                row.updatedAt = fresh
                self.sessions[id] = row
            }
            if let work = self.botRecords.work.firstIndex(where: { $0.id == Self.planAsksWorkId }),
                let step = self.botRecords.work[work].steps.firstIndex(where: { $0.id == ask.step })
            {
                self.botRecords.work[work].steps[step].state = .running
                self.botRecords.work[work].steps[step].note = nil
            }
        }
    }

    static func planAsksTranscript(for chatId: String, now: Int64) -> [MessageEntry]? {
        let minute: Int64 = 60_000
        func user(_ id: String, _ text: String, at: Int64) -> MessageEntry {
            MessageEntry(
                id: id, role: .user, parts: [.text(id: "\(id)-t", text: text)], createdAt: at,
                deviceId: "dev-mac", status: .complete)
        }
        func reply(_ id: String, _ parts: [MessagePart], at: Int64) -> MessageEntry {
            MessageEntry(id: id, role: .assistant, parts: parts, createdAt: at, deviceId: "dev-mac", status: .complete)
        }
        func ask(_ id: String, _ header: String, _ question: String, _ options: [String]) -> MessagePart {
            .input(
                id: "in-\(id)", requestId: id,
                questions: [
                    UserInputQuestion(
                        id: "q-\(id)", header: header, question: question, options: options, multiSelect: false)
                ], resolved: false)
        }
        switch chatId {
        case "chat-pa-perm":
            let command = "psql \"$PROD_URL\" -f migrations/0042_registry_cursor.sql"
            return [
                user("pa-perm-u", "Run the registry cursor migration against production.", at: now - 8 * minute),
                reply(
                    "pa-perm-a",
                    [
                        .text(id: "t1", text: "The migration is ready. It needs the production database:"),
                        prompt(
                            "pa-perm",
                            escalateTitle(
                                command, purpose: "Migrate the registry cursor table", pattern: "psql …",
                                reason: "The sandbox has no network access to the database."),
                            options: escalateOptions, kind: "escalate", call: nil, resolved: false),
                    ], at: now - 3 * minute),
            ]
        case "chat-pa-ask":
            return [
                user("pa-ask-u", "Run the webapp e2e suite against staging.", at: now - 6 * minute),
                reply(
                    "pa-ask-a",
                    [
                        .text(id: "t1", text: "Staging's database was reset this morning. Before I run the suite:"),
                        ask(
                            "pa-ask", "Staging database", "Which staging database should the e2e suite use?",
                            ["The fresh staging database", "A copy of yesterday's snapshot"]),
                    ], at: now - 2 * minute),
            ]
        case "chat-pa-sub":
            return [
                user("pa-sub-u", "Review the reconnect patch before it merges.", at: now - 50 * minute),
                reply(
                    "pa-sub-a", [.text(id: "t1", text: "Started a reviewer for the patch; it reports back here.")],
                    at: now - 40 * minute),
            ]
        case "chat-pa-sub-k1":
            return [
                user("pa-k1-u", "Review the reconnect patch in RegistryClient.swift.", at: now - 6 * minute),
                reply(
                    "pa-k1-a",
                    [
                        .text(id: "t1", text: "The patch holds the cursor until the ack lands. One thing first:"),
                        ask(
                            "pa-sub", "Retry", "Should a dropped ack retry the batch, or reconnect?",
                            ["Retry the batch", "Reconnect"]),
                    ], at: now - 4 * minute),
            ]
        case "chat-pa-restart":
            return [
                user("pa-restart-u", "Rebuild the sync index from the snapshots.", at: now - 60 * minute),
                reply(
                    "pa-restart-a",
                    [
                        .text(id: "t1", text: "The index is kept next to the app data, outside the project:"),
                        prompt(
                            "pa-restart",
                            "Allow the agent to write outside the project?\n\nContext:\n"
                                + "Path: ~/Library/Application Support/Cypher/sync-index\nTool: write",
                            options: accessOptions, kind: "access", call: nil, resolved: false),
                    ], at: now - 50 * minute),
            ]
        case "chat-pa-offline":
            let command = "xcodebuild -scheme Cypher -configuration Release archive"
            return [
                user("pa-off-u", "Archive the release build on the Mac mini.", at: now - 140 * minute),
                reply(
                    "pa-off-a",
                    [
                        .text(id: "t1", text: "Signing the archive needs the keychain:"),
                        prompt(
                            "pa-offline",
                            escalateTitle(
                                command, purpose: "Archive the signed release", pattern: "xcodebuild …",
                                reason: "Code signing reads the login keychain."),
                            options: escalateOptions, kind: "escalate", call: nil, resolved: false),
                    ], at: now - 125 * minute),
            ]
        default:
            return nil
        }
    }
}

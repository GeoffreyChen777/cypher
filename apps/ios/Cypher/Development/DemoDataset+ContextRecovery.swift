// Context recovery showcase: a demo project whose sessions show each state of
// the feature (docs/features/context-recovery.md) — a live replay of the whole
// flow, a recovery writing context, one restarting, one that succeeded, and
// one that ran out of attempts.

import Foundation

extension DemoDataset {
    static let recoverySpaceId = "space-recovery"
    /// Replays the flow when opened, and again for every message sent in it.
    static let recoveryReplayChatId = "chat-recovery-live"

    /// The showcase project and its sessions, for `standard()`.
    static func recoveryShowcase(now: Int64, config: ChatConfig) -> (Space, [Chat], [String: SessionRow]) {
        let space = Space(
            id: recoverySpaceId, deviceId: "dev-mac",
            path: "/Users/dev/preview-deploys", name: "Context recovery", gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000)
        func chat(_ id: String, _ title: String, _ preview: String, at: Int64) -> Chat {
            Chat(
                id: id, deviceId: "dev-mac", title: title,
                archived: false, cwd: space.path, branch: "main", checkoutId: nil,
                config: config, lastMessagePreview: preview,
                lastMessageAt: at, createdAt: at - 1_800_000,
                spaceId: space.id, lastSeenAt: at)
        }
        let chats = [
            chat(
                recoveryReplayChatId, "Live: restart with added context",
                "Built preview:pr-482.", at: now - 5_000),
            chat(
                "chat-recovery", "Writing context · attempt 2",
                "I can't access the staging database.", at: now - 12_000),
            chat(
                "chat-recovery-restarting", "Restarting after an error",
                "429 Too Many Requests", at: now - 20_000),
            chat(
                "chat-recovery-done", "Recovered from the beginning",
                "Added `archived_at` with a backfill.", at: now - 600_000),
            chat(
                "chat-recovery-limit", "Out of attempts",
                "Context window exceeded", at: now - 1_200_000),
        ]
        let sessions: [String: SessionRow] = [
            "chat-recovery": SessionRow(
                chatId: "chat-recovery", deviceId: "dev-mac", status: .idle,
                startedAt: nil, updatedAt: now - 8_000,
                recovery: RecoveryProgress(
                    stage: .writingContext, trigger: "I can't access", matchedIn: .response,
                    attempt: 2, startedAt: now - 8_000)),
            "chat-recovery-restarting": SessionRow(
                chatId: "chat-recovery-restarting", deviceId: "dev-mac", status: .errored,
                startedAt: nil, updatedAt: now - 3_000,
                recovery: RecoveryProgress(
                    stage: .restarting, trigger: "rate limit", matchedIn: .error,
                    attempt: 1, startedAt: now - 14_000)),
            "chat-recovery-limit": SessionRow(
                chatId: "chat-recovery-limit", deviceId: "dev-mac", status: .errored,
                startedAt: nil, updatedAt: now - 1_200_000),
        ]
        return (space, chats, sessions)
    }

    /// A showcase recovery whose progress went stale before the session was
    /// first opened starts over, so the strip still has something to show.
    func refreshRecoveryShowcase(_ chatId: String) {
        let now = nowMs()
        guard let recovery = sessions[chatId]?.recovery, !recovery.isLive(now: now) else { return }
        sessions[chatId]?.recovery?.startedAt = now - 8_000
        sessions[chatId]?.updatedAt = now
    }

    static func recoveryTranscript(for chatId: String, now: Int64) -> [MessageEntry]? {
        switch chatId {
        case recoveryReplayChatId:
            return [
                user("p1", "Build the preview image for PR #482.", at: now - 300_000),
                answer(
                    "p2",
                    [
                        .tool(
                            id: "x1", call: RenderToolCall(tag: "exec", fields: ["command": "make preview PR=482"]),
                            isError: false, resolved: true),
                        .text(id: "t0", text: "Built `preview:pr-482`. The image is 184 MB and passed the smoke test."),
                    ], at: now - 290_000),
            ]
        case "chat-recovery":
            return writingContextStory(now: now)
        case "chat-recovery-restarting":
            return [
                user("m1", "Summarize yesterday's failed CI runs.", at: now - 30_000),
                answer(
                    "m2",
                    [
                        .tool(
                            id: "x1",
                            call: RenderToolCall(
                                tag: "exec", fields: ["command": "gh run list --status failure --limit 50"]),
                            isError: true, resolved: true),
                        .error(
                            id: "e1",
                            message:
                                "429 Too Many Requests: API rate limit exceeded for this token. Retry after 60 seconds."
                        ),
                    ], at: now - 20_000),
            ]
        case "chat-recovery-done":
            let prompt = "Write a migration that adds `archived_at` to sessions."
            return [
                user(
                    "m1", prompt, at: now - 660_000,
                    recovery: ContextRecovery(
                        context: """
                            The previous attempt failed because the model endpoint was overloaded, not because \
                            of the request. Migrations live in `db/migrations`, numbered after \
                            `0043_recovery_field.sql`. Make the column nullable, backfill it from `updated_at` for \
                            archived rows, and add an index on `(archived_at)`.
                            """,
                        trigger: "overloaded", matchedIn: .error,
                        excerpt: "529 overloaded_error: the model is temporarily overloaded. Please try again.",
                        restartAfter: nil, restartPreview: nil, attempt: 1,
                        model: "anthropic/claude-haiku-4-5")),
                answer(
                    "m2",
                    [
                        .tool(
                            id: "x1",
                            call: RenderToolCall(
                                tag: "editFile", fields: ["path": "db/migrations/0044_sessions_archived_at.sql"]),
                            isError: false, resolved: true),
                        .tool(
                            id: "x2", call: RenderToolCall(tag: "exec", fields: ["command": "make migrate-check"]),
                            isError: false, resolved: true),
                        .text(
                            id: "t0",
                            text: """
                                Added `0044_sessions_archived_at.sql`:

                                - `archived_at TIMESTAMPTZ NULL` on `sessions`
                                - a backfill from `updated_at` for rows already archived
                                - an index on `archived_at`

                                `make migrate-check` passes against a fresh database and a copy of staging.
                                """),
                    ], at: now - 600_000),
            ]
        case "chat-recovery-limit":
            return [
                user("m1", "Find out why `sync_roundtrip` is flaky.", at: now - 3_600_000),
                answer(
                    "m2",
                    [
                        .text(
                            id: "t0", text: "It fails about 1 run in 30. I'll look at the timing around the reconnect.")
                    ],
                    at: now - 3_550_000),
                user(
                    "m3", "Read the last 200 CI logs and find the common cause.", at: now - 1_300_000,
                    recovery: ContextRecovery(
                        context: """
                            Two attempts ran out of context reading whole logs. Do not open the logs in full: \
                            grep each one for `sync_roundtrip` and `FAILED`, keep 20 lines around each match, \
                            and summarize in batches of 25 logs.
                            """,
                        trigger: "context window", matchedIn: .error,
                        excerpt: "…prompt is too long: context window exceeded (212,480 > 200,000 tokens)…",
                        restartAfter: "m2",
                        restartPreview: "It fails about 1 run in 30. I'll look at the…",
                        attempt: 3, model: "anthropic/claude-haiku-4-5")),
                answer(
                    "m4",
                    [
                        .tool(
                            id: "x1",
                            call: RenderToolCall(
                                tag: "exec", fields: ["command": "rg -C 20 'sync_roundtrip|FAILED' ci-logs/"]),
                            isError: false, resolved: true),
                        .error(
                            id: "e1",
                            message: "prompt is too long: context window exceeded (204,112 > 200,000 tokens)"),
                    ], at: now - 1_200_000),
            ]
        default:
            return nil
        }
    }

    private static func writingContextStory(now: Int64) -> [MessageEntry] {
        [
            user("m1", "Which migrations are pending on staging?", at: now - 1_700_000),
            answer(
                "m2",
                [
                    .text(
                        id: "t0",
                        text: "Three: `0042_add_turn_changes`, `0043_recovery_field` and `0044_backfill_models`.")
                ], at: now - 1_650_000),
            user(
                "m3", "Run them and check the row counts afterwards.", at: now - 60_000,
                recovery: ContextRecovery(
                    context: """
                        The staging database is reachable from this machine through the SSH tunnel the \
                        user keeps open: connect to 127.0.0.1:6543 with the credentials in `.env.staging`. \
                        Run the pending migrations with `make migrate ENV=staging`, then compare \
                        `SELECT count(*)` on `messages` and `sessions` before and after.
                        """,
                    trigger: "I can't access", matchedIn: .response,
                    excerpt:
                        "…Sorry, I can't access the staging database from this sandbox, so I can't run the migrations…",
                    restartAfter: "m2",
                    restartPreview: "Three: 0042_add_turn_changes, 0043_recovery_field and…",
                    attempt: 1, model: "anthropic/claude-haiku-4-5")),
            answer(
                "m4",
                [
                    .text(
                        id: "t0",
                        text:
                            "I can't access the staging database: the tunnel on 127.0.0.1:6543 refused the connection.")
                ], at: now - 12_000),
        ]
    }

    private static func user(
        _ id: String, _ text: String, at: Int64, recovery: ContextRecovery? = nil
    ) -> MessageEntry {
        MessageEntry(
            id: id, role: .user, parts: [.text(id: "t0", text: text)],
            createdAt: at, deviceId: "ios-demo", status: .complete, recovery: recovery)
    }

    private static func answer(
        _ id: String, _ parts: [MessagePart], at: Int64, status: MessageStatus = .complete
    ) -> MessageEntry {
        MessageEntry(id: id, role: .assistant, parts: parts, createdAt: at, deviceId: "dev-mac", status: status)
    }

    // MARK: Live replay

    /// The whole flow, paced to watch: the prompt draws a reply that matches
    /// a trigger; the strip says the host is writing context, then
    /// restarting; the prompt and the failed reply leave the transcript, and
    /// the prompt comes back captioned and gets a real answer.
    func playRecoveryStory(store: SessionStore, chatId: String, prompt: String) {
        recoveryTask?.cancel()
        recoveryTask = Task { [weak self, weak store] in
            guard let self, let store else { return }
            let start = nowMs()
            let before = store.fedEntries
            let failedId = "a-\(start)"
            self.setRecoverySession(chatId, status: .working, startedAt: start)
            store.setEntries(
                before + [
                    Self.user("u-\(start)", prompt, at: start),
                    Self.answer(failedId, [], at: start, status: .streaming),
                ])
            let failure =
                "Sorry, I can't access the staging cluster from this sandbox, so I can't deploy the preview build."
            guard await Self.stream(failure, into: failedId, of: store, after: []) else { return }

            // A trigger matched: the host writes context, then restarts.
            let began = nowMs()
            var progress = RecoveryProgress(
                stage: .writingContext, trigger: "I can't access", matchedIn: .response,
                attempt: 1, startedAt: began)
            self.setRecoverySession(chatId, status: .idle, recovery: progress)
            guard await Self.pause(4_500) else { return }
            progress.stage = .restarting
            self.setRecoverySession(chatId, status: .idle, recovery: progress)
            guard await Self.pause(1_600) else { return }

            let resent = nowMs()
            let previous = before.last { $0.role == .assistant }
            let record = ContextRecovery(
                context: """
                    You can reach the staging cluster: the kubeconfig for it is at \
                    `~/.kube/staging.yaml` and `kubectl` picks it up with `--context staging`. Deploy with \
                    `make deploy-preview PR=482 ENV=staging` and report the preview URL.
                    """,
                trigger: progress.trigger, matchedIn: .response,
                excerpt: "…" + failure,
                restartAfter: previous?.id,
                restartPreview: previous.flatMap(Self.openingWords),
                attempt: 1, model: "anthropic/claude-haiku-4-5")
            let answerId = "a-\(resent)"
            self.setRecoverySession(chatId, status: .working, startedAt: resent)
            store.setEntries(
                before + [
                    Self.user("u-\(resent)", prompt, at: resent, recovery: record),
                    Self.answer(answerId, [], at: resent, status: .streaming),
                ])
            let deploy = RenderToolCall(
                tag: "exec", fields: ["command": "make deploy-preview PR=482 ENV=staging"])
            guard await Self.pause(500) else { return }
            Self.update(answerId, of: store) {
                $0.parts = [.tool(id: "x1", call: deploy, isError: false, resolved: false)]
            }
            guard await Self.pause(1_800) else { return }
            let tools: [MessagePart] = [.tool(id: "x1", call: deploy, isError: false, resolved: true)]
            let success = """
                Deployed `preview:pr-482` to staging with the cluster's kubeconfig. \
                The preview is live at **https://pr-482.preview.staging.example.dev** and its health check passes.
                """
            guard await Self.stream(success, into: answerId, of: store, after: tools) else { return }
            self.setRecoverySession(chatId, status: .idle, preview: "Deployed preview:pr-482 to staging.")
        }
    }

    private func setRecoverySession(
        _ chatId: String, status: SessionStatus, startedAt: Int64? = nil,
        recovery: RecoveryProgress? = nil, preview: String? = nil
    ) {
        let now = nowMs()
        sessions[chatId] = SessionRow(
            chatId: chatId, deviceId: "dev-mac", status: status,
            startedAt: startedAt, updatedAt: now, recovery: recovery)
        if let ix = chats.firstIndex(where: { $0.id == chatId }) {
            chats[ix].lastMessageAt = now
            chats[ix].lastSeenAt = now
            if let preview { chats[ix].lastMessagePreview = preview }
        }
    }

    /// Streams `text` into the entry word by word, then settles it. False
    /// when the replay was cancelled or the entry is gone.
    private static func stream(
        _ text: String, into entryId: String, of store: SessionStore, after parts: [MessagePart]
    ) async -> Bool {
        let words = text.split(separator: " ", omittingEmptySubsequences: false)
        var shown = ""
        for (ix, word) in words.enumerated() {
            guard await pause(UInt64.random(in: 40...110)) else { return false }
            shown += (ix == 0 ? "" : " ") + word
            guard update(entryId, of: store, { $0.parts = parts + [.text(id: "t0", text: shown)] }) else {
                return false
            }
        }
        return update(entryId, of: store) { $0.status = .complete }
    }

    @discardableResult
    private static func update(_ entryId: String, of store: SessionStore, _ change: (inout MessageEntry) -> Void)
        -> Bool
    {
        var entries = store.fedEntries
        guard let ix = entries.firstIndex(where: { $0.id == entryId }) else { return false }
        change(&entries[ix])
        store.setEntries(entries)
        return true
    }

    private static func pause(_ ms: UInt64) async -> Bool {
        try? await Task.sleep(nanoseconds: ms * 1_000_000)
        return !Task.isCancelled
    }

    /// The restart point's opening words, as the host records them.
    private static func openingWords(_ entry: MessageEntry) -> String? {
        let text = entry.parts.compactMap { part -> String? in
            if case .text(_, let text, _) = part { return text }
            return nil
        }.joined(separator: " ")
        let words = text.replacingOccurrences(of: "`", with: "").split(whereSeparator: \.isWhitespace)
        guard !words.isEmpty else { return nil }
        let opening = words.prefix(8).joined(separator: " ")
        return words.count > 8 ? opening + "…" : opening
    }
}

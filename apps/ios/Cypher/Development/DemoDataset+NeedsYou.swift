// A fuller Needs you inbox (`-demo-needs`): beside the standard demo's
// approval, question with options, plan and answer to review, Atlas gains a
// session of its own work stopped on a permission prompt (a command to run
// outside the sandbox, the extension's five options) and an approval with a
// long action. `-demo-needs-many` adds six more approvals, so the page
// scrolls. Open with `-route bot:bot-atlas -sheet board:needs`; the
// permission's own page is `board:asking:work-deps:chat-atlas-w6`.

import Foundation

extension DemoDataset {
    static let needsPermissionWorkId = "work-deps"
    static let needsPermissionChatId = "chat-atlas-w6"

    /// The permission prompt and the long approval.
    func addNeedsYouStates(many: Bool = false) {
        let now = nowMs()
        let minute: Int64 = 60_000
        let chatId = Self.needsPermissionChatId
        chats.append(
            Chat(
                id: chatId, deviceId: "dev-mac", title: "Update the webapp's lockfile", archived: false,
                cwd: "/Users/dev/webapp", branch: "atlas/deps", checkoutId: nil,
                config: ChatConfig(harness: "pi", model: "demo/pi", reasoning: "high", sandbox: "workspace-write"),
                lastMessagePreview: "Run this command outside the sandbox?", lastMessageAt: now - minute,
                createdAt: now - 6 * minute, spaceId: "space-webapp", lastSeenAt: now - 2 * minute,
                startedBy: ChatStartedBy(botId: Self.atlasId, workId: Self.needsPermissionWorkId)))
        sessions[chatId] = SessionRow(
            chatId: chatId, deviceId: "dev-mac", status: .awaitingInput, startedAt: now - 6 * minute,
            updatedAt: now + 365 * 86_400_000)
        botRecords.work.append(
            BotWork(
                id: Self.needsPermissionWorkId, botId: Self.atlasId, title: "Update the webapp's dependencies",
                instruction: "Bring the webapp's dependencies up to date and make sure the build still passes.",
                summary: "Refresh the lockfile, then run the build and the unit tests.",
                steps: [
                    WorkStep(
                        id: "1", title: "Refresh the lockfile", project: "space-webapp", state: .asking,
                        sessions: [chatId], note: "Asking to reach the npm registry", startedAt: now - 6 * minute),
                    WorkStep(id: "2", title: "Run the build and the unit tests", project: "space-webapp"),
                ],
                state: .running, chatId: "chat-atlas", toolCallId: "call-plan-deps",
                createdAt: now - 8 * minute, startedAt: now - 6 * minute,
                decision: PlanDecision(approve: true, at: now - 7 * minute)))
        let question = """
            Run this command outside the sandbox?

            Context:
            Purpose: Refresh the lockfile from the public registry
            Command: npm install --package-lock-only --registry https://registry.npmjs.org
            Pattern: npm install …
            Reason: The sandbox has no network; the lockfile needs the registry.
            Note: It runs as you, with your files, keys, keychain and network.
            """
        sessionStore(for: chatId).setEntries([
            MessageEntry(
                id: "w6-u", role: .user,
                parts: [.text(id: "t0", text: "Refresh the webapp's lockfile, then run the build.")],
                createdAt: now - 6 * minute, deviceId: "dev-mac", status: .complete),
            MessageEntry(
                id: "w6-a", role: .assistant,
                parts: [
                    .text(
                        id: "t1", text: "The lockfile is two minor versions behind. Refreshing it needs the registry:"),
                    .tool(
                        id: "w6-npm",
                        call: RenderToolCall(
                            tag: "exec",
                            fields: [
                                "command": "npm install --package-lock-only --registry https://registry.npmjs.org"
                            ],
                            permission: ToolPermission(outcome: .outside, scope: .chat, purpose: "Refresh the lockfile")
                        ),
                        isError: false, resolved: false),
                    .input(
                        id: "w6-ask", requestId: "w6-ask",
                        questions: [
                            UserInputQuestion(
                                id: "q1", header: "Permission", question: question,
                                options: ["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"],
                                permission: PermissionPrompt(kind: "escalate", toolCallId: "w6-npm"))
                        ], resolved: false),
                ], createdAt: now - minute, deviceId: "dev-mac", status: .complete),
        ])
        botRecords.approvals.append(
            BotApproval(
                id: "approval-long", botId: Self.atlasId, threadChatId: "chat-atlas-thread-flaky",
                workId: "work-flaky",
                action: """
                    Push the branch atlas/reconnect-race to origin and open a pull request against main \
                    with the e2e results and the flaky-test history attached
                    """,
                reason: """
                    Opening a pull request is visible to everyone on the repository, and Atlas's \
                    instructions say never to push without asking first.
                    """,
                createdAt: now - 3 * minute))
        guard many else { return }
        let actions = [
            (
                "Start a session in cypher: bisect the snapshot decoding failure",
                "The session was not started by Atlas."
            ),
            ("Answer Harbor's question about the release date", "Harbor is a connected workspace."),
            ("Delete the stale atlas/* branches on origin", "Deleting branches can't be undone."),
            ("Run the nightly tests again on the release branch", "The routine already ran today."),
            ("Share the 0.9 release notes with Ledger", "Ledger is a connected workspace."),
            ("Start a session in webapp: update the screenshots", "The project is outside the plan."),
        ]
        for (ix, (action, reason)) in actions.enumerated() {
            botRecords.approvals.append(
                BotApproval(
                    id: "approval-many-\(ix)", botId: Self.atlasId, threadChatId: "chat-atlas-thread-flaky",
                    workId: "work-flaky", action: action, reason: reason,
                    createdAt: now - Int64(30 + ix * 7) * minute))
        }
    }
}

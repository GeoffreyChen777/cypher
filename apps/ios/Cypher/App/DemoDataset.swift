// Offline demo dataset — realistic spaces/sessions/transcripts so the app can
// be explored (and screenshotted) with no edge deployment. The flagship chat
// streams a reply on demand, exercising the live-row pipeline: incremental
// re-parse, veil fade-in, stick-to-bottom.

import Foundation
import Observation

@MainActor
@Observable
final class DemoDataset {
    var devices: [DeviceRow]
    var spaces: [Space]
    var chats: [Chat]
    var sessions: [String: SessionRow]
    private var stores: [String: SessionStore] = [:]
    private var streamTask: Task<Void, Never>?

    private static let dummyConfig = AppConfig(
        edgeURL: URL(string: "http://localhost:8787")!, mode: .dev,
        userId: "demo", orgId: "demo", deviceId: "ios-demo", deviceName: "iPhone")

    init(devices: [DeviceRow], spaces: [Space], chats: [Chat], sessions: [String: SessionRow]) {
        self.devices = devices
        self.spaces = spaces
        self.chats = chats
        self.sessions = sessions
    }

    static func standard() -> DemoDataset {
        let now = nowMs()
        let mac = DeviceRow(id: "dev-mac", name: "MacBook Pro", platform: "macos",
                            lastSeenAt: now, createdAt: now - 86_400_000 * 30)
        let vps = DeviceRow(id: "dev-vps", name: "hetzner-01", platform: "linux",
                            lastSeenAt: now - 600_000, createdAt: now - 86_400_000 * 12)
        let cypher = Space(id: "space-cypher", deviceId: "dev-mac",
                          path: "/Users/dev/cypher", name: nil, gitDetected: true,
                          gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000 * 9)
        let edge = Space(id: "space-edge", deviceId: "dev-vps",
                         path: "/srv/deploys/edge", name: nil, gitDetected: true,
                         gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000 * 4)

        let claude = ChatConfig(harness: "pi", model: "demo/pi",
                                reasoning: "high", sandbox: "workspace-write")
        let codex = ChatConfig(harness: "pi", model: "demo/pi",
                               reasoning: "high", sandbox: "workspace-write")

        var chats = [
            Chat(id: "chat-veil", deviceId: "dev-mac", title: "Streaming veil on transcript rows",
                 archived: false, cwd: "/Users/dev/.cypher/worktrees/cypher-veil-fade",
                 branch: "veil-fade", checkoutId: nil,
                 config: claude, lastMessagePreview: "Porting the paint-only fade…",
                 lastMessageAt: now - 40_000, createdAt: now - 3_600_000,
                 spaceId: cypher.id, lastSeenAt: now),
            Chat(id: "chat-picker", deviceId: "dev-mac", title: "Model picker catalog sync",
                 archived: false, cwd: cypher.path, branch: "main", checkoutId: nil,
                 config: claude, lastMessagePreview: "Which device owns the catalog?",
                 lastMessageAt: now - 120_000, createdAt: now - 7_200_000,
                 spaceId: cypher.id, lastSeenAt: now - 130_000),
            Chat(id: "chat-tabs", deviceId: "dev-mac", title: "Tool group header colors",
                 archived: false, cwd: cypher.path, branch: "main", checkoutId: nil,
                 config: codex, lastMessagePreview: "Done — failed children stay quiet.",
                 lastMessageAt: now - 900_000, createdAt: now - 86_400_000,
                 spaceId: cypher.id, lastSeenAt: now - 3_600_000),
            Chat(id: "chat-deploy", deviceId: "dev-vps", title: "Wrangler deploy hygiene",
                 archived: false, cwd: edge.path, branch: nil, checkoutId: nil,
                 config: claude, lastMessagePreview: "Hibernation-safe flush timer",
                 lastMessageAt: now - 86_400_000, createdAt: now - 86_400_000 * 2,
                 spaceId: edge.id, lastSeenAt: now - 86_400_000),
            // Archived — populate the shelf under the active list.
            Chat(id: "chat-oklch", deviceId: "dev-mac", title: "OKLCH conversion drift",
                 archived: true, cwd: cypher.path, branch: "main", checkoutId: nil,
                 config: claude, lastMessagePreview: "Gamma encode matches now.",
                 lastMessageAt: now - 86_400_000 * 3, createdAt: now - 86_400_000 * 4,
                 spaceId: cypher.id, lastSeenAt: now - 86_400_000 * 3),
            Chat(id: "chat-presence", deviceId: "dev-vps", title: "Presence beat coalescing",
                 archived: true, cwd: edge.path, branch: nil, checkoutId: nil,
                 config: codex, lastMessagePreview: "Batched to one beat per 25s.",
                 lastMessageAt: now - 86_400_000 * 6, createdAt: now - 86_400_000 * 7,
                 spaceId: edge.id, lastSeenAt: now - 86_400_000 * 6),
        ]
        var sessions: [String: SessionRow] = [
            "chat-veil": SessionRow(chatId: "chat-veil", deviceId: "dev-mac", status: .working,
                                    startedAt: now - 95_000, updatedAt: now - 5_000),
            "chat-picker": SessionRow(chatId: "chat-picker", deviceId: "dev-mac",
                                      status: .awaitingInput, startedAt: now - 400_000,
                                      updatedAt: now - 10_000),
        ]
        // Explicitly offline fixtures for the mobile Subagents inspector.
        // Durable children remain reopenable even without parent snapshots.
        var planner = chats[0]
        planner.id = "demo-child-planner"
        planner.title = "Planner · mobile implementation"
        planner.child = ChildChat(parentChatId: "chat-veil", parentRunId: "demo-run-planner",
            agent: "planner", task: "Plan the mobile subagents inspector.", mode: .async,
            toolCallId: "demo-tool-planner")
        planner.createdAt = now - 40_000
        var reviewer = planner
        reviewer.id = "demo-child-reviewer"
        reviewer.title = "Reviewer · state semantics"
        reviewer.child = ChildChat(parentChatId: "chat-veil", parentRunId: "demo-run-reviewer",
            agent: "reviewer", task: "Check stale status and durable child navigation.", mode: .sync,
            toolCallId: "demo-tool-reviewer")
        chats += [planner, reviewer]
        sessions[planner.id] = SessionRow(chatId: planner.id, deviceId: "dev-mac",
            status: .working, startedAt: now - 40_000, updatedAt: now)
        sessions[reviewer.id] = SessionRow(chatId: reviewer.id, deviceId: "dev-mac",
            status: .idle, startedAt: nil, updatedAt: now - 5_000)
        sessions["chat-veil"]?.subagents = [
            SubagentRun(runId: "demo-run-planner", toolCallId: "demo-tool-planner",
                agent: "planner", model: "demo/pi", task: "Plan the mobile subagents inspector.",
                mode: .async, status: .running, progress: "Checking the shared session schema…",
                startedAt: now - 40_000, updatedAt: now, childChatId: planner.id),
            SubagentRun(runId: "demo-run-reviewer", toolCallId: "demo-tool-reviewer",
                agent: "reviewer", model: "demo/pi", task: "Check state semantics.",
                mode: .sync, status: .done, progress: "Review complete.",
                startedAt: now - 50_000, updatedAt: now - 5_000, endedAt: now - 5_000,
                childChatId: reviewer.id),
        ]
        return DemoDataset(devices: [mac, vps], spaces: [cypher, edge],
                           chats: chats, sessions: sessions)
    }

    // MARK: Fake filesystem (folder browser demo)

    static let fileTree: [String: [String]] = [
        "/Users/dev": ["Documents", "Downloads", "Projects", "scratch"],
        "/Users/dev/Documents": ["notes", "specs"],
        "/Users/dev/Projects": ["cypher", "dotfiles", "blog", "playground"],
        "/Users/dev/Projects/cypher": ["apps", "crates", "docs", "edge"],
        "/Users/dev/Projects/blog": ["content", "public"],
        "/srv": ["deploys", "backups"],
        "/srv/deploys": ["edge", "landing"],
    ]

    func homePath(deviceId: String) -> String {
        deviceId == "dev-vps" ? "/srv" : "/Users/dev"
    }

    private static let repoNames: Set<String> = ["cypher", "dotfiles", "blog", "playground", "edge", "landing"]

    func listFolders(deviceId: String, path: String) -> FolderListing {
        let entries = (Self.fileTree[path] ?? []).map { name in
            FolderEntry(name: name, isDir: true, isRepo: Self.repoNames.contains(name))
        }
        return FolderListing(path: path, entries: entries, truncated: false)
    }

    private var refsByPath: [String: [RepoRef]] = [:]

    func listRefs(spacePath: String) -> [RepoRef] {
        if let cached = refsByPath[spacePath] { return cached }
        let seeded: [RepoRef]
        if spacePath.contains("cypher") {
            seeded = [
                RepoRef(name: "main", current: true, worktreePath: nil),
                RepoRef(name: "veil-fade", current: false,
                        worktreePath: "/Users/dev/.cypher/worktrees/cypher-veil-fade"),
                RepoRef(name: "feature/diff-pane", current: false, worktreePath: nil),
                RepoRef(name: "fix/tool-colors", current: false, worktreePath: nil),
            ]
        } else {
            seeded = [
                RepoRef(name: "main", current: true, worktreePath: nil),
                RepoRef(name: "staging", current: false, worktreePath: nil),
            ]
        }
        refsByPath[spacePath] = seeded
        return seeded
    }

    /// git checkout simulation: move the `current` marker in the repo at path.
    func switchRef(path: String, refName: String) {
        var refs = listRefs(spacePath: path)
        for ix in refs.indices {
            refs[ix].current = refs[ix].name == refName
        }
        refsByPath[path] = refs
    }

    func createWorktree(spacePath: String, base: String) -> String {
        let slug = base.replacingOccurrences(of: "/", with: "-")
        let path = "/Users/dev/.cypher/worktrees/\((spacePath as NSString).lastPathComponent)-\(slug)"
        var refs = listRefs(spacePath: spacePath)
        if let ix = refs.firstIndex(where: { $0.name == base }), refs[ix].worktreePath == nil {
            refs[ix].worktreePath = path
        }
        refsByPath[spacePath] = refs
        return path
    }

    func sessionStore(for chatId: String) -> SessionStore {
        if let existing = stores[chatId] { return existing }
        let store = SessionStore(chatId: chatId, config: Self.dummyConfig, offline: true)
        store.setEntries(Self.transcript(for: chatId))
        store.demoResponder = { [weak self, weak store] prompt, isSteer in
            guard let self, let store else { return }
            self.simulateTurn(store: store, chatId: chatId, prompt: prompt, isSteer: isSteer)
        }
        stores[chatId] = store
        return store
    }

    // MARK: Scripted transcripts

    private static func transcript(for chatId: String) -> [MessageEntry] {
        let now = nowMs()
        switch chatId {
        case "demo-child-planner", "demo-child-reviewer":
            return [
                MessageEntry(id: "child-user", role: .user,
                    parts: [.text(id: "task", text: "Review the mobile subagent experience.")],
                    createdAt: now - 40_000, deviceId: "dev-mac", status: .complete),
                MessageEntry(id: "child-reply", role: .assistant,
                    parts: [.text(id: "result", text: """
                    This is a **demo child session**. In a live workspace, this page shows the \
                    subagent's real transcript on its host device.

                    - Open the parent with the return arrow in the title bar.
                    - Continue here using the same Pi profile and working directory.
                    - Child sessions stay out of the project's main session list.
                    """)], createdAt: now - 30_000, deviceId: "dev-mac", status: .complete),
            ]
        case "chat-veil":
            return [
                MessageEntry(id: "m1", role: .user, parts: [
                    .text(id: "t0", text: "Port the streaming fade-in veil from the desktop transcript. It must never affect layout — opacity only, split at chunk boundaries."),
                ], createdAt: now - 3_500_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m2", role: .assistant, parts: [
                    .text(id: "t0", text: """
                    ## Veil port plan

                    The desktop veil (`veil.rs`) multiplies a fading alpha into each appended \
                    chunk's text color — **paint-layer only**, so shaping and wrapping never change. \
                    Three invariants to carry over:

                    1. Chunk spans keep their *exact* byte length when split
                    2. Fade duration tracks the append cadence: `clamp(ema × 3, 120, 400)` ms
                    3. Re-attach seeds the baseline — only post-switch appends animate

                    | Constant | Value |
                    | --- | --- |
                    | `VEIL_MIN_FADE_MS` | 120 |
                    | `VEIL_MAX_FADE_MS` | 400 |
                    | `VEIL_CURVE_POW` | 1.6 |

                    > The curve is `1 − (1−p)^1.6` — fast attack, soft landing.
                    """),
                    .tool(id: "tool1", call: RenderToolCall(tag: "readFile", fields: ["path": "crates/ui/src/markdown/veil.rs"]), isError: false, resolved: true),
                    .tool(id: "tool2", call: RenderToolCall(tag: "editFile", fields: ["path": "Cypher/Transcript/Veil.swift"]), isError: false, resolved: true),
                    .tool(id: "tool3", call: RenderToolCall(tag: "exec", fields: ["command": "xcodebuild -scheme Cypher build"]), isError: false, resolved: true),
                    .text(id: "t1", text: """
                    Implementation lands in `Veil.swift`:

                    ```swift
                    func veilOpacity(_ p: Double) -> Double {
                        1 - pow(1 - p, 1.6)  // fast attack, soft landing
                    }

                    // Duration follows the streaming cadence EMA.
                    let duration = min(max(ema * 3, 120), 400)
                    ```

                    The row keeps one `RowVeil` while streaming and drops it on the \
                    live→complete flip, exactly like the desktop lifecycle.
                    """),
                ], createdAt: now - 3_400_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            ]
        case "chat-picker":
            // Match the real Pi RPC fallback, including the duplicated title.
            let question = """
            Which device should serve harness/model catalogs for the picker?

            Context:
            Each device has its own installed harnesses and model catalog.
            After switching devices, the picker can still show the previous catalog.
            The project's owning device is the source of truth for its sessions.
            """
            return [
                MessageEntry(id: "m1", role: .user, parts: [
                    .text(id: "t0", text: "The model picker shows stale catalogs after switching devices — where should the catalog come from?"),
                ], createdAt: now - 400_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m2", role: .assistant, parts: [
                    .text(id: "t0", text: "Two viable sources — the local device's harness install, or the space's owning device. The desktop recently moved to the latter (`aa128a6`). Before I wire the RPC, one decision:"),
                    .input(id: "req-1", requestId: "req-1", questions: [
                        UserInputQuestion(id: "q1", header: question,
                                          question: question,
                                          options: [
                                            "Space's device (Recommended)",
                                            "Local device",
                                            "Union of both",
                                          ], multiSelect: false),
                    ], resolved: false),
                ], createdAt: now - 380_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            ]
        case "chat-tabs":
            return [
                MessageEntry(id: "m1", role: .user, parts: [
                    .text(id: "t0", text: "Tool group headers turn red when any child fails — they should stay quiet, chips carry the error."),
                ], createdAt: now - 1_000_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m2", role: .assistant, parts: [
                    .tool(id: "tool1", call: RenderToolCall(tag: "search", fields: ["pattern": "group_header_color"]), isError: false, resolved: true),
                    .tool(id: "tool2", call: RenderToolCall(tag: "exec", fields: ["command": "cargo test -p cypher-ui tool_group"]), isError: true, resolved: true),
                    .tool(id: "tool3", call: RenderToolCall(tag: "editFile", fields: ["path": "crates/ui/src/shell/transcript.rs"]), isError: false, resolved: true),
                    .text(id: "t0", text: "Done — the header keeps `text_muted` even on failure; only the chip label and the summary segment (\"1 failed\") pick up `danger`. Matches the desktop fix in `1749890`."),
                ], createdAt: now - 950_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                // Short follow-ups keep multiple exchange boundaries visible
                // together for checking bubble and inter-exchange spacing.
                MessageEntry(id: "m3", role: .user, parts: [
                    .text(id: "t0", text: "浅色模式下，失败提示也保持这个规则吗？"),
                ], createdAt: now - 900_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m4", role: .assistant, parts: [
                    .text(id: "t0", text: "是的。标题保持中性色，只有失败标记使用 `danger`，浅色和深色模式一致。"),
                ], createdAt: now - 850_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                MessageEntry(id: "m5", role: .user, parts: [
                    .text(id: "t0", text: "再确认一下，每轮对话之间留白更大。"),
                ], createdAt: now - 800_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m6", role: .assistant, parts: [
                    .text(id: "t0", text: "已调整：上一轮回复到下一条提问为 **36 pt**，同一轮提问到回复仍是 **14 pt**。"),
                ], createdAt: now - 750_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                MessageEntry(id: "m7", role: .user, parts: [
                    .text(id: "t0", text: "先不用跑测试，继续改 UI。"),
                ], createdAt: now - 740_000, deviceId: "ios-demo", status: .complete, isSteer: true),
                MessageEntry(id: "m8", role: .assistant, parts: [
                    .text(id: "t0", text: "继续调整界面。这条追加指令用小标签和细边框区分，仍属于当前这一轮。"),
                ], createdAt: now - 730_000, deviceId: "dev-mac", status: .complete),
            ]
        case "chat-deploy":
            return [
                MessageEntry(id: "m1", role: .user, parts: [
                    .text(id: "t0", text: "Audit the wrangler config for hibernation hygiene."),
                ], createdAt: now - 86_500_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(id: "m2", role: .assistant, parts: [
                    .text(id: "t0", text: "Flush timer now only arms while dirty; ping/pong uses the auto-response path so the DO never wakes for keepalives."),
                ], createdAt: now - 86_400_000, deviceId: "dev-vps", status: .complete, continuationOf: nil),
            ]
        default:
            return []  // freshly minted chats start empty
        }
    }

    // MARK: Streaming simulation

    private func simulateTurn(store: SessionStore, chatId: String, prompt: String, isSteer: Bool) {
        streamTask?.cancel()
        let now = nowMs()
        var entries = store.entries
        entries.append(MessageEntry(id: "u-\(now)", role: .user, parts: [
            .text(id: "t0", text: prompt),
        ], createdAt: now, deviceId: "ios-demo", status: .complete, continuationOf: nil, isSteer: isSteer))
        let liveId = "a-\(now)"
        entries.append(MessageEntry(id: liveId, role: .assistant, parts: [
            .text(id: "t0", text: ""),
        ], createdAt: now, deviceId: "dev-mac", status: .streaming, continuationOf: nil))
        store.setEntries(entries)
        sessions[chatId] = SessionRow(chatId: chatId, deviceId: "dev-mac", status: .working,
                                      startedAt: now, updatedAt: now)

        let reply = """
        Here's how the streamed reply renders on this device:

        - Markdown re-parses **only the tail** — the last two top-level blocks
        - New text fades in through the paint-only veil
        - The transcript stays glued to the bottom until you scroll up

        ```rust
        // The desktop constant carries over verbatim.
        const STREAM_COMMIT_MS: u64 = 120;
        ```

        When the turn settles, this entry flips `streaming → complete`, the veil \
        drops, and the row ids stay stable so nothing flickers.
        """
        let words = reply.split(separator: " ", omittingEmptySubsequences: false)

        streamTask = Task { [weak self, weak store] in
            var text = ""
            for (ix, word) in words.enumerated() {
                if Task.isCancelled { return }
                text += (ix == 0 ? "" : " ") + word
                guard let store else { return }
                var current = store.entries
                guard let last = current.indices.last, current[last].id == liveId else { return }
                current[last].parts = [.text(id: "t0", text: text)]
                store.setEntries(current)
                try? await Task.sleep(nanoseconds: UInt64.random(in: 30_000_000...140_000_000))
            }
            guard let self, let store else { return }
            var current = store.entries
            if let last = current.indices.last, current[last].id == liveId {
                current[last].status = .complete
                store.setEntries(current)
            }
            let end = nowMs()
            self.sessions[chatId] = SessionRow(chatId: chatId, deviceId: "dev-mac", status: .idle,
                                               startedAt: nil, updatedAt: end)
            if let ix = self.chats.firstIndex(where: { $0.id == chatId }) {
                self.chats[ix].lastMessageAt = end
                self.chats[ix].lastMessagePreview = "When the turn settles, this entry flips…"
                self.chats[ix].lastSeenAt = end
            }
        }
    }
}

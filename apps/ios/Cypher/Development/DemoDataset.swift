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
    /// Settings → Permissions in demo mode: on, with command rules of each kind.
    var demoPermissionSettings = PermissionSettings(raw: [
        "enabled": .bool(true),
        "approval": .object([
            "escalate": .string("assisted"), "externalWrite": .string("assisted"),
            "protectedWrite": .string("assisted"), "sensitiveRead": .string("assisted"),
            "sensitiveWrite": .string("assisted"), "network": .string("assisted"), "tools": .string("assisted"),
        ]),
        "judgeModel": .string(""),
        "toolsDefault": .string("allow"),
        "toolOverrides": .object([:]),
        "sensitive": .array([.string("**/secrets.yaml")]),
        "protected": .array([]),
        "writableSupport": .array([]),
        "localBinding": .bool(true),
        "unixSockets": .array([]),
        "networkMode": .string("open"),
        "allowDomains": .array([]),
        "rules": .array([
            .object(["capability": .string("write"), "path": .string("/Users/demo/Developer/shared")]),
            .object(["capability": .string("read"), "path": .string("tool:mcp__github__create_issue")]),
        ]),
        "commands": .array([
            .object([
                "match": .array([.string("ssh"), .string("deploy@web"), .string("**")]),
                "decision": .string("allow"), "label": .string("Check the app on the server web"),
            ]),
            .object([
                "match": .array([.string("./scripts/deploy.sh"), .string("**")]),
                "decision": .string("allow"), "label": .string("Deploy the web app"),
                "sha256": .string(String(repeating: "a", count: 64)),
            ]),
            .object([
                "match": .array([.string("git"), .string("push"), .string("**"), .string("--force"), .string("**")]),
                "decision": .string("ask"), "label": .string("Force-push"),
            ]),
            .object([
                "match": .array([.string("kubectl"), .string("delete"), .string("**")]),
                "decision": .string("deny"),
            ]),
        ]),
    ])
    private var stores: [String: SessionStore] = [:]
    private var streamTask: Task<Void, Never>?
    /// The context recovery replay (DemoDataset+ContextRecovery.swift).
    @ObservationIgnored var recoveryTask: Task<Void, Never>?
    /// The run progress replay (DemoDataset+RunProgress.swift).
    @ObservationIgnored var progressTask: Task<Void, Never>?

    static let dummyConfig = AppConfig(
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
        let mac = DeviceRow(
            id: "dev-mac", name: "MacBook Pro", platform: "macos",
            lastSeenAt: now, createdAt: now - 86_400_000 * 30)
        let vps = DeviceRow(
            id: "dev-vps", name: "hetzner-01", platform: "linux",
            lastSeenAt: now - 600_000, createdAt: now - 86_400_000 * 12)
        let cypher = Space(
            id: "space-cypher", deviceId: "dev-mac",
            path: "/Users/dev/cypher", name: nil, gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000 * 9)
        let edge = Space(
            id: "space-edge", deviceId: "dev-vps",
            path: "/srv/deploys/edge", name: nil, gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000 * 4)

        let claude = ChatConfig(
            harness: "pi", model: "demo/pi",
            reasoning: "high", sandbox: "workspace-write")
        let codex = ChatConfig(
            harness: "pi", model: "demo/pi",
            reasoning: "high", sandbox: "workspace-write")

        var chats = [
            Chat(
                id: "chat-veil", deviceId: "dev-mac", title: "Streaming veil on transcript rows",
                archived: false, cwd: "/Users/dev/.cypher/worktrees/cypher-veil-fade",
                branch: "veil-fade", checkoutId: nil,
                config: claude, lastMessagePreview: "Porting the paint-only fade…",
                lastMessageAt: now - 40_000, createdAt: now - 3_600_000,
                spaceId: cypher.id, lastSeenAt: now),
            Chat(
                id: "chat-picker", deviceId: "dev-mac", title: "Model picker catalog sync",
                archived: false, cwd: cypher.path, branch: "main", checkoutId: nil,
                config: claude, lastMessagePreview: "Which device owns the catalog?",
                lastMessageAt: now - 120_000, createdAt: now - 7_200_000,
                spaceId: cypher.id, lastSeenAt: now - 130_000),
            Chat(
                id: "chat-tabs", deviceId: "dev-mac", title: "Tool group header colors",
                archived: false, cwd: cypher.path, branch: "main", checkoutId: nil,
                config: codex, lastMessagePreview: "Done — failed children stay quiet.",
                lastMessageAt: now - 900_000, createdAt: now - 86_400_000,
                spaceId: cypher.id, lastSeenAt: now - 3_600_000),
            // LaTeX in a reply (the mock harness's [mock:math] showcase).
            Chat(
                id: "chat-math", deviceId: "dev-mac", title: "Quadratic formula and friends",
                archived: false, cwd: cypher.path, branch: "main", checkoutId: nil,
                config: claude, lastMessagePreview: "Here is the quadratic formula.",
                lastMessageAt: now - 2_000_000, createdAt: now - 2_100_000,
                spaceId: cypher.id, lastSeenAt: now - 2_000_000),
            Chat(
                id: "chat-deploy", deviceId: "dev-vps", title: "Wrangler deploy hygiene",
                archived: false, cwd: edge.path, branch: nil, checkoutId: nil,
                config: claude, lastMessagePreview: "Hibernation-safe flush timer",
                lastMessageAt: now - 86_400_000, createdAt: now - 86_400_000 * 2,
                spaceId: edge.id, lastSeenAt: now - 86_400_000),
            // Archived — populate the shelf under the active list.
            Chat(
                id: "chat-oklch", deviceId: "dev-mac", title: "OKLCH conversion drift",
                archived: true, cwd: cypher.path, branch: "main", checkoutId: nil,
                config: claude, lastMessagePreview: "Gamma encode matches now.",
                lastMessageAt: now - 86_400_000 * 3, createdAt: now - 86_400_000 * 4,
                spaceId: cypher.id, lastSeenAt: now - 86_400_000 * 3),
            Chat(
                id: "chat-presence", deviceId: "dev-vps", title: "Presence beat coalescing",
                archived: true, cwd: edge.path, branch: nil, checkoutId: nil,
                config: codex, lastMessagePreview: "Batched to one beat per 25s.",
                lastMessageAt: now - 86_400_000 * 6, createdAt: now - 86_400_000 * 7,
                spaceId: edge.id, lastSeenAt: now - 86_400_000 * 6),
        ]
        var sessions: [String: SessionRow] = [
            "chat-veil": SessionRow(
                chatId: "chat-veil", deviceId: "dev-mac", status: .working,
                startedAt: now - 95_000, updatedAt: now - 5_000,
                contextUsage: ContextUsage(used: 124_000, size: 200_000),
                progress: Self.runProgressReadings.first),
            // Idle, near full: the composer's context ring in amber.
            "chat-tabs": SessionRow(
                chatId: "chat-tabs", deviceId: "dev-mac", status: .idle,
                startedAt: nil, updatedAt: now - 900_000,
                contextUsage: ContextUsage(used: 162_000, size: 200_000)),
            "chat-picker": SessionRow(
                chatId: "chat-picker", deviceId: "dev-mac",
                status: .awaitingInput, startedAt: now - 400_000,
                updatedAt: now - 10_000),
        ]
        // Explicitly offline fixtures for the mobile Subagents inspector.
        // Durable children remain reopenable even without parent snapshots.
        var planner = chats[0]
        planner.id = "demo-child-planner"
        planner.title = "Planner · mobile implementation"
        planner.child = ChildChat(
            parentChatId: "chat-veil", parentRunId: "demo-run-planner",
            agent: "planner", task: "Plan the mobile subagents inspector.", mode: .async,
            toolCallId: "demo-tool-planner")
        planner.createdAt = now - 40_000
        var reviewer = planner
        reviewer.id = "demo-child-reviewer"
        reviewer.title = "Reviewer · state semantics"
        reviewer.child = ChildChat(
            parentChatId: "chat-veil", parentRunId: "demo-run-reviewer",
            agent: "reviewer", task: "Check stale status and durable child navigation.", mode: .sync,
            toolCallId: "demo-tool-reviewer")
        chats += [planner, reviewer]
        sessions[planner.id] = SessionRow(
            chatId: planner.id, deviceId: "dev-mac",
            status: .working, startedAt: now - 40_000, updatedAt: now)
        sessions[reviewer.id] = SessionRow(
            chatId: reviewer.id, deviceId: "dev-mac",
            status: .idle, startedAt: nil, updatedAt: now - 5_000)
        sessions["chat-veil"]?.subagents = [
            SubagentRun(
                runId: "demo-run-planner", toolCallId: "demo-tool-planner",
                agent: "planner", model: "demo/pi", task: "Plan the mobile subagents inspector.",
                mode: .async, status: .running, progress: "Checking the shared session schema…",
                startedAt: now - 40_000, updatedAt: now, childChatId: planner.id),
            SubagentRun(
                runId: "demo-run-reviewer", toolCallId: "demo-tool-reviewer",
                agent: "reviewer", model: "demo/pi", task: "Check state semantics.",
                mode: .sync, status: .done, progress: "Review complete.",
                startedAt: now - 50_000, updatedAt: now - 5_000, endedAt: now - 5_000,
                childChatId: reviewer.id),
        ]
        // Permission control: a settled deploy with every kind of permission
        // row (at Full access, so the composer shows the amber shield), and a
        // chat waiting on a "Run this command outside the sandbox?" card.
        let webapp = Space(
            id: "space-webapp", deviceId: "dev-mac",
            path: "/Users/dev/webapp", name: nil, gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 86_400_000 * 2)
        var fullAccess = claude
        fullAccess.sandbox = "danger-full-access"
        chats += [
            Chat(
                id: "chat-permissions-story", deviceId: "dev-mac", title: "Deploy the web app",
                archived: false, cwd: webapp.path, branch: "main", checkoutId: nil,
                config: fullAccess, lastMessagePreview: "Done: the app on web is running again.",
                lastMessageAt: now - 300_000, createdAt: now - 900_000,
                spaceId: webapp.id, lastSeenAt: now - 300_000),
            Chat(
                id: "chat-permissions-card", deviceId: "dev-mac", title: "Is the app on web healthy?",
                archived: false, cwd: webapp.path, branch: "main", checkoutId: nil,
                config: claude, lastMessagePreview: "Let me check the app on web.",
                lastMessageAt: now - 60_000, createdAt: now - 120_000,
                spaceId: webapp.id, lastSeenAt: now - 100_000),
        ]
        sessions["chat-permissions-card"] = SessionRow(
            chatId: "chat-permissions-card", deviceId: "dev-mac",
            status: .awaitingInput, startedAt: now - 70_000, updatedAt: now - 60_000)
        // Context recovery: a project of its own, one session per state.
        let (recoverySpace, recoveryChats, recoverySessions) = recoveryShowcase(now: now, config: claude)
        chats += recoveryChats
        sessions.merge(recoverySessions) { _, new in new }
        return DemoDataset(
            devices: [mac, vps], spaces: [cypher, edge, webapp, recoverySpace],
            chats: chats, sessions: sessions)
    }

    /// `-demo-many`: enough devices and projects to scroll Home and overflow
    /// its device tabs. Idle sessions only, so the standard fixtures keep
    /// their statuses.
    func addManyProjects() {
        let now = nowMs()
        let config = ChatConfig(
            harness: "pi", model: "demo/pi",
            reasoning: "high", sandbox: "workspace-write")
        let extra: [(DeviceRow, [String])] = [
            (
                DeviceRow(
                    id: "dev-studio", name: "Studio's Mac mini", platform: "macos",
                    lastSeenAt: now, createdAt: now - 86_400_000 * 20),
                ["collabmd", "pi-extensions", "dotfiles", "blog"]
            ),
            (
                DeviceRow(
                    id: "dev-gpu", name: "5090 Workstation", platform: "linux",
                    lastSeenAt: now, createdAt: now - 86_400_000 * 15),
                ["mvp-engine", "npu-slurm-setup", "Documents"]
            ),
            (
                DeviceRow(
                    id: "dev-alpha", name: "DRC Alpha", platform: "linux",
                    lastSeenAt: now - 86_400_000, createdAt: now - 86_400_000 * 40),
                ["landing", "playground", "benchmarks", "infra", "notes"]
            ),
        ]
        for (device, names) in extra {
            devices.append(device)
            for (ix, name) in names.enumerated() {
                let space = Space(
                    id: "space-\(device.id)-\(name)", deviceId: device.id,
                    path: "/Users/dev/Projects/\(name)", name: nil,
                    gitDetected: ix % 2 == 0, gitCheckedAt: now, checkoutId: nil,
                    createdAt: now - 86_400_000 * Int64(ix + 1))
                spaces.append(space)
                for n in 0..<(ix % 3) {
                    let at = now - 3_600_000 * Int64(ix * 3 + n + 1)
                    chats.append(
                        Chat(
                            id: "chat-\(space.id)-\(n)", deviceId: device.id,
                            title: "\(name) task \(n + 1)", archived: false,
                            cwd: space.path, branch: "main", checkoutId: nil,
                            config: config, lastMessagePreview: "Done.",
                            lastMessageAt: at, createdAt: at, spaceId: space.id,
                            lastSeenAt: at))
                }
            }
        }
    }

    /// Pi's discovery shape (the plugins' own wording): extension commands,
    /// then the synthesized built-ins. With nothing typed the menu lists only
    /// the stateful ones; the rest are found by name.
    static let slashCommands: [SlashCommand] = [
        SlashCommand(name: "fast", description: "Toggle GPT Fast mode (service_tier: priority)", inputHint: nil),
        SlashCommand(
            name: "scripts",
            description: "Let the agent run several tools in one script. Faster, and uses less context.",
            inputHint: nil),
        SlashCommand(
            name: "goal", description: "Run a goal to completion: /goal [--tokens 100k] <goal_to_complete>",
            inputHint: nil),
        SlashCommand(
            name: "orchestrate",
            description: "Enable/disable adaptive subagent delegation (on|off|status, default off)",
            inputHint: nil),
        SlashCommand(name: "subagents", description: "List available subagents", inputHint: nil),
        SlashCommand(
            name: "subagent-status", description: "Show live subagent tasks and current-branch task history",
            inputHint: nil),
        SlashCommand(name: "review", description: "Review the working tree's changes", inputHint: nil),
        SlashCommand(name: "skill:frontend-design", description: "Load the frontend skill", inputHint: nil),
        SlashCommand(
            name: "compact", description: "Compact the conversation context (pi built-in)",
            inputHint: "custom instructions"),
        SlashCommand(
            name: "export-html", description: "Export the session to an HTML file (pi built-in)",
            inputHint: "output path"),
    ]

    /// The demo host's Pi switches: Scripts on (its default), the rest off.
    static let piSessionModes = PiSessionModes(fast: false, codemode: true, orchestrate: false)

    // MARK: Fake filesystem (folder browser demo)

    /// The demo checkout's files for `@` mentions (SearchFiles' answer:
    /// relative paths, folders marked).
    static let checkoutFiles: [FileSearchMatch] =
        [
            "README.md", "Cargo.toml", "apps/ios/Cypher/Composer/ComposerView.swift",
            "apps/ios/Cypher/Composer/Mentions.swift", "crates/ui/src/composer.rs",
            "crates/ui/src/transcript.rs", "crates/engine/src/repos.rs", "docs/chat2-sync.md",
        ].map { FileSearchMatch(path: $0, isDir: false) }
        + ["apps/ios", "crates/ui", "docs"].map { FileSearchMatch(path: $0, isDir: true) }

    func fileMatches(_ query: String) -> [FileSearchMatch] {
        let needle = query.lowercased()
        guard !needle.isEmpty else { return Array(Self.checkoutFiles.prefix(6)) }
        return Self.checkoutFiles.filter { $0.path.lowercased().contains(needle) }
    }

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

    func folderListing(at path: String) -> FolderListing {
        let entries = (Self.fileTree[path] ?? []).map { name in
            FolderEntry(name: name, isDir: true, isRepo: Self.repoNames.contains(name))
        }
        return FolderListing(path: path, entries: entries, truncated: false)
    }

    private var refsByPath: [String: [RepoRef]] = [:]

    func refs(spacePath: String) -> [RepoRef] {
        if let cached = refsByPath[spacePath] { return cached }
        let seeded: [RepoRef]
        if spacePath.contains("cypher") {
            seeded = [
                RepoRef(name: "main", current: true, worktreePath: nil),
                RepoRef(
                    name: "veil-fade", current: false,
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
    func checkOut(_ refName: String, in path: String) {
        var refs = self.refs(spacePath: path)
        for ix in refs.indices {
            refs[ix].current = refs[ix].name == refName
        }
        refsByPath[path] = refs
    }

    func addWorktree(spacePath: String, base: String) -> String {
        let slug = base.replacingOccurrences(of: "/", with: "-")
        let path = "/Users/dev/.cypher/worktrees/\((spacePath as NSString).lastPathComponent)-\(slug)"
        var refs = self.refs(spacePath: spacePath)
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
            if chatId == Self.recoveryReplayChatId {
                self.playRecoveryStory(store: store, chatId: chatId, prompt: prompt)
            } else {
                self.simulateTurn(store: store, chatId: chatId, prompt: prompt, isSteer: isSteer)
            }
        }
        stores[chatId] = store
        refreshRecoveryShowcase(chatId)
        if chatId == Self.runProgressChatId { playRunProgressStory() }
        if chatId == Self.recoveryReplayChatId {
            // Plays once the session has opened and settled.
            recoveryTask = Task { [weak self, weak store] in
                try? await Task.sleep(nanoseconds: 1_200_000_000)
                guard let self, let store, !Task.isCancelled else { return }
                self.playRecoveryStory(store: store, chatId: chatId, prompt: "Deploy the preview build to staging.")
            }
        }
        return store
    }

    // MARK: Scripted transcripts

    private static func transcript(for chatId: String) -> [MessageEntry] {
        let now = nowMs()
        switch chatId {
        case "demo-child-planner", "demo-child-reviewer":
            return [
                MessageEntry(
                    id: "child-user", role: .user,
                    parts: [.text(id: "task", text: "Review the mobile subagent experience.")],
                    createdAt: now - 40_000, deviceId: "dev-mac", status: .complete),
                MessageEntry(
                    id: "child-reply", role: .assistant,
                    parts: [
                        .text(
                            id: "result",
                            text: """
                                This is a **demo child session**. In a live workspace, this page shows the \
                                subagent's real transcript on its host device.

                                - Open the parent with the return arrow in the title bar.
                                - Continue here using the same Pi profile and working directory.
                                - Child sessions stay out of the project's main session list.
                                """)
                    ], createdAt: now - 30_000, deviceId: "dev-mac", status: .complete),
            ]
        case "chat-veil":
            return [
                MessageEntry(
                    id: "m1", role: .user,
                    parts: [
                        .text(
                            id: "t0",
                            text:
                                "Port the streaming fade-in veil from the desktop transcript. It must never affect layout — opacity only, split at chunk boundaries."
                        )
                    ], createdAt: now - 3_500_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m2", role: .assistant,
                    parts: [
                        .text(
                            id: "t0",
                            text: """
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
                        .tool(
                            id: "tool1",
                            call: RenderToolCall(tag: "readFile", fields: ["path": "crates/ui/src/markdown/veil.rs"]),
                            isError: false, resolved: true),
                        .tool(
                            id: "tool2",
                            call: RenderToolCall(tag: "editFile", fields: ["path": "Cypher/Transcript/Veil.swift"]),
                            isError: false, resolved: true),
                        .tool(
                            id: "tool3",
                            call: RenderToolCall(tag: "exec", fields: ["command": "xcodebuild -scheme Cypher build"]),
                            isError: false, resolved: true),
                        .text(
                            id: "t1",
                            text: """
                                Implementation lands in `Veil.swift`:

                                ```swift
                                func veilOpacity(_ p: Double) -> Double {
                                    1 - pow(1 - p, 1.6)  // fast attack, soft landing
                                }

                                // Duration follows the streaming cadence EMA.
                                let duration = min(max(ema * 3, 120), 400)
                                guard row === liveRow, duration != 0 else { return }
                                ```

                                The row keeps one `RowVeil` while streaming (`row === liveRow`) and \
                                drops it on the live→complete flip, exactly like the desktop lifecycle.
                                """),
                    ], createdAt: now - 3_400_000, deviceId: "dev-mac", status: .complete, continuationOf: nil,
                    models: [AnsweredModel(model: "claude-opus-5-5")]),
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
                MessageEntry(
                    id: "m1", role: .user,
                    parts: [
                        .text(
                            id: "t0",
                            text:
                                "The model picker shows stale catalogs after switching devices — where should the catalog come from?"
                        )
                    ], createdAt: now - 400_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m2", role: .assistant,
                    parts: [
                        .text(
                            id: "t0",
                            text:
                                "Two viable sources — the local device's harness install, or the space's owning device. The desktop recently moved to the latter (`aa128a6`). Before I wire the RPC, one decision:"
                        ),
                        .input(
                            id: "req-1", requestId: "req-1",
                            questions: [
                                UserInputQuestion(
                                    id: "q1", header: question,
                                    question: question,
                                    options: [
                                        "Space's device (Recommended)",
                                        "Local device",
                                        "Union of both",
                                    ], multiSelect: false)
                            ], resolved: false),
                    ], createdAt: now - 380_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            ]
        case "chat-tabs":
            // m4 is an append-mode translation (the agent's answer, the rule,
            // then the translation): its original folds behind "Show original".
            let answer =
                "Yes. The header stays neutral; only the failure marker uses `danger`, in light and dark mode alike."
            return [
                MessageEntry(
                    id: "m1", role: .user,
                    parts: [
                        .text(
                            id: "t0",
                            text:
                                "Tool group headers turn red when any child fails — they should stay quiet, chips carry the error."
                        )
                    ], createdAt: now - 1_000_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m2", role: .assistant,
                    parts: [
                        // Thinking folds behind a collapsed "Thought" toggle.
                        .reasoning(
                            id: "r0",
                            text:
                                "The header color is probably derived from the worst child status. Find where `group_header_color` is computed, then keep the header on `text_muted` and let only the failed chip turn red."
                        ),
                        .tool(
                            id: "tool1", call: RenderToolCall(tag: "search", fields: ["pattern": "group_header_color"]),
                            isError: false, resolved: true),
                        .tool(
                            id: "tool2",
                            call: RenderToolCall(
                                tag: "exec", fields: ["command": "cargo test -p cypher-ui tool_group"]), isError: true,
                            resolved: true),
                        .tool(
                            id: "tool3",
                            call: RenderToolCall(
                                tag: "editFile", fields: ["path": "crates/ui/src/shell/transcript.rs"]), isError: false,
                            resolved: true),
                        .text(
                            id: "t0",
                            text:
                                "Done — the header keeps `text_muted` even on failure; only the chip label and the summary segment (\"1 failed\") pick up `danger`. Matches the desktop fix in `1749890`."
                        ),
                    ], createdAt: now - 950_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                // Short follow-ups keep multiple exchange boundaries visible
                // together for checking bubble and inter-exchange spacing.
                MessageEntry(
                    id: "m3", role: .user,
                    parts: [
                        .text(id: "t0", text: "浅色模式下，失败提示也保持这个规则吗？")
                    ], createdAt: now - 900_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m4", role: .assistant,
                    parts: [
                        .text(
                            id: "t0",
                            text: answer + translationAppendSeparator
                                + "是的。标题保持中性色，只有失败标记使用 `danger`，浅色和深色模式一致。",
                            agentText: answer)
                    ], createdAt: now - 850_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m5", role: .user,
                    parts: [
                        .text(id: "t0", text: "再确认一下，每轮对话之间留白更大。")
                    ], createdAt: now - 800_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m6", role: .assistant,
                    parts: [
                        .text(id: "t0", text: "已调整：上一轮回复到下一条提问为 **36 pt**，同一轮提问到回复仍是 **14 pt**。")
                    ], createdAt: now - 750_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m7", role: .user,
                    parts: [
                        .text(id: "t0", text: "先不用跑测试，继续改 UI。")
                    ], createdAt: now - 740_000, deviceId: "ios-demo", status: .complete, isSteer: true),
                MessageEntry(
                    id: "m8", role: .assistant,
                    parts: [
                        .text(id: "t0", text: "继续调整界面。这条追加指令用小标签和细边框区分，仍属于当前这一轮。")
                    ], createdAt: now - 730_000, deviceId: "dev-mac", status: .complete),
            ]
        case "chat-deploy":
            return [
                MessageEntry(
                    id: "m1", role: .user,
                    parts: [
                        .text(id: "t0", text: "Audit the wrangler config for hibernation hygiene.")
                    ], createdAt: now - 86_500_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m2", role: .assistant,
                    parts: [
                        // A Pi codemode script and the calls it made (`{script}/{n}`
                        // ids), which the transcript nests under it.
                        .tool(
                            id: "ts1",
                            call: RenderToolCall(
                                tag: "unknown",
                                fields: [
                                    "name": "tool_search", "query": "cloudflare docs",
                                ]), isError: false, resolved: true),
                        .tool(
                            id: "s1",
                            call: RenderToolCall(
                                tag: "unknown",
                                fields: [
                                    "name": "codemode",
                                    "code": """

                                    const config = await tools.read({ path: "edge/wrangler.jsonc" });
                                    const docs = await tools.mcp__cloudflare_docs__search_cloudflare_documentation({
                                      query: "Durable Objects WebSocket hibernation auto-response",
                                    });
                                    return { bytes: config.length, docs };
                                    """,
                                ]), isError: false, resolved: true),
                        .tool(
                            id: "s1/1", call: RenderToolCall(tag: "readFile", fields: ["path": "edge/wrangler.jsonc"]),
                            isError: false, resolved: true),
                        .tool(
                            id: "s1/2",
                            call: RenderToolCall(
                                tag: "mcp",
                                fields: [
                                    "server": "cloudflare-docs", "tool": "search_cloudflare_documentation",
                                ]),
                            isError: false, resolved: true),
                        .text(
                            id: "t0",
                            text:
                                "Flush timer now only arms while dirty; ping/pong uses the auto-response path so the DO never wakes for keepalives."
                        ),
                    ], createdAt: now - 86_400_000, deviceId: "dev-vps", status: .complete, continuationOf: nil),
            ]
        case "chat-math":
            return [
                MessageEntry(
                    id: "m1", role: .user,
                    parts: [.text(id: "t0", text: "Show me some formulas.")],
                    createdAt: now - 2_100_000, deviceId: "ios-demo", status: .complete, continuationOf: nil),
                MessageEntry(
                    id: "m2", role: .assistant, parts: [.text(id: "t0", text: mathShowcase)],
                    createdAt: now - 2_000_000, deviceId: "dev-mac", status: .complete, continuationOf: nil),
            ]
        case "chat-permissions-story":
            return permissionStory(now: now)
        case "chat-permissions-card":
            return permissionCard(now: now)
        default:
            // The context recovery showcase, or a freshly minted chat (empty).
            return recoveryTranscript(for: chatId, now: now) ?? []
        }
    }

    /// The reply of the mock harness's [mock:math] showcase
    /// (crates/harness/src/mock/math.rs): every delimiter, prices and code
    /// that keep their dollars, and TeX that does not typeset.
    private static let mathShowcase = #"""
        Here is the quadratic formula. For $ax^2 + bx + c = 0$ with $a \neq 0$, the roots are

        $$
        x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
        $$

        and the discriminant \(\Delta = b^2 - 4ac\) decides how many are real: two when $\Delta > 0$, one when $\Delta = 0$.

        **Euler's identity** links the five constants $e$, $i$, $\pi$, $1$ and $0$:
        \[
        e^{i\pi} + 1 = 0
        \]

        A few more shapes, inline: $\sum_{k=1}^{n} k = \frac{n(n+1)}{2}$, $\int_0^1 x^2\,dx = \tfrac{1}{3}$, $\lim_{x \to 0} \frac{\sin x}{x} = 1$, and a matrix:

        $$
        A = \begin{pmatrix} \cos\theta & -\sin\theta \\ \sin\theta & \cos\theta \end{pmatrix}, \qquad \det A = 1
        $$

        | Quantity | Formula |
        | --- | --- |
        | Gaussian | $\frac{1}{\sigma\sqrt{2\pi}} e^{-\frac{(x-\mu)^2}{2\sigma^2}}$ |
        | Big-O | $O(n \log n)$ |

        - Prices stay prose: it costs $5 today and $10 tomorrow.
        - Code keeps its dollars: `echo $HOME` and `\(not math\)`.
        - TeX that does not typeset shows as code: $\undefinedmacro{x}$.
        """#

    // MARK: Permission control (the mock harness's [mock:permissions] story)

    private static func command(
        _ id: String, _ command: String, _ outcome: ToolPermission.Outcome?,
        scope: ToolPermission.Scope? = nil, rule: String? = nil, purpose: String? = nil
    ) -> MessagePart {
        .tool(
            id: id,
            call: RenderToolCall(
                tag: "exec", fields: ["command": command],
                permission: outcome.map { ToolPermission(outcome: $0, scope: scope, rule: rule, purpose: purpose) }),
            isError: outcome == .blocked || outcome == .denied, resolved: true)
    }

    private static func prompt(
        _ id: String, _ title: String, options: [String], kind: String?, call: String?, resolved: Bool = true
    ) -> MessagePart {
        .input(
            id: "in-\(id)", requestId: id,
            questions: [
                UserInputQuestion(
                    id: id, header: title, question: title, options: options,
                    permission: kind.map { PermissionPrompt(kind: $0, toolCallId: call) })
            ],
            resolved: resolved)
    }

    private static let escalateOptions = ["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]
    private static let accessOptions = ["Allow once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]
    private static let sshStatus = "ssh deploy@web 'systemctl status app --no-pager'"
    private static let push = "git push --force origin release/0.3.49"

    private static func escalateTitle(_ command: String, purpose: String, pattern: String, reason: String) -> String {
        """
        Run this command outside the sandbox?

        Context:
        Purpose: \(purpose)
        Command: \(command)
        Pattern: \(pattern)
        Reason: \(reason)
        Note: It runs as you, with your files, keys, keychain and network.
        """
    }

    private static func permissionStory(now: Int64) -> [MessageEntry] {
        let purpose = "Check the app on the server web"
        return [
            MessageEntry(
                id: "p1", role: .user,
                parts: [.text(id: "t0", text: "Restart the app on web, then publish the release.")],
                createdAt: now - 900_000, deviceId: "ios-demo", status: .complete),
            MessageEntry(
                id: "p2", role: .assistant,
                parts: [
                    .text(id: "t0", text: "I'll check the app on **web** before restarting it."),
                    command("perm-1", sshStatus, .blocked),
                    command("perm-2", sshStatus, .outside, scope: .chat, purpose: purpose),
                    prompt(
                        "q-2",
                        escalateTitle(
                            sshStatus, purpose: purpose, pattern: "ssh deploy@web …",
                            reason: "The sandbox hides the SSH key; the task needs the server."),
                        options: escalateOptions, kind: "escalate", call: "perm-2"),
                    command(
                        "perm-3", "ssh deploy@web 'sudo systemctl restart app'", .outside, scope: .rule,
                        rule: "ssh deploy@web …", purpose: purpose),
                    .tool(
                        id: "perm-4",
                        call: RenderToolCall(
                            tag: "editFile", fields: ["path": "../shared/deploy-notes.md"],
                            permission: ToolPermission(outcome: .allowed, scope: .chat)),
                        isError: false, resolved: true),
                    prompt(
                        "q-4",
                        "Allow the agent to write outside the workspace?\n\nContext:\nPath: ~/Developer/shared/deploy-notes.md\nTool: edit",
                        options: accessOptions, kind: "access", call: "perm-4"),
                    .text(id: "t5", text: "The app is back up. Then the release steps:"),
                    command("perm-5", "cat .env", .blocked),
                    command("perm-6", push, .blocked),
                    command("perm-7", push, .denied, purpose: "Publish the release branch"),
                    prompt(
                        "q-7",
                        escalateTitle(
                            push, purpose: "Publish the release branch", pattern: "git push …",
                            reason: "Pushing needs the keychain's GitHub credentials."),
                        options: escalateOptions, kind: "escalate", call: "perm-7"),
                    .tool(
                        id: "perm-8",
                        call: RenderToolCall(tag: "mcp", fields: ["server": "github", "tool": "create_issue"]),
                        isError: false, resolved: true),
                    prompt(
                        "q-8",
                        "Allow the agent to use mcp__github__create_issue?\n\nContext:\nTool: mcp__github__create_issue",
                        options: accessOptions, kind: "tool", call: "perm-8"),
                    // A prompt from a Runtime that does not mark its prompts.
                    prompt(
                        "q-old",
                        "Allow the agent to reach registry.example.com?\n\nContext:\nDomain: registry.example.com",
                        options: [
                            "Allow for this command", "Allow for this chat", "Always allow", "Deny", "Deny and stop",
                        ],
                        kind: nil, call: nil),
                    // An ordinary question, for contrast.
                    prompt(
                        "q-question", "Which environment should the next deploy go to?",
                        options: ["staging", "production"], kind: nil, call: nil),
                    .text(
                        id: "t9",
                        text:
                            "Done: the app on **web** is running again and the notes are updated. You declined the force-push, so `release/0.3.49` is still local, and I left `.env` alone; I opened issue #42 to track publishing."
                    ),
                ],
                createdAt: now - 880_000, deviceId: "dev-mac", status: .complete),
        ]
    }

    private static func permissionCard(now: Int64) -> [MessageEntry] {
        let purpose = "Check the app on the server web"
        return [
            MessageEntry(
                id: "c1", role: .user,
                parts: [.text(id: "t0", text: "Is the app on web healthy?")],
                createdAt: now - 70_000, deviceId: "ios-demo", status: .complete),
            MessageEntry(
                id: "c2", role: .assistant,
                parts: [
                    .text(id: "t0", text: "Let me check the app on **web**."),
                    command("card-1", sshStatus, .blocked),
                    .tool(
                        id: "card-2", call: RenderToolCall(tag: "exec", fields: ["command": sshStatus]),
                        isError: false, resolved: false),
                    prompt(
                        "q-card",
                        escalateTitle(
                            sshStatus, purpose: purpose, pattern: "ssh deploy@web …",
                            reason: "The sandbox hides the SSH key; the task needs the server."),
                        options: escalateOptions, kind: "escalate", call: "card-2", resolved: false),
                ],
                createdAt: now - 65_000, deviceId: "dev-mac", status: .streaming),
        ]
    }

    // MARK: Streaming simulation

    private func simulateTurn(store: SessionStore, chatId: String, prompt: String, isSteer: Bool) {
        streamTask?.cancel()
        let now = nowMs()
        var entries = store.fedEntries
        entries.append(
            MessageEntry(
                id: "u-\(now)", role: .user,
                parts: [
                    .text(id: "t0", text: prompt)
                ], createdAt: now, deviceId: "ios-demo", status: .complete, continuationOf: nil, isSteer: isSteer))
        let liveId = "a-\(now)"
        entries.append(
            MessageEntry(
                id: liveId, role: .assistant,
                parts: [
                    .text(id: "t0", text: "")
                ], createdAt: now, deviceId: "dev-mac", status: .streaming, continuationOf: nil))
        store.setEntries(entries)
        sessions[chatId] = SessionRow(
            chatId: chatId, deviceId: "dev-mac", status: .working,
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

        // A script and the call it makes run first, so the live chips' status
        // icons turn from the spinner to a check before the reply streams.
        let script = RenderToolCall(
            tag: "unknown",
            fields: [
                "name": "codemode", "code": "return await tools.read({ path: \"crates/ui/src/markdown/veil.rs\" });",
            ])
        let read = RenderToolCall(tag: "readFile", fields: ["path": "crates/ui/src/markdown/veil.rs"])
        let toolPhases: [[MessagePart]] = [
            [.tool(id: "s1", call: script, isError: false, resolved: false)],
            [
                .tool(id: "s1", call: script, isError: false, resolved: false),
                .tool(id: "s1/1", call: read, isError: false, resolved: false),
            ],
            [
                .tool(id: "s1", call: script, isError: false, resolved: false),
                .tool(id: "s1/1", call: read, isError: false, resolved: true),
            ],
            [
                .tool(id: "s1", call: script, isError: false, resolved: true),
                .tool(id: "s1/1", call: read, isError: false, resolved: true),
            ],
        ]

        streamTask = Task { [weak self, weak store] in
            for parts in toolPhases {
                if Task.isCancelled { return }
                guard let store else { return }
                var current = store.fedEntries
                guard let last = current.indices.last, current[last].id == liveId else { return }
                current[last].parts = parts
                store.setEntries(current)
                try? await Task.sleep(nanoseconds: 600_000_000)
            }
            let tools = toolPhases.last ?? []
            var text = ""
            for (ix, word) in words.enumerated() {
                if Task.isCancelled { return }
                text += (ix == 0 ? "" : " ") + word
                guard let store else { return }
                var current = store.fedEntries
                guard let last = current.indices.last, current[last].id == liveId else { return }
                current[last].parts = tools + [.text(id: "t0", text: text)]
                store.setEntries(current)
                try? await Task.sleep(nanoseconds: UInt64.random(in: 30_000_000...140_000_000))
            }
            guard let self, let store else { return }
            var current = store.fedEntries
            if let last = current.indices.last, current[last].id == liveId {
                current[last].status = .complete
                store.setEntries(current)
            }
            let end = nowMs()
            self.sessions[chatId] = SessionRow(
                chatId: chatId, deviceId: "dev-mac", status: .idle,
                startedAt: nil, updatedAt: end)
            if let ix = self.chats.firstIndex(where: { $0.id == chatId }) {
                self.chats[ix].lastMessageAt = end
                self.chats[ix].lastMessagePreview = "When the turn settles, this entry flips…"
                self.chats[ix].lastSeenAt = end
            }
        }
    }
}

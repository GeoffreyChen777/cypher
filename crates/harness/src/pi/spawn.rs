//! Building and spawning the pi child: CLI resolution, launch flags, PATH,
//! the bundled engine client, and the Cypher bridge / child-subagent env.

use super::*;

/// Env vars the subagents extension keys on (mirrors
/// `extensions/subagents/message.ts`): a child pi process loads the extension
/// in child mode and registers the generic messaging tools.
const ENV_ROLE: &str = "PI_SUBAGENT_ROLE";
const ROLE_CHILD: &str = "child";
const ENV_CHANNEL_ROOT: &str = "PI_SUBAGENT_CHANNEL_ROOT";
const ENV_RUN_ID: &str = "PI_SUBAGENT_RUN_ID";
const ENV_AGENT: &str = "PI_SUBAGENT_AGENT";
const ENV_CHILD_INDEX: &str = "PI_SUBAGENT_CHILD_INDEX";
/// The chat id this pi process belongs to (parent or child) — injected by the
/// harness as `CYPHER_CHAT_ID`, consumed by the subagents extension for the
/// Cypher bridge.
const ENV_CYPHER_CHAT_ID: &str = "CYPHER_CHAT_ID";
/// The task the run belongs to ([`RunHostContext::task_id`]), read by the
/// permission extension's ledger (`extensions/cypher-permissions/ledger.ts`):
/// chats of one task are never each other's "other task". Set or cleared on
/// every spawn, like the chat id.
const ENV_CYPHER_TASK_ID: &str = "CYPHER_TASK_ID";
/// Local engine IPC socket path the extension's Cypher bridge helper dials
/// (`StartSubagent` / `WatchAgentEvents`).
const ENV_CYPHER_ENGINE_SOCKET: &str = "CYPHER_ENGINE_SOCKET";
/// Bridge protocol the engine speaks, set with the socket: the runtime's
/// subagents patch hosts child runs as Cypher child chats only when it reads a
/// version it knows, so an older engine (which truncates the task to the
/// 500-char label) keeps the extension's own child processes. `2` = the
/// `StartSubagent` `prompt` + `address` fields and a lag-tolerant,
/// de-duplicated `WatchAgentEvents`.
const ENV_CYPHER_SUBAGENT_BRIDGE: &str = "CYPHER_SUBAGENT_BRIDGE";
const SUBAGENT_BRIDGE_VERSION: &str = "2";
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// The chat's sandbox setting, read by the Runtime's permission extension
/// (`extensions/cypher-permissions.ts`): `read-only`, `workspace-write` or
/// `danger-full-access`. Absent for discovery and slash-command processes,
/// which then use the extension's configured default.
const ENV_PERMISSION_LEVEL: &str = "CYPHER_PERMISSION_LEVEL";
/// The owner's own words ([`RunHostContext::owner_words`]), read by the
/// same extension (`extensions/cypher-permissions/explicit.ts`): the only
/// text a dangerous command may run on without a dialog. Set or cleared on
/// every spawn, so a run never inherits another chat's (or the engine's).
const ENV_OWNER_WORDS: &str = "CYPHER_OWNER_WORDS";
/// GPT Fast mode a run asks for (`on`/`off`; cypher-fast-mode.ts), from its
/// request's `modelOptions.fast`. Set or cleared on every spawn.
const ENV_FAST_MODE: &str = "CYPHER_FAST_MODE";
const ENV_PACKAGE_DIR: &str = "PI_PACKAGE_DIR";
/// Set only in agent-bot runs; the bot tools extension registers its tools
/// only when it sees this.
const ENV_CYPHER_BOT_ID: &str = "CYPHER_BOT_ID";
/// A bot run's workspace roots (a JSON array of absolute paths): the bot
/// tools extension records them as the permission extension's extra roots
/// for this chat before that extension loads them, and removes stale ones.
const ENV_CYPHER_BOT_WORKSPACE_ROOTS: &str = "CYPHER_BOT_WORKSPACE_ROOTS";
/// The bot tools extension, written beside the engine client.
const BOT_TOOLS_EXTENSION: &str = "cypher-bot-tools.ts";
/// The memory tools extension (`memory.ts`), loaded into every run; it
/// registers its tools only when this is set (the engine gave the run
/// memory).
const ENV_CYPHER_MEMORY: &str = "CYPHER_MEMORY";
const MEMORY_EXTENSION: &str = "cypher-memory.ts";

/// The messaging tools every child gets regardless of its allowlist (the
/// extension's `spawn.ts` appends the same trio).
const MESSAGING_TOOLS: [&str; 3] = ["send_message", "read_inbox", "reply_message"];

/// The file tools whose approvals go through `request_permission` (the
/// permission extension's `OWN_TOOLS`, less `bash`, which escalates instead).
const FILE_TOOLS: [&str; 6] = ["read", "write", "edit", "ls", "grep", "find"];
/// Every file-tool denial tells the agent to call this, so it comes with any
/// file tool: Pi drops a tool the allowlist does not name even when an
/// extension registers it. With permission control off nothing registers it
/// and Pi ignores the name.
const PERMISSION_TOOL: &str = "request_permission";

/// `tools` plus `request_permission` when it names a file tool.
fn with_permission_tool(mut tools: Vec<String>) -> Vec<String> {
    if tools.iter().any(|t| FILE_TOOLS.contains(&t.as_str()))
        && !tools.iter().any(|t| t == PERMISSION_TOOL)
    {
        tools.push(PERMISSION_TOOL.to_string());
    }
    tools
}

/// Whether an allowlist names a tool that reaches MCP tools. Pi offers MCP
/// tools only through `codemode` or `tool_search`; without either its MCP
/// extension still connects every server and warns that their tools cannot
/// be called, so such a run starts with `--no-mcp` instead.
fn reaches_mcp(tools: &[String]) -> bool {
    tools.iter().any(|tool| {
        tool == cypher_proto::view::CODEMODE_TOOL || tool == cypher_proto::view::TOOL_SEARCH_TOOL
    })
}

/// Every env var a [`RunProfile`] sets; each spawn clears them all first.
const PROFILE_ENV: [&str; 8] = [
    ENV_CYPHER_MEMORY,
    ENV_ROLE,
    ENV_CHANNEL_ROOT,
    ENV_RUN_ID,
    ENV_AGENT,
    ENV_CHILD_INDEX,
    ENV_CYPHER_BOT_ID,
    ENV_CYPHER_BOT_WORKSPACE_ROOTS,
];

/// The CLI args and env one run profile adds to the pi command.
struct ProfileLaunch {
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
}

/// The one place a [`RunProfile`] becomes pi flags and env: the tool
/// allowlist, extra extensions and the appended prompt. `append_prompt` is
/// the temp file holding [`RunProfile::appended_prompt`].
fn profile_launch(
    host: &RunHostContext,
    append_prompt: Option<&PathBuf>,
) -> std::io::Result<ProfileLaunch> {
    let profile = &host.profile;
    let memory = host.memory_prompt.is_some();
    let mut args = Vec::new();
    let mut env = Vec::new();
    if memory {
        env.push((ENV_CYPHER_MEMORY, "1".to_string()));
    }
    match profile {
        RunProfile::Root => {}
        // Child-subagent semantics (Cypher-hosted child chats): the persisted
        // agent allowlist plus the messaging tools (and `request_permission`
        // beside file tools), and the child-mode env the subagents extension
        // keys on.
        RunProfile::Child(child) => {
            let mut tools = child.tools.clone();
            let extra = MESSAGING_TOOLS.iter().chain(
                cypher_proto::MEMORY_TOOLS
                    .iter()
                    .filter(|_| memory && !child.tools.is_empty()),
            );
            for tool in extra {
                if !tools.iter().any(|t| t == tool) {
                    tools.push(tool.to_string());
                }
            }
            let tools = with_permission_tool(tools);
            if !tools.is_empty() {
                args.push("--tools".into());
                args.push(tools.join(","));
            }
            env.push((ENV_ROLE, ROLE_CHILD.to_string()));
            // The messaging channel is host-local and only exists for the
            // initial run; later child turns have no channel and the child's
            // messaging tools honestly report "unavailable".
            if let Some(channel_root) = &child.channel_root {
                env.push((ENV_CHANNEL_ROOT, channel_root.clone()));
            }
            env.push((ENV_RUN_ID, child.run_id.clone()));
            env.push((ENV_AGENT, child.agent.clone()));
            env.push((ENV_CHILD_INDEX, child.child_index.to_string()));
        }
        // Bot runs see only their allowlist, even an empty one: an absent
        // `--tools` would hand them every tool. No shell or file access
        // unless the engine lists them (then `request_permission` beside
        // them), no subagents, and MCP only through codemode or tool_search
        // when the engine lists one of them (the owner's own chat). The
        // engine enforces the bot's scope inside each bot tool.
        RunProfile::Bot(bot) => {
            args.push("--tools".into());
            args.push(with_permission_tool(bot.tools.clone()).join(","));
            if !reaches_mcp(&bot.tools) {
                args.push("--no-mcp".into());
            }
            args.push("-e".into());
            args.push(
                private_support_dir()?
                    .join(BOT_TOOLS_EXTENSION)
                    .display()
                    .to_string(),
            );
            env.push((ENV_CYPHER_BOT_ID, bot.bot_id.clone()));
            // Always set, even empty: the extension then drops roots an
            // earlier run of this chat recorded.
            env.push((
                ENV_CYPHER_BOT_WORKSPACE_ROOTS,
                serde_json::to_string(&bot.workspace_roots).unwrap_or_else(|_| "[]".to_string()),
            ));
        }
        // A pass sees only its allowlist (the memory tools), like a bot.
        RunProfile::Memory(pass) => {
            args.push("--tools".into());
            args.push(pass.tools.join(","));
            if !reaches_mcp(&pass.tools) {
                args.push("--no-mcp".into());
            }
        }
    }
    if let Some(path) = append_prompt
        && host.appended_prompt().is_some()
    {
        args.push("--append-system-prompt".into());
        args.push(path.display().to_string());
    }
    Ok(ProfileLaunch { args, env })
}

/// Write the child agent's persisted system prompt to a 0600 temp file
/// (`--append-system-prompt` takes a path) and return it for later cleanup.
pub(super) fn write_temp_prompt(agent: &str, prompt: &str) -> std::io::Result<PathBuf> {
    let safe = agent.replace(
        |c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_',
        "_",
    );
    let dir = std::env::temp_dir().join(format!("pi-subagent-{safe}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("prompt.md");
    std::fs::write(&path, prompt)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// The extension that tells the model what the chat renders
/// (`chat-rendering.ts`), loaded into every Pi with `-e`.
const CHAT_RENDERING_EXTENSION: &str = "cypher-chat-rendering.ts";
/// The `/explain` command and its live session explainer (`explain.ts`),
/// loaded beside it.
const EXPLAIN_EXTENSION: &str = "cypher-explain.ts";

/// Private files Cypher-spawned Pi processes load: the engine client module
/// the bundled extensions import, the app's own extensions, and the bot
/// tools extension bot runs load.
fn private_support_dir() -> std::io::Result<&'static std::path::Path> {
    static DIRECTORY: std::sync::OnceLock<Result<tempfile::TempDir, String>> =
        std::sync::OnceLock::new();
    let directory = DIRECTORY.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("cypher-private-support-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        for (name, contents) in [
            ("engine-client.mjs", include_str!("engine-client.mjs")),
            (BOT_TOOLS_EXTENSION, include_str!("bot-tools.ts")),
            (CHAT_RENDERING_EXTENSION, include_str!("chat-rendering.ts")),
            (EXPLAIN_EXTENSION, include_str!("explain.ts")),
            (MEMORY_EXTENSION, include_str!("memory.ts")),
        ] {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.path().join(name))
                .map_err(|e| e.to_string())?;
            file.write_all(contents.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        Ok(dir)
    });
    directory
        .as_ref()
        .map(|dir| dir.path())
        .map_err(|e| std::io::Error::other(e.clone()))
}

fn inject_engine_client(cmd: &mut Command) -> std::io::Result<()> {
    let directory = private_support_dir()?;
    cmd.env(
        "CYPHER_ENGINE_CLIENT_MODULE",
        directory.join("engine-client.mjs"),
    );
    Ok(())
}

/// The wire name of a sandbox level (`SandboxLevel`'s serde form).
/// What a process is launched with beyond its host context: the requested
/// model and thinking level, the permission level and GPT Fast mode.
/// Discovery processes launch with none.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Launch<'a> {
    pub model: Option<&'a str>,
    pub thinking: Option<&'a str>,
    pub sandbox: Option<SandboxLevel>,
    pub fast: Option<bool>,
}

impl<'a> Launch<'a> {
    /// A run's launch, from its request.
    pub fn of(request: &'a RunRequest) -> Self {
        Self {
            model: request.model.as_deref(),
            thinking: request.reasoning.map(thinking_level),
            sandbox: Some(request.sandbox),
            fast: request
                .model_options
                .get(cypher_proto::FAST_MODEL_OPTION)
                .and_then(serde_json::Value::as_bool),
        }
    }
}

fn permission_level(level: SandboxLevel) -> &'static str {
    level.wire()
}

/// Where a chat's live permission level is kept for its running Pi
/// (`extensions/cypher-permissions/level.ts` `levelFile`): under the agent
/// directory's `cypher-permissions/`, which is the permission system's own
/// (config.ts `permissionSystemPatterns`): outside every sandbox's writable
/// roots, and on the floor for a file tool or a command that would write
/// it. The extension re-reads it before each decision, and takes a level
/// above the one it runs at only once the engine confirms it
/// (`PermissionLevel` on the agent socket), so a forged file can lower a
/// run's level but never raise it.
pub fn level_file(agent_dir: &std::path::Path, chat_id: &str) -> PathBuf {
    let safe: String = chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    agent_dir
        .join("cypher-permissions")
        .join("levels")
        .join(format!("{safe}.json"))
}

/// Whether the Runtime whose Pi package is `package_dir` (`<runtime>/pi`)
/// ships a permission extension that follows [`level_file`]: its
/// `extensions/cypher-permissions/level.ts`. An older Runtime's never reads
/// the file.
pub fn follows_live_levels(package_dir: &std::path::Path) -> bool {
    package_dir.parent().is_some_and(|runtime| {
        runtime
            .join("extensions")
            .join("cypher-permissions")
            .join("level.ts")
            .is_file()
    })
}

/// Write `level` as `chat_id`'s live level ([`level_file`]), whole or not
/// at all (a temporary file renamed over it).
pub fn write_level_file(
    agent_dir: &std::path::Path,
    chat_id: &str,
    level: SandboxLevel,
) -> std::io::Result<()> {
    let path = level_file(agent_dir, chat_id);
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("level file has no folder"))?;
    std::fs::create_dir_all(dir)?;
    let body = serde_json::json!({
        "level": level.wire(),
        "at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
    });
    let temporary = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("level"),
        std::process::id()
    ));
    std::fs::write(&temporary, format!("{body}\n"))?;
    std::fs::rename(&temporary, &path)
}

/// Resolve the pi CLI: `PI_EXECUTABLE` override, then the shared CLI resolver
/// (PATH + login-shell PATH + npm-global bins + node-version-manager bins).
pub(super) fn resolve_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PI_EXECUTABLE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::resolve_cli("pi")
}

impl PiHarness {
    pub(super) fn resolve_program(&self) -> Result<(PathBuf, Vec<String>), HarnessError> {
        let args = vec![
            "--mode".into(),
            "rpc".into(),
            "--session-dir".into(),
            self.session_dir.display().to_string(),
        ];
        if let Some(exe) = &self.executable {
            if exe.is_file() {
                return Ok((exe.clone(), args));
            }
            return Err(HarnessError::NotInstalled(format!(
                "Cypher Pi Runtime is not installed ({})",
                exe.display()
            )));
        }
        match resolve_executable() {
            Some(exe) => Ok((exe, args)),
            None => Err(HarnessError::NotInstalled(
                "pi (searched PATH, the login shell's PATH, npm global bins, and \
                 fnm/nvm/volta/pnpm/bun install dirs; install with \
                 `npm install -g @earendil-works/pi-coding-agent`; set \
                 PI_EXECUTABLE to override)"
                    .into(),
            )),
        }
    }

    /// Test seam: the `std::process::Command` a run would spawn — CLI args,
    /// PATH composition, cwd, and the bridge env — without spawning a child.
    /// Lets tests assert `CYPHER_ENGINE_SOCKET` (and child-env) injection
    /// deterministically.
    #[doc(hidden)]
    pub fn spawn_command(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
    ) -> Result<Command, HarnessError> {
        self.spawn_command_with_config(cwd, host, append_prompt, Launch::default())
    }

    /// Test seam for the exact command used by a normal run, including the
    /// requested model and thinking level. Starting Pi on the selected model
    /// avoids briefly initializing its persisted/default model and, more
    /// importantly, avoids racing RPC `set_model` against Pi's asynchronously
    /// populated model-catalog snapshot.
    #[doc(hidden)]
    pub fn spawn_run_command(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        request: &RunRequest,
    ) -> Result<Command, HarnessError> {
        self.spawn_command_with_config(cwd, host, append_prompt, Launch::of(request))
    }

    fn spawn_command_with_config(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        launch: Launch<'_>,
    ) -> Result<Command, HarnessError> {
        let Launch {
            model: requested_model,
            thinking: requested_thinking,
            sandbox,
            fast,
        } = launch;
        let (exe, mut args) = self.resolve_program()?;
        for extension in [
            CHAT_RENDERING_EXTENSION,
            EXPLAIN_EXTENSION,
            MEMORY_EXTENSION,
        ] {
            args.push("--extension".into());
            args.push(private_support_dir()?.join(extension).display().to_string());
        }
        let profile = profile_launch(host, append_prompt)?;
        args.extend(profile.args);
        // A child profile is authoritative for model/thinking when it
        // supplies either launch value; otherwise the RunRequest value is
        // used, just like a root chat.
        let child = host.profile.child();
        let launch_model = child
            .and_then(|child| child.model.as_deref())
            .or(requested_model)
            .filter(|model| concrete_model(model));
        let launch_thinking = launch_model.and_then(|_| {
            child
                .and_then(|child| child.thinking.as_deref())
                .or(requested_thinking)
        });
        if let Some(model) = launch_model {
            args.push("--model".into());
            args.push(model.into());
        }
        if let Some(thinking) = launch_thinking {
            args.push("--thinking".into());
            args.push(thinking.into());
        }
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        crate::compose_child_path(&mut cmd, &exe);
        if let Some(agent_dir) = &self.agent_dir {
            cmd.env(ENV_AGENT_DIR, agent_dir);
        }
        if let Some(package_dir) = &self.package_dir {
            cmd.env(ENV_PACKAGE_DIR, package_dir);
        }
        inject_engine_client(&mut cmd)?;
        cmd.env_remove(ENV_PERMISSION_LEVEL);
        if let Some(level) = sandbox {
            cmd.env(ENV_PERMISSION_LEVEL, permission_level(level));
        }
        cmd.env_remove(ENV_FAST_MODE);
        if let Some(fast) = fast {
            cmd.env(ENV_FAST_MODE, if fast { "on" } else { "off" });
        }
        cmd.env_remove(ENV_OWNER_WORDS);
        if let Some(words) = host.owner_words.as_deref().filter(|w| !w.is_empty()) {
            cmd.env(ENV_OWNER_WORDS, words);
        }
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        // Cypher bridge identity: the chat this run belongs to plus the local
        // engine IPC socket path (so the extension can StartSubagent +
        // WatchAgentEvents). Discovery processes pass an empty host context
        // and therefore never receive a parent chat id.
        cmd.env_remove(ENV_CYPHER_CHAT_ID);
        cmd.env_remove(ENV_CYPHER_TASK_ID);
        cmd.env_remove(ENV_CYPHER_ENGINE_SOCKET);
        cmd.env_remove(ENV_CYPHER_SUBAGENT_BRIDGE);
        cmd.env_remove("CYPHER_ENGINE_WS_URL");
        if let Some(chat_id) = host.chat_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_CHAT_ID, chat_id);
        }
        if let Some(task_id) = host.task_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_TASK_ID, task_id);
        }
        if let Some(url) = self.engine_socket.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_ENGINE_SOCKET, url);
            cmd.env(ENV_CYPHER_SUBAGENT_BRIDGE, SUBAGENT_BRIDGE_VERSION);
        }
        // Profile env: cleared first, so no run inherits another role's.
        for key in PROFILE_ENV {
            cmd.env_remove(key);
        }
        for (key, value) in profile.env {
            cmd.env(key, value);
        }
        Ok(cmd)
    }

    pub(super) async fn spawn_child(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        launch: Launch<'_>,
    ) -> Result<(Child, crate::process::StderrTail), HarnessError> {
        let mut cmd = self.spawn_command_with_config(cwd, host, append_prompt, launch)?;
        // The live level starts as the one it launches with: a file a
        // previous run of the chat left says nothing about this one.
        if let (Some(agent_dir), Some(chat_id), Some(level)) = (
            self.agent_dir.as_deref(),
            host.chat_id.as_deref().filter(|id| !id.is_empty()),
            launch.sandbox,
        ) && let Err(err) = write_level_file(agent_dir, chat_id, level)
        {
            tracing::warn!(chat = %chat_id, error = %err, "could not write the live level file");
        }
        let exe = cmd.as_std().get_program().to_string_lossy().into_owned();
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.clone())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let stderr_tail = crate::process::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(%line, "pi stderr");
                    tail.push(&line);
                }
            });
        }
        Ok((child, stderr_tail))
    }
}

#[cfg(test)]
mod level_tests {
    use super::*;

    #[test]
    fn a_live_level_is_written_where_the_extension_reads_it() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("agent");
        // The extension's `levelFile`: the same folder and the same name.
        assert_eq!(
            level_file(&agent, "chat/../x y"),
            agent.join("cypher-permissions/levels/chat_.._x_y.json")
        );
        let runtime = root.path().join("runtime");
        let harness = PiHarness::new(root.path().join("sessions"))
            .with_runtime_environment(&agent, runtime.join("pi"));
        // A Runtime whose extension doesn't follow the file: nothing to tell.
        assert!(!harness.apply_level("c-1", SandboxLevel::DangerFullAccess));
        assert!(!level_file(&agent, "c-1").exists());
        std::fs::create_dir_all(runtime.join("extensions/cypher-permissions")).unwrap();
        std::fs::write(runtime.join("extensions/cypher-permissions/level.ts"), "").unwrap();
        assert!(harness.apply_level("c-1", SandboxLevel::DangerFullAccess));
        let read = |chat: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(level_file(&agent, chat)).unwrap()).unwrap()
        };
        assert_eq!(read("c-1")["level"], "danger-full-access");
        assert!(harness.apply_level("c-1", SandboxLevel::WorkspaceWrite));
        assert_eq!(read("c-1")["level"], "workspace-write");
        // Nothing but the file is left in its folder.
        let names: Vec<_> = std::fs::read_dir(agent.join("cypher-permissions/levels"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["c-1.json"]);
        // No agent directory of its own: no file it would read.
        assert!(!PiHarness::new(root.path().join("s2")).apply_level("c-1", SandboxLevel::ReadOnly));
    }
}

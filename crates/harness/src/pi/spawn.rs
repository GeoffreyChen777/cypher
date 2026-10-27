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
const ENV_PACKAGE_DIR: &str = "PI_PACKAGE_DIR";

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

/// Private files every Cypher-spawned Pi loads: the engine client module the
/// bundled extensions import, and the app's own extensions.
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
            (CHAT_RENDERING_EXTENSION, include_str!("chat-rendering.ts")),
            (EXPLAIN_EXTENSION, include_str!("explain.ts")),
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
fn permission_level(level: SandboxLevel) -> &'static str {
    match level {
        SandboxLevel::ReadOnly => "read-only",
        SandboxLevel::WorkspaceWrite => "workspace-write",
        SandboxLevel::DangerFullAccess => "danger-full-access",
    }
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
        self.spawn_command_with_config(cwd, host, append_prompt, None, None, None)
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
        self.spawn_command_with_config(
            cwd,
            host,
            append_prompt,
            request.model.as_deref(),
            request.reasoning.map(thinking_level),
            Some(request.sandbox),
        )
    }

    fn spawn_command_with_config(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        requested_model: Option<&str>,
        requested_thinking: Option<&str>,
        sandbox: Option<SandboxLevel>,
    ) -> Result<Command, HarnessError> {
        let (exe, mut args) = self.resolve_program()?;
        for extension in [CHAT_RENDERING_EXTENSION, EXPLAIN_EXTENSION] {
            args.push("--extension".into());
            args.push(private_support_dir()?.join(extension).display().to_string());
        }
        // Child-subagent semantics (Cypher-hosted child chats): restrict tools
        // to the persisted agent allowlist plus the messaging tools (and
        // `request_permission` beside file tools), append
        // the persisted system prompt, preserve model/thinking. The child
        // profile is authoritative when it supplies either launch value;
        // otherwise the RunRequest value is used, just like a root chat.
        let launch_model = host
            .child
            .as_ref()
            .and_then(|child| child.model.as_deref())
            .or(requested_model)
            .filter(|model| concrete_model(model));
        let launch_thinking = launch_model.and_then(|_| {
            host.child
                .as_ref()
                .and_then(|child| child.thinking.as_deref())
                .or(requested_thinking)
        });
        if let Some(child) = &host.child {
            let mut tools = child.tools.clone();
            let file_tools = tools.iter().any(|t| FILE_TOOLS.contains(&t.as_str()));
            for tool in MESSAGING_TOOLS
                .into_iter()
                .chain(file_tools.then_some(PERMISSION_TOOL))
            {
                if !tools.iter().any(|t| t == tool) {
                    tools.push(tool.to_string());
                }
            }
            if !tools.is_empty() {
                args.push("--tools".into());
                args.push(tools.join(","));
            }
            if let Some(path) = append_prompt
                && !child.system_prompt.trim().is_empty()
            {
                args.push("--append-system-prompt".into());
                args.push(path.display().to_string());
            }
        }
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
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        // Cypher bridge identity: the chat this run belongs to plus the local
        // engine IPC socket path (so the extension can StartSubagent +
        // WatchAgentEvents). Discovery processes pass an empty host context
        // and therefore never receive a parent chat id.
        cmd.env_remove(ENV_CYPHER_CHAT_ID);
        cmd.env_remove(ENV_CYPHER_ENGINE_SOCKET);
        cmd.env_remove(ENV_CYPHER_SUBAGENT_BRIDGE);
        cmd.env_remove("CYPHER_ENGINE_WS_URL");
        if let Some(chat_id) = host.chat_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_CHAT_ID, chat_id);
        }
        if let Some(url) = self.engine_socket.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_ENGINE_SOCKET, url);
            cmd.env(ENV_CYPHER_SUBAGENT_BRIDGE, SUBAGENT_BRIDGE_VERSION);
        }
        if let Some(child) = &host.child {
            cmd.env(ENV_ROLE, ROLE_CHILD);
            // The messaging channel is host-local and only exists for the
            // initial run; later child turns have no channel and the child's
            // messaging tools honestly report "unavailable".
            if let Some(channel_root) = &child.channel_root {
                cmd.env(ENV_CHANNEL_ROOT, channel_root);
            }
            cmd.env(ENV_RUN_ID, &child.run_id);
            cmd.env(ENV_AGENT, &child.agent);
            cmd.env(ENV_CHILD_INDEX, child.child_index.to_string());
        }
        Ok(cmd)
    }

    pub(super) async fn spawn_child(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        requested_model: Option<&str>,
        requested_thinking: Option<&str>,
        sandbox: Option<SandboxLevel>,
    ) -> Result<(Child, crate::process::StderrTail), HarnessError> {
        let mut cmd = self.spawn_command_with_config(
            cwd,
            host,
            append_prompt,
            requested_model,
            requested_thinking,
            sandbox,
        )?;
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

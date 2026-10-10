//! Native pi harness: spawns `pi --mode rpc --session-dir <cypher-owned-dir>`
//! and speaks pi's OWN RPC protocol (strict JSONL over stdio) directly — no
//! community `pi-acp` adapter in between.
//!
//! Session truth division: the cypher doc is the display/sync truth (the
//! harness never touches it); the pi session file is the LLM-context truth.
//! `--session-dir` points at a cypher-owned directory, and
//! `RunRequest.resume` (engine-injected) carries the pi session file's
//! ABSOLUTE path: a present value first sends `switch_session`, whose failure
//! is a LOUD error (Done Errored naming the path — never a silent fresh
//! session); an absent value means a fresh session pi creates itself.
//!
//! Event mapping (see the table in `docs/research/pi-rpc.md`): text/thinking
//! deltas, tool calls + capped results, extension errors, and the steer /
//! abort commands. Segment semantics:
//! - each assistant `message_end` emits `AssistantMessageCompleted` (a
//!   journal boundary; the doc fold treats it as a no-op);
//! - a mailbox message arriving mid-turn rides RPC `prompt` with
//!   `streamingBehavior:"steer"`, and pi's response says what it did
//!   (`disposition`, pi ≥ 0.99): `queued` — delivered after the current
//!   assistant message's tool calls, and the NEXT assistant `message_start`
//!   emits `Steered { prev, next }` BEFORE the steered content streams (the
//!   engine splits the doc entry there);
//!   `handled` — an extension consumed it, and its boundary still fires at
//!   the next assistant message or before the turn's Done (the engine retires
//!   one routed message per boundary); `started` — pi had settled first, so
//!   the message opened a fresh run and the next turn. The response is read
//!   in stdout order with the events, so a settle that preceded it on the
//!   wire is always handled first. Older runtimes report no disposition: they
//!   get a raw `steer`, and a steer the turn settled ahead of is cleared from
//!   pi's queue (its next run would deliver it again) and retried as a parked
//!   prompt.
//! - a mailbox message arriving while the session is PARKED restarts it via
//!   RPC `prompt` with `streamingBehavior:"steer"` — atomic across pi's
//!   REAL state: a truly idle pi starts a fresh turn, a pi still (or newly)
//!   active queues the message as a steer. Never a raw `steer` (a parked pi
//!   only queues steers, so one would strand forever) and never a plain
//!   `prompt` (pi REJECTS a prompt without `streamingBehavior` while
//!   streaming — the confirmed parked-session wedge). The `Steered` boundary
//!   fires BEFORE the routed prompt is dispatched, so pre-response
//!   notify/dialog output folds into the new turn's segment (a boundary after
//!   Done with no prompt behind it would re-arm the parked session with no
//!   turn to settle).
//!
//! One child per run (persistent across turns within the run, parked between
//! them while the steering mailbox lives), child-lifecycle hardening
//! (StderrTail, SIGTERM→SIGKILL, PATH composition) reused from `process.rs`.

mod client;
pub mod fork;
mod models;
mod session;
mod throughput;
mod wire;

use models::*;
use session::*;
use wire::*;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::{Map, Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use cypher_proto::{
    AgentEvent, AnsweredModel, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
    SlashCommand, SteeringMode, SubagentRun, SubagentRunMode, SubagentRunStatus, ToolCall,
    UserInputQuestion,
};

use crate::pi::client::{Incoming, PiClient};
use crate::process::{Signal, crash_message, send_signal, shutdown_child};
use crate::{
    Harness, HarnessError, OUTPUT_CAP, RunControls, RunHostContext, cap_text, parse_commands,
};

/// Env vars the subagents extension keys on (mirrors
/// `extensions/subagents/message.ts`): a child pi process loads the extension
/// in child mode and registers the generic messaging tools.
pub(crate) const ENV_ROLE: &str = "PI_SUBAGENT_ROLE";
pub(crate) const ROLE_CHILD: &str = "child";
pub(crate) const ENV_CHANNEL_ROOT: &str = "PI_SUBAGENT_CHANNEL_ROOT";
pub(crate) const ENV_RUN_ID: &str = "PI_SUBAGENT_RUN_ID";
pub(crate) const ENV_AGENT: &str = "PI_SUBAGENT_AGENT";
pub(crate) const ENV_CHILD_INDEX: &str = "PI_SUBAGENT_CHILD_INDEX";
/// The chat id this pi process belongs to (parent or child) — injected by the
/// harness as `CYPHER_CHAT_ID`, consumed by the subagents extension for the
/// Cypher bridge.
pub(crate) const ENV_CYPHER_CHAT_ID: &str = "CYPHER_CHAT_ID";
/// Local engine IPC socket path the extension's Cypher bridge helper dials
/// (`StartSubagent` / `WatchAgentEvents`).
pub(crate) const ENV_CYPHER_ENGINE_SOCKET: &str = "CYPHER_ENGINE_SOCKET";
/// Bridge protocol the engine speaks, set with the socket: the runtime's
/// subagents patch hosts child runs as Cypher child chats only when it reads a
/// version it knows, so an older engine (which truncates the task to the
/// 500-char label) keeps the extension's own child processes. `2` = the
/// `StartSubagent` `prompt` + `address` fields and a lag-tolerant,
/// de-duplicated `WatchAgentEvents`.
pub(crate) const ENV_CYPHER_SUBAGENT_BRIDGE: &str = "CYPHER_SUBAGENT_BRIDGE";
const SUBAGENT_BRIDGE_VERSION: &str = "2";
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
const ENV_PACKAGE_DIR: &str = "PI_PACKAGE_DIR";

/// The messaging tools every child gets regardless of its allowlist (the
/// extension's `spawn.ts` appends the same trio).
const MESSAGING_TOOLS: [&str; 3] = ["send_message", "read_inbox", "reply_message"];

/// Write the child agent's persisted system prompt to a 0600 temp file
/// (`--append-system-prompt` takes a path) and return it for later cleanup.
fn write_temp_prompt(agent: &str, prompt: &str) -> std::io::Result<PathBuf> {
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

/// pi's thinking ladder in cypher terms (its extra "off" tier has no cypher
/// equivalent and stays the agent default). Each model offers a subset of it
/// ([`model_ladder`]).
const FULL_LADDER: [ReasoningLevel; 6] = [
    ReasoningLevel::Minimal,
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
    ReasoningLevel::Max,
];

/// Map cypher's reasoning level onto pi's `set_thinking_level` value.
/// Ultra-family modes collapse to max (pi has no ultra tiers).
fn thinking_level(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal => "minimal",
        ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::XHigh => "xhigh",
        ReasoningLevel::Max
        | ReasoningLevel::Ultra
        | ReasoningLevel::Ultracode
        | ReasoningLevel::Ultrathink => "max",
    }
}

/// The levels pi honors for a `get_available_models` entry — a mirror of
/// pi-ai's `getSupportedThinkingLevels`: a `null` in `thinkingLevelMap`
/// disables that level, and `xhigh`/`max` exist only when the map names them.
/// pi clamps any other request to a neighbouring level, so offering it would
/// only mislabel what actually runs. Empty when the model cannot think.
fn model_ladder(m: &Value) -> Vec<ReasoningLevel> {
    if !m.get("reasoning").and_then(Value::as_bool).unwrap_or(false) {
        return Vec::new();
    }
    let map = m.get("thinkingLevelMap");
    FULL_LADDER
        .into_iter()
        .filter(
            |&level| match map.and_then(|map| map.get(thinking_level(level))) {
                Some(Value::Null) => false,
                Some(_) => true,
                None => !matches!(level, ReasoningLevel::XHigh | ReasoningLevel::Max),
            },
        )
        .collect()
}

/// Cypher uses `unknown/unknown` as the empty-catalog placeholder. It is not a
/// real Pi model and must never be passed to `pi --model`: extension commands
/// such as `/newapi-provider-add` are specifically how a fresh installation
/// creates its first provider/model.
fn concrete_model(model: &str) -> bool {
    model.split_once('/').is_some_and(|(provider, id)| {
        !provider.is_empty() && !id.is_empty() && provider != "unknown" && id != "unknown"
    })
}

/// Minimum gap between forwarded [`AgentEvent::ToolProgress`] events for the
/// SAME tool call. pi streams `tool_execution_update` per partial chunk (a
/// subagent can emit many per second); the doc fold rewrites the tool part's
/// transient column on every tick, so forwarding every one would churn doc
/// writes + cross-device sync for content that only ever shows the last 8
/// lines. First update per tool always forwards (the fold needs the initial
/// tail); after that, ≥500ms between forwards keeps the live card fresh
/// without the churn.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(500);

fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Built-in TUI slash commands with RPC equivalents, synthesized into the
/// discovery result. pi's `get_commands` lists only extension / prompt /
/// skill commands — built-in TUI commands (`/compact`, `/export-html`, …) are
/// NOT advertised, and sending one as prompt text would not execute it (pi
/// only executes `get_commands` results via `prompt`). The harness advertises
/// the ones it can dispatch over RPC itself; a same-name discovered command
/// always wins (dedup) and is left to pi.
const SYNTHESIZED_COMMANDS: [(&str, &str, &str); 2] = [
    (
        "compact",
        "Compact the conversation context (pi built-in)",
        "custom instructions",
    ),
    (
        "export-html",
        "Export the session to an HTML file (pi built-in)",
        "output path",
    ),
];

/// Append the synthesized built-ins to the discovered commands, skipping any
/// whose name a discovered extension/prompt/skill command already owns. The
/// synthesized entries land at the tail. Hide/show for the composer `/` menu
/// is a UI preference (Settings → Commands), not a harness filter.
fn synthesize_commands(discovered: &[SlashCommand]) -> Vec<SlashCommand> {
    let mut commands = discovered.to_vec();
    for (name, description, hint) in SYNTHESIZED_COMMANDS {
        if discovered.iter().any(|c| c.name == name) {
            continue;
        }
        commands.push(SlashCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            input_hint: Some(hint.to_owned()),
        });
    }
    commands
}

/// Rotate the assistant message id; returns (previous, next).
fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
}

/// Private files every Cypher-spawned Pi loads: the engine client module the
/// bundled extensions import.
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
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.path().join("engine-client.mjs"))
            .map_err(|e| e.to_string())?;
        file.write_all(include_str!("pi/engine-client.mjs").as_bytes())
            .map_err(|e| e.to_string())?;
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

/// Resolve the pi CLI: `PI_EXECUTABLE` override, then the shared CLI resolver
/// (PATH + login-shell PATH + npm-global bins + node-version-manager bins).
fn resolve_executable() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PI_EXECUTABLE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    crate::resolve_cli("pi")
}

/// The native pi harness. Construct with [`PiHarness::new`]; tests point it at
/// a fake pi with [`PiHarness::with_executable`].
pub struct PiHarness {
    executable: Option<PathBuf>,
    /// Cypher-owned Pi config root. When set, the harness never reads
    /// `~/.pi/agent`.
    agent_dir: Option<PathBuf>,
    /// Package root for Pi's built-in assets (`dist/`, themes, export
    /// templates) when launched from the Cypher runtime wrapper.
    package_dir: Option<PathBuf>,
    /// cypher-owned pi session store (`<profile store>/agent-sessions`),
    /// passed through as `--session-dir`.
    session_dir: PathBuf,
    interrupt_grace: Duration,
    kill_grace: Duration,
    handshake_timeout: Duration,
    /// How long a turn waits for a first agent event after the prompt is
    /// accepted before settling itself with `Done{Completed}`. A turn with no
    /// agent activity at all (e.g. an extension command whose handler only
    /// notifies) must never sit "Working" forever. On a steerable run the
    /// child remains parked afterward so process-local extension state (such
    /// as `/fast`) survives into the next turn.
    no_activity_grace: Duration,
    /// How long model discovery waits for `get_available_models` to become
    /// non-empty. pi's RPC snapshot is empty until the catalog refresh
    /// finishes (`--list-models` awaits that refresh; RPC does not).
    model_catalog_wait: Duration,
    /// Local engine private Unix socket path — injected
    /// into every pi child as `CYPHER_ENGINE_SOCKET` so the subagents
    /// extension can reach the engine's `StartSubagent`/`WatchAgentEvents`
    /// bridge. Set by `default_registry_with_bridge` (production assembly
    /// knows `ipc_socket`); `None` in bare tests and edge-less engines.
    engine_socket: Option<String>,
    /// Discovery result cache: the RAW `get_commands` probe result (extension /
    /// prompt / skill commands) survives across calls until
    /// [`Self::invalidate_discovery`]. It stays the interception authority —
    /// `commands()` appends the synthesized built-ins per call, so a populated
    /// cache carrying a same-name command disables the harness-side dispatch
    /// in `run`.
    commands: Mutex<Option<Vec<SlashCommand>>>,
    /// Model discovery cache: only a successful, non-empty probe is cached.
    models_cache: Mutex<Option<Vec<Model>>>,
}

impl PiHarness {
    pub fn new(session_dir: PathBuf) -> Self {
        Self {
            executable: None,
            agent_dir: None,
            package_dir: None,
            session_dir,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            handshake_timeout: Duration::from_secs(120),
            no_activity_grace: Duration::from_secs(2),
            model_catalog_wait: Duration::from_secs(8),
            engine_socket: None,
            commands: Mutex::new(None),
            models_cache: Mutex::new(None),
        }
    }

    /// Point pi children at the local engine IPC socket path (production
    /// assembly). `None` keeps the harness standalone (tests, edge-less).
    pub fn with_engine_bridge(mut self, socket: Option<String>) -> Self {
        self.engine_socket = socket;
        self
    }

    /// Use a fixed pi binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    /// Isolate Pi's mutable config and package-root lookup from the user's
    /// system Pi installation.
    pub fn with_runtime_environment(
        mut self,
        agent_dir: impl Into<PathBuf>,
        package_dir: impl Into<PathBuf>,
    ) -> Self {
        self.agent_dir = Some(agent_dir.into());
        self.package_dir = Some(package_dir.into());
        self
    }

    /// Tune the interrupt→SIGTERM→SIGKILL escalation timing.
    pub fn with_graces(mut self, interrupt_grace: Duration, kill_grace: Duration) -> Self {
        self.interrupt_grace = interrupt_grace;
        self.kill_grace = kill_grace;
        self
    }

    /// Tune the handshake bound (tests shrink it; default 120s).
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Test seam: how long a turn waits for a first agent event after the
    /// prompt is accepted before settling with `Done{Completed}` (default
    /// 2s). Tests shrink it so no-activity settlement is fast.
    pub fn with_no_activity_grace(mut self, grace: Duration) -> Self {
        self.no_activity_grace = grace;
        self
    }

    /// Test seam: how long discovery polls an empty `get_available_models`
    /// snapshot before falling back to `get_state.model`.
    pub fn with_model_catalog_wait(mut self, wait: Duration) -> Self {
        self.model_catalog_wait = wait;
        self
    }

    /// Test seam: the program `run` would spawn (the pi CLI itself).
    #[doc(hidden)]
    pub fn launch_program(&self) -> Result<PathBuf, HarnessError> {
        self.resolve_program().map(|(exe, _)| exe)
    }

    fn resolve_program(&self) -> Result<(PathBuf, Vec<String>), HarnessError> {
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

    fn cached_commands(&self) -> Option<Vec<SlashCommand>> {
        self.commands.lock().ok().and_then(|g| g.clone())
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
        self.spawn_command_with_config(cwd, host, append_prompt, None, None)
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
        )
    }

    fn spawn_command_with_config(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        requested_model: Option<&str>,
        requested_thinking: Option<&str>,
    ) -> Result<Command, HarnessError> {
        let (exe, mut args) = self.resolve_program()?;
        // Child-subagent semantics (Cypher-hosted child chats): restrict tools
        // to the persisted agent allowlist plus the messaging tools, append
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
            for tool in MESSAGING_TOOLS {
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

    async fn spawn_child(
        &self,
        cwd: Option<&str>,
        host: &RunHostContext,
        append_prompt: Option<&PathBuf>,
        requested_model: Option<&str>,
        requested_thinking: Option<&str>,
    ) -> Result<(Child, crate::process::StderrTail), HarnessError> {
        let mut cmd = self.spawn_command_with_config(
            cwd,
            host,
            append_prompt,
            requested_model,
            requested_thinking,
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
                    tracing::debug!(target: "cypher_harness::pi", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }
        Ok((child, stderr_tail))
    }

    /// Short-lived discovery run for [`Harness::models`]: `get_state` (a
    /// liveness probe — the child is up and serving) then poll
    /// `get_available_models` until configured providers appear (or the wait
    /// expires). Size-stability is only a fallback when we do not know which
    /// providers to expect.
    ///
    /// pi's RPC handler returns `modelRuntime.getAvailableSnapshot()` with no
    /// await; `--list-models` instead awaits `getAvailable()`. A probe that
    /// reads the snapshot immediately after spawn therefore sees `[]` even
    /// when the CLI lists dozens of models a moment later. An extension such
    /// as pi-claude-bridge can also fill the snapshot before gateway
    /// providers finish registering — returning at first non-empty would
    /// cache a Claude-only catalog.
    async fn discover_models(&self) -> Result<DiscoveredModels, HarnessError> {
        let (mut child, _stderr) = self
            .spawn_child(None, &RunHostContext::default(), None, None, None)
            .await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let wait = self.model_catalog_wait;
        let expected = self
            .agent_dir
            .as_deref()
            .map(expected_model_providers)
            .unwrap_or_default();
        let discovery = async {
            let state = client.request("get_state", Map::new()).await?;
            let deadline = Instant::now() + wait;
            let stable_for = wait.min(MODEL_CATALOG_STABLE);
            let mut best: Vec<Model> = Vec::new();
            let mut unchanged_since: Option<Instant> = None;
            loop {
                let available = match client.request("get_available_models", Map::new()).await {
                    Ok(value) => value,
                    Err(_err) if !best.is_empty() => {
                        return Ok(DiscoveredModels {
                            models: best,
                            from_catalog: true,
                        });
                    }
                    Err(err) => return Err(err),
                };
                let models = models_from_response(&available);
                let now = Instant::now();
                if models.len() > best.len() {
                    best = models;
                    unchanged_since = Some(now);
                }
                if catalog_covers(&best, &expected) {
                    return Ok(DiscoveredModels {
                        models: best,
                        from_catalog: true,
                    });
                }
                if expected.is_empty() && !best.is_empty() {
                    let since = unchanged_since.get_or_insert(now);
                    if now.duration_since(*since) >= stable_for {
                        return Ok(DiscoveredModels {
                            models: best,
                            from_catalog: true,
                        });
                    }
                }
                if now >= deadline {
                    let from_catalog = !best.is_empty();
                    return Ok(DiscoveredModels {
                        models: if from_catalog {
                            best
                        } else {
                            models_from_responses(&available, &state)
                        },
                        from_catalog,
                    });
                }
                tokio::time::sleep(MODEL_CATALOG_POLL).await;
            }
        };
        // Outer bound is wait + one RPC round-trip + poll slack, so a hung
        // child still cannot pin discovery past the picker timeout.
        let result = tokio::time::timeout(wait + Duration::from_secs(2), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("model discovery timed out".into())),
        }
    }

    /// Short-lived discovery run for [`Harness::commands`]: `get_commands`
    /// (extension / prompt / skill commands, all three sources).
    async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let (mut child, _stderr) = self
            .spawn_child(None, &RunHostContext::default(), None, None, None)
            .await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let discovery = async {
            let resp = client.request("get_commands", Map::new()).await?;
            Ok::<Vec<SlashCommand>, HarnessError>(parse_commands(resp.get("commands")))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }

    /// Run `/command` through a short-lived `pi --mode rpc` child so command
    /// handlers (MCP OAuth, etc.) execute inside Pi, the same path as the TUI.
    async fn run_slash_command(&self, prompt: &str) -> Result<String, HarnessError> {
        self.run_slash_command_ui(prompt, None).await
    }

    async fn run_slash_command_ui(
        &self,
        prompt: &str,
        mut ui: Option<crate::SlashUi>,
    ) -> Result<String, HarnessError> {
        // Same command path as the TUI (`pi --mode rpc` + `/mcp login`).
        let mut cmd = self.spawn_command(None, &RunHostContext::default(), None)?;
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if ui.is_some() {
                Stdio::null()
            } else {
                Stdio::inherit()
            })
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled("pi".into())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => PiClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("pi child has no stdio".into()));
            }
        };
        let mut params = Map::new();
        params.insert("message".into(), Value::String(prompt.to_owned()));
        let prompt_client = client.clone();
        let mut prompt_fut = Box::pin(async move { prompt_client.request("prompt", params).await });
        let mut prompt_done = false;
        let mut output = String::new();
        let mut error: Option<String> = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
        let cancel = ui.as_ref().map(|ui| ui.cancel.clone()).unwrap_or_default();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    error = Some("MCP sign-in cancelled.".into());
                    break;
                }
                response = async {
                    match &mut ui {
                        Some(ui) => ui.responses.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match response {
                        Some((id, payload)) => client.respond_ui(&id, payload),
                        None => { error = Some("MCP sign-in closed.".into()); break; }
                    }
                }
                res = &mut prompt_fut, if !prompt_done => {
                    prompt_done = true;
                    match res {
                        Ok(_) => {}
                        Err(err) => {
                            error = Some(err.to_string());
                            break;
                        }
                    }
                }
                inc = incoming.recv() => match inc {
                    Some(Incoming::UiRequest { id, method, payload }) => {
                        match method.as_str() {
                            "notify" => {
                                let message = payload
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let is_error = payload
                                    .get("notifyType")
                                    .and_then(Value::as_str)
                                    == Some("error");
                                if is_error {
                                    error = Some(message.to_owned());
                                } else if !message.is_empty() {
                                    if !output.is_empty() {
                                        output.push('\n');
                                    }
                                    output.push_str(message);
                                    // `/mcp login` announces the authorization
                                    // link in a notify, before its input dialog.
                                    if let Some(ui) = &ui {
                                        let _ = ui.requests.try_send((id, payload));
                                    }
                                }
                            }
                            // Do NOT cancel input/select: `/mcp login` races
                            // `ui.input` (paste callback URL) against the
                            // localhost OAuth callback. Cancelling input
                            // wins that race and aborts sign-in. Leave the
                            // dialog unanswered; the callback completes it.
                            "select" | "input" | "editor" | "confirm" => {
                                if let Some(ui) = &ui
                                    && (method != "input" || ui.requests.try_send((id, payload)).is_err()) {
                                        error = Some("Unsupported MCP sign-in dialog.".into());
                                        break;
                                    }
                            }
                            _ => {}
                        }
                    }
                    Some(Incoming::Event(_) | Incoming::Response { .. }) => {}
                    Some(Incoming::Eof) | None => break,
                },
                _ = tokio::time::sleep_until(deadline) => {
                    error = Some("The MCP sign-in timed out.".into());
                    break;
                }
            }
            if prompt_done {
                break;
            }
        }
        shutdown_child(&mut child, self.kill_grace).await;
        match error {
            Some(message) if !message.is_empty() => Err(HarnessError::Protocol(message)),
            _ => Ok(output),
        }
    }
}

#[async_trait]
impl Harness for PiHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Pi"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    // pi steers deliver mid-turn (after the current assistant message's tool
    // calls, before the next LLM call) — a step boundary within the live turn.
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    // Every catalog model carries its exact ladder (see `model_ladder`), so
    // there is no harness-wide fallback: one would re-offer the full ladder
    // for models that cannot think.
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }

    /// The pi CLI present on this device: a filesystem probe, never a spawn.
    /// Explicit executables (tests, `PI_EXECUTABLE` overrides) always count.
    fn installed(&self) -> bool {
        self.executable
            .as_ref()
            .is_some_and(|executable| executable.is_file())
            || (self.executable.is_none() && resolve_executable().is_some())
    }

    /// pi's provider/model config is the source of truth: a short-lived probe
    /// reads the configured models (cached on success). An absent binary
    /// surfaces as NotInstalled.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_program()?;
        if let Some(models) = self.models_cache.lock().ok().and_then(|g| g.clone()) {
            return Ok(models);
        }
        let discovered = self.discover_models().await?;
        if discovered.from_catalog
            && !discovered.models.is_empty()
            && let Ok(mut slot) = self.models_cache.lock()
        {
            *slot = Some(discovered.models.clone());
        }
        Ok(discovered.models)
    }

    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        if let Some(discovered) = self.cached_commands() {
            return Ok(synthesize_commands(&discovered));
        }
        let discovered = self.discover_commands().await?;
        if let Ok(mut slot) = self.commands.lock() {
            *slot = Some(discovered.clone());
        }
        Ok(synthesize_commands(&discovered))
    }

    fn invalidate_discovery(&self) {
        if let Ok(mut slot) = self.commands.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.models_cache.lock() {
            *slot = None;
        }
    }

    async fn run_slash(&self, prompt: &str) -> Result<String, HarnessError> {
        self.run_slash_command(prompt).await
    }

    async fn run_slash_interactive(
        &self,
        prompt: &str,
        ui: crate::SlashUi,
    ) -> Result<String, HarnessError> {
        self.run_slash_command_ui(prompt, Some(ui)).await
    }

    /// Session Fork (v1): Pi implements it natively (a separate
    /// `--no-extensions` helper process — see [`PiHarness::fork_session`]).
    async fn fork_session(
        &self,
        request: cypher_proto::PiSessionForkRequest,
    ) -> Result<cypher_proto::PiSessionForkResult, HarnessError> {
        self.fork_session(request).await
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        // The session store must exist before pi is pointed at it.
        std::fs::create_dir_all(&self.session_dir)?;
        // Child-subagent runs append the persisted system prompt from a temp
        // file (`--append-system-prompt` takes a path); owned by the run task
        // so it is cleaned up when the run ends (even on early error).
        let mut temp_prompt: Option<PathBuf> = None;
        if let Some(child) = controls.host.child.as_ref()
            && !child.system_prompt.trim().is_empty()
        {
            temp_prompt = Some(
                match write_temp_prompt(&child.agent, &child.system_prompt) {
                    Ok(path) => path,
                    Err(err) => {
                        return Err(HarnessError::Io(err));
                    }
                },
            );
        }
        let spawn = self
            .spawn_child(
                Some(&request.cwd),
                &controls.host,
                temp_prompt.as_ref(),
                request.model.as_deref(),
                request.reasoning.map(thinking_level),
            )
            .await;
        let (mut child, stderr_tail) = match spawn {
            Ok(child) => child,
            Err(err) => {
                if let Some(path) = &temp_prompt {
                    let _ = std::fs::remove_file(path);
                    let _ = path.parent().map(std::fs::remove_dir);
                }
                return Err(err);
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("pi child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("pi child has no stdout".into()))?;
        let (client, incoming) = PiClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        // Which synthesized built-ins this run intercepts: a populated
        // discovery cache whose probe lists a same-name command hands it to
        // pi (extension wins); an unpopulated cache intercepts — popup
        // selections already passed through `commands()` dedup, so a matching
        // prompt can only be the built-in.
        let intercept = match self.cached_commands() {
            Some(discovered) => BuiltinIntercept::from_probe(&discovered),
            None => BuiltinIntercept::all(),
        };
        let mcp_servers = mcp_server_names(self.agent_dir.as_deref(), &request.cwd);
        tokio::spawn(run_session(Session {
            child,
            client,
            incoming,
            event_tx,
            controls,
            request,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            handshake_timeout: self.handshake_timeout,
            no_activity_grace: self.no_activity_grace,
            model_catalog_wait: self.model_catalog_wait,
            stderr_tail,
            intercept,
            temp_prompt,
            mcp_servers,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

#[cfg(test)]
mod tests;

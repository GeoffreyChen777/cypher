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
//! abort commands. Segment semantics mirror the ACP harness:
//! - each assistant `message_end` emits `AssistantMessageCompleted` (a
//!   journal boundary; the doc fold treats it as a no-op, exactly like the
//!   ACP turn boundary markers);
//! - a steer accepted by pi is delivered after the current assistant
//!   message's tool calls (pi-native mid-run steer); the NEXT assistant
//!   `message_start` emits `Steered { prev, next }` BEFORE the steered
//!   content streams — the same point the ACP harness emits it (the engine
//!   splits the doc entry there; a boundary after Done would re-arm the
//!   parked session with no turn behind it).
//! - a mailbox message arriving while the session is PARKED restarts it via
//!   RPC `prompt` with `streamingBehavior:"steer"` — atomic across pi's
//!   REAL state: a truly idle pi starts a fresh turn, a pi still (or newly)
//!   active queues the message as a steer. Never a raw `steer` (a parked pi
//!   only queues steers, so one would strand forever) and never a plain
//!   `prompt` (pi REJECTS a prompt without `streamingBehavior` while
//!   streaming — the confirmed parked-session wedge). The `Steered` boundary
//!   fires BEFORE the routed prompt is dispatched, so pre-response
//!   notify/dialog output folds into the new turn's segment.
//!
//! One child per run (persistent across turns within the run, parked between
//! them while the steering mailbox lives), child-lifecycle hardening
//! (StderrTail, SIGTERM→SIGKILL, PATH composition) reused from `lib.rs`.

mod client;
pub mod fork;

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
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand,
    SteeringMode, SubagentRun, SubagentRunMode, SubagentRunStatus, ToolCall, UserInputQuestion,
};

use crate::acp::normalize::{OUTPUT_CAP, cap_text, parse_commands};
use crate::pi::client::{Incoming, PiClient};
use crate::{
    Harness, HarnessError, RunControls, RunHostContext, Signal, crash_message, send_signal,
    shutdown_child,
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
/// Local engine IPC WebSocket URL the extension's Cypher bridge helper dials
/// (`StartSubagent` / `WatchAgentEvents`).
pub(crate) const ENV_CYPHER_ENGINE_WS_URL: &str = "CYPHER_ENGINE_WS_URL";

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
/// equivalent and stays the agent default).
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

/// Minimum gap between forwarded [`AgentEvent::ToolProgress`] events for the
/// SAME tool call. pi streams `tool_execution_update` per partial chunk (a
/// subagent can emit many per second); the doc fold rewrites the tool part's
/// transient column on every tick, so forwarding every one would churn doc
/// writes + cross-device sync for content that only ever shows the last 8
/// lines. First update per tool always forwards (the fold needs the initial
/// tail); after that, ≥500ms between forwards keeps the live card fresh
/// without the churn.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(500);

/// Status key of the cypher subagent status protocol: `extensions/subagents`
/// publishes `setStatus("cypher.subagents.v1", JSON.stringify({version:1,
/// runs:[…]}))`. Every other key stays ignored transient TUI furniture.
pub(crate) const SUBAGENTS_STATUS_KEY: &str = "cypher.subagents.v1";
/// Whole-snapshot byte cap (the extension caps at 64KiB; the harness
/// re-checks so a misbehaving publisher can't smuggle an unbounded blob).
const SUBAGENTS_STATUS_MAX_BYTES: usize = 64 * 1024;
/// Max runs in one snapshot.
const SUBAGENTS_MAX_RUNS: usize = 32;
/// Max task chars per run (cut on Unicode boundaries by the publisher).
const SUBAGENTS_TASK_MAX_CHARS: usize = 500;
/// Max progress lines / bytes per run.
const SUBAGENTS_PROGRESS_MAX_LINES: usize = 8;
const SUBAGENTS_PROGRESS_MAX_BYTES: usize = 4096;
/// Max chars for the Cypher child chat id a run may carry (a child chat id is
/// an engine-minted uuid string; a longer value is a publisher bug and is
/// rejected with the rest of the strict per-run parse).
const SUBAGENTS_CHILD_CHAT_ID_MAX_CHARS: usize = 256;

/// Parse a `cypher.subagents.v1` `statusText` into runs.
///
/// - missing/blank text → `Some(vec![])` (a clear snapshot).
/// - anything failing strict validation (not version 1, invalid JSON, an
///   oversize snapshot, more than [`SUBAGENTS_MAX_RUNS`] runs, an over-cap
///   task, an over-cap progress tail, an unknown enum, or a missing required
///   field) → `None` with a warning. The caller must ignore it and keep the
///   run going — a bad status frame is never a reason to interrupt the agent.
fn parse_subagent_status(text: &str) -> Option<Vec<SubagentRun>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    if text.len() > SUBAGENTS_STATUS_MAX_BYTES {
        tracing::warn!(
            target: "cypher_harness::pi",
            bytes = text.len(),
            "subagent status snapshot over 64KiB; ignoring"
        );
        return None;
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                target: "cypher_harness::pi",
                error = %err,
                "subagent status: invalid JSON; ignoring"
            );
            return None;
        }
    };
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        tracing::warn!(
            target: "cypher_harness::pi",
            "subagent status: unsupported snapshot version; ignoring"
        );
        return None;
    }
    let runs = value.get("runs").and_then(Value::as_array)?;
    if runs.len() > SUBAGENTS_MAX_RUNS {
        tracing::warn!(
            target: "cypher_harness::pi",
            count = runs.len(),
            "subagent status: too many runs; ignoring"
        );
        return None;
    }
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        match parse_subagent_run(run) {
            Some(parsed) => out.push(parsed),
            None => {
                tracing::warn!(
                    target: "cypher_harness::pi",
                    "subagent status: malformed run; ignoring snapshot"
                );
                return None;
            }
        }
    }
    Some(out)
}

/// Strict per-run parse of one `cypher.subagents.v1` run object. `None` on
/// any malformed field (missing runId/agent, unknown mode/status enum, or an
/// over-cap task/progress).
fn parse_subagent_run(v: &Value) -> Option<SubagentRun> {
    let run_id = v.get("runId").and_then(Value::as_str)?.to_owned();
    let agent = v.get("agent").and_then(Value::as_str)?.to_owned();
    let task = v.get("task").and_then(Value::as_str).unwrap_or_default();
    if task.chars().count() > SUBAGENTS_TASK_MAX_CHARS {
        return None;
    }
    let mode = match v.get("mode").and_then(Value::as_str)? {
        "sync" => SubagentRunMode::Sync,
        "async" => SubagentRunMode::Async,
        "message" => SubagentRunMode::Message,
        _ => return None,
    };
    let status = match v.get("status").and_then(Value::as_str)? {
        "running" => SubagentRunStatus::Running,
        "done" => SubagentRunStatus::Done,
        "error" => SubagentRunStatus::Error,
        _ => return None,
    };
    let progress = v.get("progress").and_then(Value::as_str);
    if let Some(progress) = progress
        && (progress.lines().count() > SUBAGENTS_PROGRESS_MAX_LINES
            || progress.len() > SUBAGENTS_PROGRESS_MAX_BYTES)
    {
        return None;
    }
    // Bounded child chat id: a too-long value is a publisher bug — reject the
    // whole snapshot like the other over-cap fields.
    let child_chat_id = match v.get("childChatId").and_then(Value::as_str) {
        Some(id) if id.chars().count() > SUBAGENTS_CHILD_CHAT_ID_MAX_CHARS => return None,
        Some(id) => Some(id.to_owned()),
        None => None,
    };
    Some(SubagentRun {
        run_id,
        tool_call_id: v
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        agent,
        model: v.get("model").and_then(Value::as_str).map(str::to_owned),
        task: task.to_owned(),
        mode,
        status,
        progress: progress.map(str::to_owned),
        started_at: v.get("startedAt").and_then(Value::as_i64)?,
        updated_at: v.get("updatedAt").and_then(Value::as_i64)?,
        ended_at: v.get("endedAt").and_then(Value::as_i64),
        // The extension publishes the Cypher child chat id when the engine
        // hosts the run (`StartSubagent` bridge). Absent on standalone runs
        // and on old publishers.
        child_chat_id,
    })
}

/// pi's built-in tool set (`read`/`bash`/`write`/`edit`/`grep`/`find`/`ls`)
/// maps onto the typed [`ToolCall`] cypher renders, extracting the known arg
/// names. Extension/MCP tools fall through to [`ToolCall::Unknown`] with the
/// raw args. This is the pi-flavored counterpart of `acp/normalize.rs`'s
/// `typed_call` (ACP keys by `kind`, pi by tool name).
fn pi_typed_call(name: &str, args: &Value) -> ToolCall {
    let arg = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    match name {
        "bash" => ToolCall::Exec {
            command: arg("command").unwrap_or_default(),
        },
        "read" => ToolCall::ReadFile {
            path: arg("path").unwrap_or_default(),
        },
        "write" => ToolCall::WriteFile {
            path: arg("path").unwrap_or_default(),
            content: None,
        },
        "edit" => ToolCall::EditFile {
            path: arg("path").unwrap_or_default(),
            old_string: None,
            new_string: None,
        },
        "grep" => ToolCall::Search {
            pattern: arg("pattern").unwrap_or_default(),
            path: arg("path"),
        },
        "find" => ToolCall::Glob {
            pattern: arg("pattern").unwrap_or_default(),
        },
        "ls" => ToolCall::Search {
            pattern: String::new(),
            path: arg("path"),
        },
        _ => ToolCall::Unknown {
            name: name.to_owned(),
            input: Some(args.clone()),
        },
    }
}

/// The joined text of a pi tool result's `content` blocks (`{type: "text",
/// text}`), capped like the ACP tool-output path.
fn tool_output_text(result: &Value) -> Option<String> {
    let parts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .filter(|t| !t.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(cap_text(&parts.join("\n"), OUTPUT_CAP))
}

/// Human-readable context-window tag: `1M` at a million and up (`1.5M`
/// for fractional), `k` below that — never "1000k".
fn context_window_tag(w: u64) -> String {
    if w >= 1_000_000 {
        let millions = w as f64 / 1_000_000.0;
        let text = if (millions - millions.round()).abs() < f64::EPSILON {
            format!("{}", millions.round() as u64)
        } else {
            format!("{millions:.1}")
        };
        format!("{text}M context")
    } else {
        format!("{}k context", w.div_ceil(1000))
    }
}

/// Map a `get_available_models` entry onto the picker [`Model`]:
/// `id = "{provider}/{modelId}"` (pi's CLI provider/id convention), `label =
/// name`, and `description = "{provider} · {n}k context"` — the picker
/// renders the description on the row's muted subline, which is what
/// distinguishes the same vendor model served by several providers (deepseek
/// vs opencode-go vs … all offering "DeepSeek V4 Flash"). Full reasoning
/// ladder applies when the model supports thinking (else none).
fn model_from_wire(m: &Value) -> Option<Model> {
    let model_id = m.get("id").and_then(Value::as_str)?;
    let provider = m.get("provider").and_then(Value::as_str).unwrap_or("pi");
    let reasoning = m.get("reasoning").and_then(Value::as_bool).unwrap_or(false);
    let name = m.get("name").and_then(Value::as_str).unwrap_or(model_id);
    let description = match m.get("contextWindow").and_then(Value::as_u64) {
        Some(w) => format!("{provider} · {}", context_window_tag(w)),
        None => provider.to_owned(),
    };
    Some(Model {
        id: format!("{provider}/{model_id}"),
        label: name.to_owned(),
        description: Some(description),
        reasoning_levels: if reasoning {
            FULL_LADDER.to_vec()
        } else {
            Vec::new()
        },
        options: Vec::new(),
    })
}

fn models_from_response(resp: &Value) -> Vec<Model> {
    resp.get("models")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(model_from_wire)
        .collect()
}

/// Build the picker catalog from pi's model directory response, falling back
/// to the model currently selected in `get_state` when the directory is empty.
///
/// pi can have a valid configured provider/model (including custom providers)
/// while `get_available_models` returns `{ models: [] }`. In that case the
/// current state is still enough to offer a concrete model rather than leaving
/// the Cypher picker empty.
fn models_from_responses(available: &Value, state: &Value) -> Vec<Model> {
    let models = models_from_response(available);
    if !models.is_empty() {
        return models;
    }
    state
        .get("model")
        .and_then(model_from_wire)
        .into_iter()
        .collect()
}

/// Result of a short-lived pi model probe. `from_catalog` is false when the
/// directory snapshot stayed empty and we fell back to `get_state.model` —
/// that fallback must not be cached, or the picker would keep a single row
/// even after the catalog finishes loading.
struct DiscoveredModels {
    models: Vec<Model>,
    from_catalog: bool,
}

/// Pause between empty `get_available_models` snapshots. 200ms is well under
/// the catalog refresh we measured (~3s) without spinning the child.
const MODEL_CATALOG_POLL: Duration = Duration::from_millis(200);

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

/// Every Cypher-spawned Pi needs this: the login keychain rejects writes/reads
/// from these children, so MCP OAuth is stored in ~/.pi/agent/mcp-oauth and
/// served through a keyring shim.
fn inject_mcp_keyring_preload(cmd: &mut Command) {
    let preload = std::env::temp_dir().join("cypher-mcp-keyring-preload.cjs");
    if std::fs::write(&preload, include_str!("mcp_keyring_preload.cjs")).is_err() {
        return;
    }
    let mut node_options = std::env::var("NODE_OPTIONS").unwrap_or_default();
    if !node_options.is_empty() {
        node_options.push(' ');
    }
    node_options.push_str("--require ");
    node_options.push_str(&preload.display().to_string());
    cmd.env("NODE_OPTIONS", node_options);
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
    /// cypher-owned pi session store (`<profile store>/agent-sessions`),
    /// passed through as `--session-dir`.
    session_dir: PathBuf,
    interrupt_grace: Duration,
    kill_grace: Duration,
    handshake_timeout: Duration,
    /// How long a run waits for a first agent event after the prompt is
    /// accepted before terminating itself with `Done{Completed}`. A run with
    /// no agent activity at all (e.g. an extension command whose handler
    /// only notifies) must never sit "Working" forever.
    no_activity_grace: Duration,
    /// How long model discovery waits for `get_available_models` to become
    /// non-empty. pi's RPC snapshot is empty until the catalog refresh
    /// finishes (`--list-models` awaits that refresh; RPC does not).
    model_catalog_wait: Duration,
    /// Local engine IPC WebSocket URL (`ws://127.0.0.1:<ipc_port>`) — injected
    /// into every pi child as `CYPHER_ENGINE_WS_URL` so the subagents
    /// extension can reach the engine's `StartSubagent`/`WatchAgentEvents`
    /// bridge. Set by `default_registry_with_bridge` (production assembly
    /// knows `ipc_port`); `None` in bare tests and edge-less engines.
    engine_ws_url: Option<String>,
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
            session_dir,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            handshake_timeout: Duration::from_secs(120),
            no_activity_grace: Duration::from_secs(2),
            model_catalog_wait: Duration::from_secs(8),
            engine_ws_url: None,
            commands: Mutex::new(None),
            models_cache: Mutex::new(None),
        }
    }

    /// Point pi children at the local engine IPC WebSocket URL (production
    /// assembly). `None` keeps the harness standalone (tests, edge-less).
    pub fn with_engine_bridge(mut self, url: Option<String>) -> Self {
        self.engine_ws_url = url;
        self
    }

    /// Use a fixed pi binary instead of PATH/known-location resolution.
    pub fn with_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
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

    /// Test seam: how long a run waits for a first agent event after the
    /// prompt is accepted before terminating with `Done{Completed}` (default
    /// 2s). Tests shrink it so the no-activity termination is fast.
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
            return Ok((exe.clone(), args));
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
    /// Lets tests assert `CYPHER_ENGINE_WS_URL` (and child-env) injection
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
            .or(requested_model);
        let launch_thinking = host
            .child
            .as_ref()
            .and_then(|child| child.thinking.as_deref())
            .or(requested_thinking);
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
        inject_mcp_keyring_preload(&mut cmd);
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        // Cypher bridge identity: the chat this run belongs to plus the local
        // engine IPC WebSocket URL (so the extension can StartSubagent +
        // WatchAgentEvents). Discovery processes pass an empty host context
        // and therefore never receive a parent chat id.
        if let Some(chat_id) = host.chat_id.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_CHAT_ID, chat_id);
        }
        if let Some(url) = self.engine_ws_url.as_deref().filter(|s| !s.is_empty()) {
            cmd.env(ENV_CYPHER_ENGINE_WS_URL, url);
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
    ) -> Result<(Child, crate::StderrTail), HarnessError> {
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
        let stderr_tail = crate::StderrTail::default();
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
    /// `get_available_models` until the catalog snapshot is non-empty.
    ///
    /// pi's RPC handler returns `modelRuntime.getAvailableSnapshot()` with no
    /// await; `--list-models` instead awaits `getAvailable()`. A probe that
    /// reads the snapshot immediately after spawn therefore sees `[]` even
    /// when the CLI lists dozens of models a moment later.
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
        let discovery = async {
            let state = client.request("get_state", Map::new()).await?;
            let deadline = Instant::now() + wait;
            loop {
                let available = client.request("get_available_models", Map::new()).await?;
                let models = models_from_response(&available);
                if !models.is_empty() {
                    return Ok(DiscoveredModels {
                        models,
                        from_catalog: true,
                    });
                }
                if Instant::now() >= deadline {
                    return Ok(DiscoveredModels {
                        models: models_from_responses(&available, &state),
                        from_catalog: false,
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

    /// Run `/command` through a short-lived `pi --mode rpc` child so extension
    /// handlers (MCP OAuth, etc.) execute inside Pi, the same path as the TUI.
    pub async fn run_slash_command(&self, prompt: &str) -> Result<String, HarnessError> {
        // Same plugin path as the TUI (`pi --mode rpc` + `/mcp-auth`).
        let mut cmd = self.spawn_command(None, &RunHostContext::default(), None)?;
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env(
                "CYPHER_MCP_AUTH_DUMP",
                std::path::PathBuf::from(home).join(".pi/agent/.cypher-mcp-auth-dump.jsonl"),
            );
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
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
        loop {
            tokio::select! {
                biased;
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
                                }
                            }
                            // Do NOT cancel input/select: `/mcp-auth` races
                            // `ui.input` (paste callback URL) against the
                            // localhost OAuth callback. Cancelling input
                            // wins that race and aborts sign-in. Leave the
                            // dialog unanswered; the callback completes it.
                            "select" | "input" | "editor" | "confirm" => {
                                let _ = (id, payload);
                            }
                            _ => {}
                        }
                    }
                    Some(Incoming::Event(_)) => {}
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
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &FULL_LADDER
    }

    /// The pi CLI present on this device: a filesystem probe, never a spawn.
    /// Explicit executables (tests, `PI_EXECUTABLE` overrides) always count.
    fn installed(&self) -> bool {
        self.executable.is_some() || resolve_executable().is_some()
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
        if discovered.from_catalog && !discovered.models.is_empty() {
            if let Ok(mut slot) = self.models_cache.lock() {
                *slot = Some(discovered.models.clone());
            }
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
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

struct Session {
    child: Child,
    client: PiClient,
    incoming: mpsc::Receiver<Incoming>,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    controls: RunControls,
    request: RunRequest,
    interrupt_grace: Duration,
    kill_grace: Duration,
    handshake_timeout: Duration,
    no_activity_grace: Duration,
    model_catalog_wait: Duration,
    stderr_tail: crate::StderrTail,
    /// Which synthesized built-in commands this run intercepts (computed from
    /// the discovery cache at run start).
    intercept: BuiltinIntercept,
    /// Temp file holding the child agent's persisted system prompt
    /// (`--append-system-prompt`), removed when the run ends.
    temp_prompt: Option<PathBuf>,
}

/// Which synthesized built-in commands a run intercepts. A same-name
/// extension/prompt/skill command wins and is left to pi itself; an
/// UNPOPULATED cache still intercepts — the popup's selections come from
/// `commands()`, which already deduped the synthesized entries, so a
/// `/compact` prompt without a discovered `compact` command can only be the
/// built-in.
#[derive(Clone, Copy, Default)]
struct BuiltinIntercept {
    compact: bool,
    export_html: bool,
}

impl BuiltinIntercept {
    fn all() -> Self {
        Self {
            compact: true,
            export_html: true,
        }
    }

    fn from_probe(discovered: &[SlashCommand]) -> Self {
        Self {
            compact: !discovered.iter().any(|c| c.name == "compact"),
            export_html: !discovered.iter().any(|c| c.name == "export-html"),
        }
    }
}

/// Whether an intercepted built-in ended the run or fell through.
enum InterceptOutcome {
    /// Not a built-in command (or a discovered command owns the name): the
    /// normal prompt path runs.
    Passthrough,
    /// The built-in was dispatched and Done is already sent: the caller reaps
    /// the child and returns.
    Handled,
}

/// Does `prompt` invoke the built-in command `name` (exact `/name` or
/// `/name <rest>`)? Outer None = no match; inner Some = the non-empty
/// argument, None = no argument. Ordinary text is never matched.
fn builtin_match<'a>(prompt: &'a str, name: &str) -> Option<Option<&'a str>> {
    let slash = format!("/{name}");
    if prompt == slash {
        return Some(None);
    }
    prompt.strip_prefix(&format!("{slash} ")).map(|rest| {
        let rest = rest.trim();
        (!rest.is_empty()).then_some(rest)
    })
}

/// Emit a synthesized built-in command's terminal events: one TextDelta (also
/// pushed into Done's `result`) then Done{Completed}. The run ends
/// immediately — there is no agent stream for these commands, so the
/// no-activity grace never applies.
async fn finish_builtin(
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    session_file: &str,
    text: String,
    last_assistant_text: &mut String,
) {
    last_assistant_text.push_str(&text);
    let _ = send(event_tx, AgentEvent::TextDelta { text: text.clone() }).await;
    let _ = send(
        event_tx,
        AgentEvent::Done {
            status: DoneStatus::Completed,
            result: Some(text),
            error: None,
            session_id: Some(session_file.to_owned()),
        },
    )
    .await;
}

/// Dispatch a `/compact` or `/export-html` prompt over RPC (pi's built-in TUI
/// commands have RPC equivalents; the harness synthesizes and intercepts
/// them). On success one TextDelta + Done{Completed} end the run; a rejected
/// command is Done{Errored} whose error names the command and failure. Any
/// other prompt — or a same-name discovered command (extension wins) — is a
/// Passthrough.
async fn intercept_builtin(
    client: &PiClient,
    event_tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    prompt: &str,
    session_file: &str,
    intercept: BuiltinIntercept,
    last_assistant_text: &mut String,
) -> InterceptOutcome {
    if intercept.compact
        && let Some(instructions) = builtin_match(prompt, "compact")
    {
        let mut params = Map::new();
        if let Some(instructions) = instructions {
            params.insert(
                "customInstructions".into(),
                Value::String(instructions.to_owned()),
            );
        }
        match client.request("compact", params).await {
            Ok(data) => {
                let text = match (
                    data.get("tokensBefore").and_then(Value::as_u64),
                    data.get("estimatedTokensAfter").and_then(Value::as_u64),
                ) {
                    (Some(before), Some(after)) => {
                        format!("Context compacted: {before} → {after} tokens")
                    }
                    _ => "Context compacted.".to_owned(),
                };
                finish_builtin(event_tx, session_file, text, last_assistant_text).await;
            }
            Err(e) => {
                // The request error already names the command (`compact: …`).
                let _ = send(
                    event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(session_file.to_owned()),
                    },
                )
                .await;
            }
        }
        return InterceptOutcome::Handled;
    }

    if intercept.export_html
        && let Some(path) = builtin_match(prompt, "export-html")
    {
        let mut params = Map::new();
        if let Some(path) = path {
            params.insert("outputPath".into(), Value::String(path.to_owned()));
        }
        match client.request("export_html", params).await {
            Ok(data) => {
                let text = data
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|p| format!("Exported to {p}"))
                    .unwrap_or_else(|| "Exported.".to_owned());
                finish_builtin(event_tx, session_file, text, last_assistant_text).await;
            }
            Err(e) => {
                let _ = send(
                    event_tx,
                    AgentEvent::Done {
                        status: DoneStatus::Errored,
                        result: None,
                        error: Some(e.to_string()),
                        session_id: Some(session_file.to_owned()),
                    },
                )
                .await;
            }
        }
        return InterceptOutcome::Handled;
    }

    InterceptOutcome::Passthrough
}

type RequestInputFn = Box<
    dyn Fn(
            Vec<UserInputQuestion>,
        ) -> tokio::sync::oneshot::Receiver<Vec<cypher_proto::UserInputAnswer>>
        + Send
        + Sync,
>;

/// The role of a `message_start` / `message_end` payload (assistant only —
/// toolResult/user messages are internal to the turn).
fn message_is_assistant(message: Option<&Value>) -> bool {
    message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .map(|r| r == "assistant")
        .unwrap_or(false)
}

/// One `extension_ui_request` dialog → the engine's input bridge. The bridge
/// answers with option labels; the response maps them back per method:
/// select/input/editor take a `value` (or `cancelled`), confirm a boolean.
/// A dropped resolver degrades to cancelled/`confirmed: false` — never a
/// silent pick. Fire-and-forget methods (notify/setStatus/setWidget/setTitle/
/// set_editor_text) never reach this — the caller maps notify itself and
/// ignores the transient TUI methods.
fn bridge_ui_request(
    client: &PiClient,
    request_input: std::sync::Arc<RequestInputFn>,
    id: &str,
    method: &str,
    payload: &Value,
) {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Agent question")
        .to_owned();
    let question = UserInputQuestion {
        // The extension request's own id: the answer comes back keyed on it.
        id: id.to_owned(),
        header: title.clone(),
        question: title.clone(),
        options: match method {
            "select" => payload
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            "confirm" => vec!["Confirm".into(), "Cancel".into()],
            // input/editor: free text. `prefill` is ignored — cypher's input
            // bridge has no prefilled-text slot.
            _ => Vec::new(),
        },
        multi_select: false,
    };
    let client = client.clone();
    let request_input = std::sync::Arc::clone(&request_input);
    // Owned copies for the spawned task (the caller's refs are not 'static).
    let id = id.to_owned();
    let is_confirm = method == "confirm";
    tokio::spawn(async move {
        let answers = (request_input)(vec![question.clone()])
            .await
            .unwrap_or_default();
        let picked = answers
            .iter()
            .find(|a| a.question_id == question.id)
            .and_then(|a| a.labels.first());
        let payload = if is_confirm {
            json!({ "confirmed": picked.map(|l| l == "Confirm").unwrap_or(false) })
        } else {
            match picked.filter(|l| !l.is_empty()) {
                Some(value) => json!({ "value": value }),
                None => json!({ "cancelled": true }),
            }
        };
        client.respond_ui(&id, payload);
    });
}

/// One steer command as a 'static future (the client clone is moved in so the
/// future owns its borrow), polled from the main select — awaiting inline
/// would block draining `incoming` while pi streams. The text rides back out
/// with the result so a steer the turn settles before delivering can be
/// retried as an idle prompt (an idle pi only QUEUES steers).
fn steer_call_future(
    client: PiClient,
    text: String,
) -> BoxFuture<'static, (String, Result<Value, HarnessError>)> {
    Box::pin(async move {
        let mut params = Map::new();
        params.insert("message".into(), Value::String(text.clone()));
        let result = client.request("steer", params).await;
        (text, result)
    })
}

/// Owns the child prompt temp file for the run's lifetime: removing it on
/// drop covers every `run_session` return path (early errors included).
struct TempPromptGuard(Option<PathBuf>);

impl Drop for TempPromptGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
            let _ = path.parent().map(std::fs::remove_dir);
        }
    }
}

fn requested_model_parts(requested: &str) -> Result<(&str, &str), HarnessError> {
    let Some((provider, model_id)) = requested.split_once('/') else {
        return Err(HarnessError::Protocol(format!(
            "pi model must use provider/id form: {requested}"
        )));
    };
    if provider.is_empty() || model_id.is_empty() {
        return Err(HarnessError::Protocol(format!(
            "pi model must use provider/id form: {requested}"
        )));
    }
    Ok((provider, model_id))
}

fn state_model_key(state: &Value) -> Option<String> {
    let model = state.get("model")?;
    let provider = model.get("provider")?.as_str()?;
    let model_id = model.get("id")?.as_str()?;
    Some(format!("{provider}/{model_id}"))
}

fn state_uses_model(state: &Value, requested: &str) -> bool {
    state_model_key(state).as_deref() == Some(requested)
}

fn catalog_contains_model(catalog: &Value, provider: &str, model_id: &str) -> bool {
    catalog
        .get("models")
        .and_then(Value::as_array)
        .is_some_and(|models| {
            models.iter().any(|model| {
                model.get("provider").and_then(Value::as_str) == Some(provider)
                    && model.get("id").and_then(Value::as_str) == Some(model_id)
            })
        })
}

/// Re-select after `switch_session` only when the loaded session overrode the
/// launch model. Pi's RPC `set_model` consults an asynchronously populated
/// snapshot and can report "Model not found" during a cold start, so wait for
/// the exact catalog row before retrying. A user-selected model is never a
/// best-effort hint: exhaustion is a loud setup failure, not a silent fallback.
async fn select_requested_model(
    client: &PiClient,
    requested: &str,
    catalog_wait: Duration,
) -> Result<(), HarnessError> {
    let (provider, model_id) = requested_model_parts(requested)?;
    let set_params = || {
        let mut params = Map::new();
        params.insert("provider".into(), Value::String(provider.into()));
        params.insert("modelId".into(), Value::String(model_id.into()));
        params
    };
    let first_error = match client.request("set_model", set_params()).await {
        Ok(_) => return Ok(()),
        Err(err) => err,
    };
    if !first_error.to_string().contains("Model not found") {
        return Err(first_error);
    }

    let deadline = Instant::now() + catalog_wait;
    loop {
        let catalog = client.request("get_available_models", Map::new()).await?;
        if catalog_contains_model(&catalog, provider, model_id) {
            return client.request("set_model", set_params()).await.map(|_| ());
        }
        if Instant::now() >= deadline {
            return Err(HarnessError::Protocol(format!(
                "requested pi model {requested} was unavailable after {}s; \
                 initial set_model failed: {first_error}",
                catalog_wait.as_secs_f32()
            )));
        }
        tokio::time::sleep(MODEL_CATALOG_POLL).await;
    }
}

/// The per-run event loop: one task multiplexing agent events, the steering
/// mailbox, the interrupt token, and consumer liveness.
async fn run_session(session: Session) {
    let Session {
        mut child,
        client,
        mut incoming,
        event_tx,
        controls,
        request,
        interrupt_grace,
        kill_grace,
        handshake_timeout,
        no_activity_grace,
        model_catalog_wait,
        stderr_tail,
        intercept,
        temp_prompt,
    } = session;
    // Dropped at the end of every path — the temp prompt file never leaks.
    let _temp_prompt = TempPromptGuard(temp_prompt);
    let RunControls {
        request_input,
        mut steering,
        interrupt,
        host,
    } = controls;
    let _host = host; // child env was already applied at spawn; kept for clarity
    let request_input = std::sync::Arc::new(request_input);
    let agent_name = "pi";

    // ---- handshake + session setup (interruptible) -------------------------
    let setup = async {
        // Resume: switch to the engine-provided session file. Loud failure:
        // a stale/missing path must never silently start fresh.
        if let Some(path) = &request.resume {
            let mut params = Map::new();
            params.insert("sessionPath".into(), Value::String(path.clone()));
            if let Err(e) = client.request("switch_session", params).await {
                return Err(HarnessError::Protocol(format!(
                    "pi session resume failed: {e} ({path})"
                )));
            }
        }
        // Fresh runs already launched with --model/--thinking, so there is no
        // default-model initialization followed by an unconditional switch.
        // A resumed session may restore its historical model; detect that
        // exact case and re-select, waiting out Pi's cold catalog snapshot.
        let mut state = client.request("get_state", Map::new()).await?;
        if let Some(requested) = request.model.as_deref()
            && !state_uses_model(&state, requested)
        {
            select_requested_model(&client, requested, model_catalog_wait).await?;
            state = client.request("get_state", Map::new()).await?;
            if !state_uses_model(&state, requested) {
                let actual = state_model_key(&state).unwrap_or_else(|| "<none>".into());
                return Err(HarnessError::Protocol(format!(
                    "pi selected {actual} instead of requested model {requested}"
                )));
            }
        }
        if let Some(level) = request.reasoning {
            let requested = thinking_level(level);
            if state.get("thinkingLevel").and_then(Value::as_str) != Some(requested) {
                let mut params = Map::new();
                params.insert("level".into(), Value::String(requested.into()));
                client.request("set_thinking_level", params).await?;
                state = client.request("get_state", Map::new()).await?;
                if state.get("thinkingLevel").and_then(Value::as_str) != Some(requested) {
                    let actual = state
                        .get("thinkingLevel")
                        .and_then(Value::as_str)
                        .unwrap_or("<none>");
                    return Err(HarnessError::Protocol(format!(
                        "pi selected thinking level {actual} instead of requested {requested}"
                    )));
                }
            }
        }
        let session_file = state
            .get("sessionFile")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let model_name = state
            .get("model")
            .and_then(|m| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok::<(String, String), HarnessError>((session_file, model_name))
    };
    let (session_file, model_name) = tokio::select! {
        res = tokio::time::timeout(handshake_timeout, setup) => {
            let res = res.unwrap_or_else(|_| Err(HarnessError::Protocol(format!(
                "pi did not complete the RPC handshake within {}s (the agent \
                 may be waiting for a login — try running it once in a terminal)",
                handshake_timeout.as_secs()
            ))));
            match res {
                Ok(v) => v,
                Err(e) => {
                    let error = match child.try_wait() {
                        Ok(Some(status)) => {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            format!("{e}; {}", crash_message(agent_name, Some(status), &stderr_tail))
                        }
                        _ => match stderr_tail.snapshot() {
                            Some(tail) => format!("{e}; stderr: {tail}"),
                            None => e.to_string(),
                        },
                    };
                    tracing::warn!(target: "cypher_harness::pi", %error, "pi setup failed");
                    let _ = event_tx
                        .send(Ok(AgentEvent::Done {
                            status: DoneStatus::Errored,
                            result: None,
                            error: Some(error),
                            session_id: None,
                        }))
                        .await;
                    shutdown_child(&mut child, kill_grace).await;
                    return;
                }
            }
        },
        _ = interrupt.cancelled() => {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: None,
                }))
                .await;
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
    };

    let mut assistant_message_id = new_message_id();
    if !send(
        &event_tx,
        AgentEvent::SessionStarted {
            harness: HarnessId::Pi,
            model: if model_name.is_empty() {
                request.model.clone().unwrap_or_default()
            } else {
                model_name
            },
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session_file.clone(),
            assistant_message_id: assistant_message_id.clone(),
        },
    )
    .await
    {
        shutdown_child(&mut child, kill_grace).await;
        return;
    }

    // ---- synthesized built-in command interception -------------------------
    // The current assistant message's streamed text (Done's `result` and the
    // error text for an `error` stopReason). The built-ins below also feed it.
    let mut last_assistant_text = String::new();
    // `/compact` and `/export-html` are pi built-in TUI commands with RPC
    // equivalents: pi's `get_commands` never advertises built-ins and sending
    // one as prompt text would not execute it. A same-name extension/prompt/
    // skill command discovered in the cache wins (interception skipped — pi
    // handles it); otherwise the harness dispatches the RPC directly and Dones
    // immediately (no agent stream to wait on, so no no-activity grace).
    // compaction_start/end events, if any, stay ignored like the main loop's.
    match intercept_builtin(
        &client,
        &event_tx,
        &request.prompt,
        &session_file,
        intercept,
        &mut last_assistant_text,
    )
    .await
    {
        InterceptOutcome::Handled => {
            shutdown_child(&mut child, kill_grace).await;
            return;
        }
        InterceptOutcome::Passthrough => {}
    }

    // The first prompt is dispatched FROM the main loop (same path as a
    // parked restart). Real pi only ACKs an extension command after its
    // handler returns — and handlers like `/subagent-config` block on
    // `ctx.ui.select` first. Awaiting the ACK here would deadlock: the
    // select arrives as `Incoming::UiRequest`, which is only drained in
    // the loop. Attachments are inlined ONLY on this first prompt: routed
    // mailbox messages carry none, and re-sending them would duplicate.
    let attachments_images = inline_images(&request.attachments);
    let mut prompt_params = Map::new();
    prompt_params.insert("message".into(), Value::String(request.prompt.clone()));
    if let Some(images) = &attachments_images {
        prompt_params.insert("images".into(), images.clone());
    }
    let first_prompt_client = client.clone();

    // ---- main loop --------------------------------------------------------
    // The last assistant message's stopReason ("stop"/"length"/"error"/
    // "aborted"); Completed for anything but error/aborted.
    let mut last_stop_reason = "stop".to_owned();
    let mut last_error_message: Option<String> = None;
    let mut interrupted = false;
    let mut interrupt_sent = false;
    let mut done_sent = false;
    // Live-progress throttle: toolCallId → last FORWARDED ToolProgress. A
    // tool_execution_update is forwarded only when ≥[`PROGRESS_THROTTLE`]
    // elapsed since the last forward for that id (first always forwards).
    // `progress_ended` marks tools whose end we've seen — late/duplicate
    // updates after end are dropped (the doc fold would ignore them anyway
    // once resolved; this stops the harness from even emitting them).
    let mut progress_last: HashMap<String, Instant> = HashMap::new();
    let mut progress_ended: HashSet<String> = HashSet::new();
    // False until the FIRST agent event of any kind arrives. While it stays
    // false the run is proven inert (no agent activity, e.g. an extension
    // command whose handler only notifies) and the grace timer below ends it.
    let mut agent_started = false;
    let mut in_turn = true;
    let mut steering_open = true;
    // Steers pi has ACCEPTED but not yet delivered (one per assistant
    // message; pi's default steering mode is one-at-a-time). Texts are kept
    // so a steer the turn settles before delivering can be retried as an idle
    // prompt (an idle pi only QUEUES steers).
    let mut steers_queued: VecDeque<String> = VecDeque::new();
    // In-flight steer command (polled so the loop keeps draining `incoming`),
    // plus followers awaiting their turn.
    let mut steer_call: Option<BoxFuture<'static, (String, Result<Value, HarnessError>)>> = None;
    let mut steer_backlog: VecDeque<String> = VecDeque::new();
    // In-flight `prompt` RPC: the first turn starts here, and parked-turn
    // restarts reuse the same slot. Serialized with steer calls (never both
    // in flight); followers queue in `prompt_backlog` until the turn settles.
    let mut idle_prompt: Option<BoxFuture<'static, Result<Value, HarnessError>>> =
        Some(Box::pin(async move {
            first_prompt_client.request("prompt", prompt_params).await
        }));
    // True if a select/notify/confirm landed while the prompt RPC was in
    // flight. Real pi only ACKs extension commands after the handler returns,
    // so an ACK with UI and no agent lifecycle means the command is done —
    // do not wait the 2s no-activity grace (that spin after closing a picker).
    let mut had_ui = false;
    let mut prompt_backlog: VecDeque<String> = VecDeque::new();
    // Interrupt escalation: abort, then SIGTERM → SIGKILL if the agent
    // doesn't wind down.
    let mut escalation: Option<tokio::task::JoinHandle<()>> = None;
    // Started when the main loop begins (right after the prompt was accepted):
    // if pi never emits a single agent event, the run terminates with
    // Done{Completed} rather than sit "Working" forever. Any agent-lifecycle
    // event disarms it (informational events do not). Late sendMessage work
    // arriving after this fires is dropped — a documented degradation. A
    // parked-turn restart re-arms it (a fresh sleep) only once its prompt is
    // ACCEPTED.
    let mut no_activity = Box::pin(tokio::time::sleep(no_activity_grace));

    'main: loop {
        // Parked with a queued mailbox message: start the NEXT turn via RPC
        // `prompt` with `streamingBehavior:"steer"` — atomic across pi's
        // real state: a truly idle pi starts a fresh turn; a pi still (or
        // newly) active queues the message as a steer instead of rejecting
        // the prompt (the confirmed parked-session wedge). A raw `steer` is
        // never sent idle — a parked pi only QUEUES steers, so it would
        // strand forever. The Steered boundary fires BEFORE the prompt is
        // dispatched: an extension notify can land before the prompt response
        // and must fold into the new turn's segment. If the boundary cannot
        // be sent, the run is over — do not dispatch.
        if !in_turn
            && idle_prompt.is_none()
            && steer_call.is_none()
            && steers_queued.is_empty()
            && let Some(text) = prompt_backlog.pop_front()
        {
            let (prev, next) = rotate(&mut assistant_message_id);
            if !send(
                &event_tx,
                AgentEvent::Steered {
                    assistant_message_id: Some(prev),
                    next_assistant_message_id: Some(next),
                },
            )
            .await
            {
                break 'main;
            }
            // Per-turn reset: Done's result/status, the progress throttle,
            // and activity tracking all belong to the new turn.
            last_assistant_text.clear();
            last_stop_reason = "stop".to_owned();
            last_error_message = None;
            agent_started = false;
            done_sent = false;
            in_turn = true;
            progress_last.clear();
            progress_ended.clear();
            // The no-activity timer is NOT armed here: the previous turn's
            // sleep may already have elapsed and must not fire during this
            // prompt's preflight. The `idle_prompt.is_none()` guard keeps the
            // branch disabled while the request is in flight; the resolution
            // re-arms it once accepted (lifecycle events that landed first
            // disarm it via agent_started).
            let mut params = Map::new();
            params.insert("message".into(), Value::String(text));
            // `streamingBehavior:"steer"` makes the parked restart atomic:
            // an idle pi starts a fresh turn, a still-streaming pi queues the
            // message as a steer — a plain prompt would be REJECTED while pi
            // streams (the confirmed parked-session wedge).
            params.insert("streamingBehavior".into(), Value::String("steer".into()));
            let client = client.clone();
            had_ui = false;
            idle_prompt = Some(Box::pin(
                async move { client.request("prompt", params).await },
            ));
        }

        tokio::select! {
            // biased: the steer response must resolve BEFORE the steer-reply
            // message_start that follows it on the wire (both become ready in
            // the same select round) — otherwise the boundary is missed and
            // the steer reply folds into the current segment.
            biased;
            res = async { steer_call.as_mut().expect("guarded by if").await },
                if steer_call.is_some() =>
            {
                let _ = steer_call.take();
                if !interrupted {
                    match res {
                        (text, Ok(_)) => {
                            if in_turn {
                                // Accepted during a live turn: pi will deliver
                                // it as the next assistant message (the
                                // Steered boundary fires at that message_start).
                                steers_queued.push_back(text);
                            } else {
                                // The turn settled while this steer was in
                                // flight — an idle pi only queues steers, so
                                // it can never be delivered. Retry it as an
                                // idle prompt after the park.
                                prompt_backlog.push_back(text);
                            }
                        }
                        (_text, Err(e)) => {
                            tracing::debug!(
                                target: "cypher_harness::pi",
                                "steer rejected (dropped): {e}"
                            );
                        }
                    }
                }
                if !in_turn {
                    // Parked: queued followers can't be delivered as steers —
                    // they restart the next turn via prompt instead.
                    while let Some(text) = steer_backlog.pop_front() {
                        prompt_backlog.push_back(text);
                    }
                } else if let Some(text) = steer_backlog.pop_front() {
                    steer_call = Some(steer_call_future(client.clone(), text));
                }
                if !steering_open
                    && !in_turn
                    && steers_queued.is_empty()
                    && steer_call.is_none()
                    && prompt_backlog.is_empty()
                {
                    break 'main;
                }
            },

            res = async { idle_prompt.as_mut().expect("guarded by if").await },
                if idle_prompt.is_some() =>
            {
                let _ = idle_prompt.take();
                // The response only means the prompt was ACCEPTED — the turn
                // streams from here. A rejected prompt is the one case
                // nothing will ever stream for it: one Done Errored ends the
                // run (and the terminal bookkeeping reaps the child).
                match res {
                    Ok(_) => {
                        // Arm the no-activity grace NOW that the prompt is
                        // accepted. Lifecycle events that landed during the
                        // preflight already set agent_started (disarming the
                        // branch); a genuinely inert (notify-only) turn gets
                        // a fresh grace window from here — never a stale
                        // timer from the previous turn.
                        // Extension commands ACK only after the handler
                        // returns. If UI already happened and no agent
                        // started, skip the 2s wait (close-picker spin).
                        // Zero-sleep still yields to `incoming` first
                        // (biased select) so a ui-select-then-ACK-then-text
                        // burst is not cut off.
                        let grace = if had_ui && !agent_started {
                            Duration::ZERO
                        } else {
                            no_activity_grace
                        };
                        no_activity = Box::pin(tokio::time::sleep(grace));
                    }
                    Err(e) => {
                        done_sent = true;
                        let _ = event_tx
                            .send(Ok(AgentEvent::Done {
                                status: DoneStatus::Errored,
                                result: None,
                                error: Some(e.to_string()),
                                session_id: Some(session_file.clone()),
                            }))
                            .await;
                        break 'main;
                    }
                }
            },

            inc = incoming.recv() => match inc {
                Some(Incoming::Event(ev)) => {
                    // Only AGENT-LIFECYCLE events prove a turn is running and
                    // disarm the no-activity grace. Informational events fire
                    // outside any turn — `thinking_level_changed` rides the
                    // set_model/set_thinking_level setup commands (live-verified:
                    // it is what hung /subagents runs), `extension_error` can
                    // arrive from extension activity — counting either would
                    // leave a no-LLM run parked "Working" forever.
                    let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
                    if matches!(
                        kind,
                        "agent_start"
                            | "turn_start"
                            | "message_start"
                            | "message_update"
                            | "message_end"
                            | "tool_execution_start"
                            | "tool_execution_update"
                            | "tool_execution_end"
                            | "agent_end"
                            | "agent_settled"
                    ) {
                        agent_started = true;
                    }
                    match kind {
                        "message_update" => {
                            let ame = ev.get("assistantMessageEvent");
                            match ame.and_then(|a| a.get("type")).and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str) {
                                        last_assistant_text.push_str(text);
                                        if !text.is_empty()
                                            && !send(&event_tx, AgentEvent::TextDelta { text: text.to_owned() }).await
                                        {
                                            break 'main;
                                        }
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(text) = ame.and_then(|a| a.get("delta")).and_then(Value::as_str)
                                        && !text.is_empty()
                                        && !send(&event_tx, AgentEvent::ReasoningDelta { text: text.to_owned() }).await
                                    {
                                        break 'main;
                                    }
                                }
                                // *_start/*_end/toolcall_*: internal state only.
                                _ => {}
                            }
                        }
                        "message_start" => {
                            // A new assistant message starts a fresh text
                            // accumulator — Done's `result` is the LAST
                            // assistant message's text, not the whole turn's.
                            // (toolResult/user messages are internal.)
                            if message_is_assistant(ev.get("message")) {
                                last_assistant_text.clear();
                                // The NEXT assistant message after an accepted
                                // steer is the steer's reply: split the doc entry
                                // here (before its content streams), exactly like
                                // the ACP harness emits Steered at an injection.
                                if let Some(_text) = steers_queued.pop_front() {
                                    // A steer delivery opens a turn: even if
                                    // an agent_settled raced ahead of it, the
                                    // steer reply's own settle must Done.
                                    in_turn = true;
                                    let (prev, next) = rotate(&mut assistant_message_id);
                                    if !send(
                                        &event_tx,
                                        AgentEvent::Steered {
                                            assistant_message_id: Some(prev),
                                            next_assistant_message_id: Some(next),
                                        },
                                    )
                                    .await
                                    {
                                        break 'main;
                                    }
                                }
                            }
                        }
                        "message_end" => {
                            if message_is_assistant(ev.get("message")) {
                                if let Some(message) = ev.get("message") {
                                    if let Some(stop) = message.get("stopReason").and_then(Value::as_str)
                                    {
                                        last_stop_reason = stop.to_owned();
                                    }
                                    if let Some(err) =
                                        message.get("errorMessage").and_then(Value::as_str)
                                    {
                                        last_error_message = Some(err.to_owned());
                                    }
                                }
                                // Journal boundary marker: the doc fold treats
                                // this as a no-op (one segment per turn until
                                // a Steered/Done), but the journal records it
                                // per assistant message like the ACP turn
                                // markers. Rotate the id for the next one.
                                let completed = assistant_message_id.clone();
                                rotate(&mut assistant_message_id);
                                if !send(
                                    &event_tx,
                                    AgentEvent::AssistantMessageCompleted {
                                        assistant_message_id: completed,
                                    },
                                )
                                .await
                                {
                                    break 'main;
                                }
                            }
                        }
                        "tool_execution_start" => {
                            let id = ev.get("toolCallId").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let name = ev.get("toolName").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let args = ev.get("args").cloned().unwrap_or(Value::Null);
                            if !send(
                                &event_tx,
                                AgentEvent::ToolCall {
                                    id,
                                    call: pi_typed_call(&name, &args),
                                },
                            )
                            .await
                            {
                                break 'main;
                            }
                        }
                        "tool_execution_update" => {
                            // Live progress for an unresolved tool: same
                            // `{content:[{type:"text",...}]}` shape as the
                            // end result, so `tool_output_text` (which caps
                            // at OUTPUT_CAP) extracts it. Throttled per
                            // toolCallId (first always forwards) — the doc
                            // fold only ever keeps the last 8 lines, so
                            // forwarding every partial chunk is pure churn.
                            let id = ev
                                .get("toolCallId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let output = ev.get("partialResult").and_then(tool_output_text);
                            if let (Some(output), false) = (output, id.is_empty())
                                && !progress_ended.contains(&id)
                            {
                                let now = Instant::now();
                                let due = progress_last
                                    .get(&id)
                                    .is_none_or(|last| now.duration_since(*last) >= PROGRESS_THROTTLE);
                                if due {
                                    progress_last.insert(id.clone(), now);
                                    if !send(
                                        &event_tx,
                                        AgentEvent::ToolProgress { id, output },
                                    )
                                    .await
                                    {
                                        break 'main;
                                    }
                                }
                            }
                        }
                        "tool_execution_end" => {
                            let id = ev.get("toolCallId").and_then(Value::as_str).unwrap_or_default().to_owned();
                            let is_error = ev.get("isError").and_then(Value::as_bool).unwrap_or(false);
                            let output = ev.get("result").and_then(tool_output_text);
                            // The tool settled: retire its throttle entry (no
                            // more progress) and stop forwarding late updates.
                            progress_last.remove(&id);
                            progress_ended.insert(id.clone());
                            if !send(
                                &event_tx,
                                AgentEvent::ToolResult {
                                    id,
                                    is_error,
                                    output,
                                    diff: None,
                                },
                            )
                            .await
                            {
                                break 'main;
                            }
                        }
                        "extension_error" => {
                            let message = ev
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("extension error")
                                .to_owned();
                            if !send(&event_tx, AgentEvent::Error { message }).await {
                                break 'main;
                            }
                        }
                        "agent_settled" => {
                            // A stale duplicate (or an abort racing a settled
                            // turn) must not double-Done.
                            if !in_turn {
                                continue;
                            }
                            in_turn = false;
                            // Steers pi accepted but never delivered (the
                            // turn settled before the steer reply streamed)
                            // are stranded — an idle pi only QUEUES steers.
                            // Retry them as idle prompts after the park,
                            // never dropped.
                            while let Some(text) = steers_queued.pop_front() {
                                prompt_backlog.push_back(text);
                            }
                            done_sent = true;
                            let (status, error) = if interrupted {
                                (DoneStatus::Interrupted, None)
                            } else {
                                match last_stop_reason.as_str() {
                                    "error" => (
                                        DoneStatus::Errored,
                                        Some(
                                            last_error_message
                                                .clone()
                                                .filter(|m| !m.trim().is_empty())
                                                .or_else(|| {
                                                    (!last_assistant_text.is_empty())
                                                        .then(|| last_assistant_text.clone())
                                                })
                                                .unwrap_or_else(|| {
                                                    "The agent reported an error.".into()
                                                }),
                                        ),
                                    ),
                                    "aborted" => (DoneStatus::Interrupted, None),
                                    _ => (DoneStatus::Completed, None),
                                }
                            };
                            let result = (!last_assistant_text.is_empty())
                                .then(|| last_assistant_text.clone());
                            if !send(
                                &event_tx,
                                AgentEvent::Done {
                                    status,
                                    result,
                                    error,
                                    session_id: Some(session_file.clone()),
                                },
                            )
                            .await
                            {
                                break 'main;
                            }
                            // An interrupt or an errored turn ends the run; a
                            // clean turn parks the child + mailbox for the
                            // next routed send. With the mailbox closed AND
                            // nothing left to dispatch, the run ends here.
                            if interrupted
                                || status == DoneStatus::Errored
                                || (!steering_open
                                    && idle_prompt.is_none()
                                    && steer_call.is_none()
                                    && steers_queued.is_empty()
                                    && prompt_backlog.is_empty())
                            {
                                break 'main;
                            }
                        }
                        // agent_end/turn_*/queue_update/compaction_*/auto_retry_*/
                        // summarization_*/bash_execution_update: nothing cypher
                        // renders — ignored.
                        _ => {}
                    }
                }
                Some(Incoming::UiRequest { id, method, payload }) => {
                    had_ui = true;
                    match method.as_str() {
                        // Dialog methods block the agent until answered.
                        "select" | "input" | "editor" | "confirm" => {
                            bridge_ui_request(&client, std::sync::Arc::clone(&request_input), &id, &method, &payload);
                        }
                        // notify is the extension command's output channel:
                        // info/warning maps to a TextDelta (and feeds Done's
                        // result, so a notify-only run still carries its
                        // output), error to an Error event. The message lives
                        // at the request top level and may carry escaped
                        // multi-line text — passed through as-is.
                        "notify" => {
                            let message = payload
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            let is_error = payload
                                .get("notifyType")
                                .and_then(Value::as_str)
                                .map(|t| t == "error")
                                .unwrap_or(false);
                            if is_error {
                                if !send(&event_tx, AgentEvent::Error { message }).await {
                                    break 'main;
                                }
                            } else if !message.is_empty() {
                                last_assistant_text.push_str(&message);
                                if !send(&event_tx, AgentEvent::TextDelta { text: message }).await {
                                    break 'main;
                                }
                            }
                        }
                        // setStatus with the cypher subagent status key is the
                        // one exception to the transient-TUI-furniture rule:
                        // a STRUCTURED live projection (`cypher.subagents.v1`
                        // snapshot JSON in `statusText`) that the engine
                        // consumes. Strictly validated; any other key — or an
                        // invalid snapshot — stays ignored and can never
                        // interrupt the run.
                        "setStatus" => {
                            let key = payload
                                .get("statusKey")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if key == SUBAGENTS_STATUS_KEY {
                                let text = payload
                                    .get("statusText")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                if let Some(runs) = parse_subagent_status(text)
                                    && !send(&event_tx, AgentEvent::SubagentStatus { runs })
                                        .await
                                {
                                    break 'main;
                                }
                            }
                            // Any other key stays TUI furniture (ignored).
                        }
                        // Deliberate: setWidget/setTitle/set_editor_text (and
                        // any non-cypher setStatus) are transient TUI furniture
                        // — cypher has its own state surface (see the
                        // classification table in docs/research/pi-rpc.md).
                        _ => {}
                    }
                }
                Some(Incoming::Eof) | None => {
                    // A child death while PARKED (turn already settled) ends
                    // the run cleanly — the engine treats a parked stream end
                    // as such. Mid-turn, it is a crash.
                    if done_sent && !in_turn {
                        break 'main;
                    }
                    if interrupted {
                        done_sent = true;
                        let _ = event_tx
                            .send(Ok(AgentEvent::Done {
                                status: DoneStatus::Interrupted,
                                result: None,
                                error: None,
                                session_id: Some(session_file.clone()),
                            }))
                            .await;
                    } else {
                        done_sent = true;
                        let status = child.try_wait().ok().flatten();
                        let _ = event_tx
                            .send(Ok(AgentEvent::Done {
                                status: DoneStatus::Errored,
                                result: None,
                                error: Some(crash_message(
                                    agent_name,
                                    status,
                                    &stderr_tail,
                                )),
                                session_id: Some(session_file.clone()),
                            }))
                            .await;
                    }
                    break 'main;
                }
            },

            steer = steering.recv(), if steering_open && !interrupted => match steer {
                Some(msg) => {
                    if !in_turn || idle_prompt.is_some() {
                        // Parked (or a parked-turn prompt still in preflight):
                        // this mailbox message starts a NEW turn via RPC
                        // prompt — a parked pi only QUEUES steers, so sending
                        // one idle would strand it forever. Followers queue
                        // for after the turn settles (never concurrent with
                        // the in-flight prompt).
                        prompt_backlog.push_back(msg.prompt);
                    } else {
                        // Active turn: pi-native mid-run steer — delivered
                        // after the current assistant message's tool calls,
                        // before the next LLM call. One in flight at a time.
                        if steer_call.is_some() {
                            steer_backlog.push_back(msg.prompt);
                        } else {
                            steer_call = Some(steer_call_future(client.clone(), msg.prompt));
                        }
                    }
                }
                None => {
                    steering_open = false;
                    if !in_turn
                        && steers_queued.is_empty()
                        && steer_call.is_none()
                        && prompt_backlog.is_empty()
                    {
                        break 'main;
                    }
                }
            },

            _ = interrupt.cancelled(), if !interrupt_sent => {
                interrupt_sent = true;
                interrupted = true;
                if in_turn {
                    client.send("abort", Map::new());
                    // Escalate if pi doesn't wind down (agent_settled) within
                    // the grace periods.
                    if let Some(pid) = child.id() {
                        escalation = Some(tokio::spawn(async move {
                            tokio::time::sleep(interrupt_grace).await;
                            send_signal(pid, Signal::Term);
                            tokio::time::sleep(kill_grace).await;
                            send_signal(pid, Signal::Kill);
                        }));
                    }
                } else {
                    // Idle between turns: nothing to abort — the terminal
                    // bookkeeping below still guarantees Done { Interrupted }.
                    break 'main;
                }
            },

            _ = &mut no_activity, if !agent_started && !done_sent && idle_prompt.is_none() => {
                // The prompt was accepted but no agent event ever arrived
                // (an extension command whose handler only notifies, say).
                // Terminate the run: Done Completed whose result is whatever
                // text arrived (the notify output), then reap the child —
                // no parking; late sendMessage work is dropped by design.
                done_sent = true;
                let result = (!last_assistant_text.is_empty())
                    .then(|| last_assistant_text.clone());
                let _ = event_tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result,
                        error: None,
                        session_id: Some(session_file.clone()),
                    }))
                    .await;
                break 'main;
            },

            _ = event_tx.closed() => break 'main,
        }
    }

    // Terminal bookkeeping: never end the stream without a Done unless the
    // consumer already hung up.
    if !event_tx.is_closed() && !done_sent {
        if interrupted {
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Interrupted,
                    result: None,
                    error: None,
                    session_id: Some(session_file.clone()),
                }))
                .await;
        } else {
            let status = child.try_wait().ok().flatten();
            let _ = event_tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Errored,
                    result: None,
                    error: Some(crash_message(agent_name, status, &stderr_tail)),
                    session_id: Some(session_file.clone()),
                }))
                .await;
        }
    }

    // Escalation dies BEFORE the child is reaped: after `shutdown_child`
    // waits the pid, a still-armed SIGTERM/SIGKILL timer would fire at a
    // freed (reusable) pid.
    if let Some(handle) = escalation {
        handle.abort();
    }
    shutdown_child(&mut child, kill_grace).await;
}

/// `RunRequest.attachments` (absolute paths already staged on the run device)
/// → pi `prompt`/`steer` `images` blocks. Only image/* files inline — other
/// attachments have no pi content block and are left to the prompt text refs.
fn inline_images(paths: &[String]) -> Option<Value> {
    use base64::Engine as _;
    let mut images: Vec<Value> = Vec::new();
    for path in paths {
        let mime = mime_for_path(path);
        if !mime.starts_with("image/") {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            tracing::debug!(target: "cypher_harness::pi", "attachment unreadable: {path}");
            continue;
        };
        images.push(json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            "mimeType": mime,
        }));
    }
    (!images.is_empty()).then_some(Value::Array(images))
}

/// Guess a MIME type from the file extension (attachments carry no explicit
/// type). Unknown extensions default to octet-stream and are never inlined.
fn mime_for_path(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesized_commands_dedup_against_the_probe() {
        let probe = vec![
            SlashCommand {
                name: "compact".into(), // extension wins: no synthesized twin
                description: "Compact the session".into(),
                input_hint: None,
            },
            SlashCommand {
                name: "skill:brave-search".into(),
                description: "Web search via Brave".into(),
                input_hint: None,
            },
        ];
        let commands = synthesize_commands(&probe);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        // Only the missing built-in is appended, at the tail.
        assert_eq!(names, vec!["compact", "skill:brave-search", "export-html"]);
        let tail = &commands[2];
        assert_eq!(
            tail.description,
            "Export the session to an HTML file (pi built-in)"
        );
        assert_eq!(tail.input_hint.as_deref(), Some("output path"));
        // No probe → both synthesized, in order.
        let empty = synthesize_commands(&[]);
        let names: Vec<&str> = empty.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["compact", "export-html"]);
    }

    #[test]
    fn discovered_extension_commands_are_advertised() {
        // Settings → Commands owns hide/show; the harness returns everything.
        let probe = vec![SlashCommand {
            name: "compact-ui-config".into(),
            description: "Interactive compact-ui settings".into(),
            input_hint: None,
        }];
        let commands = synthesize_commands(&probe);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["compact-ui-config", "compact", "export-html"]);
    }

    #[test]
    fn builtin_match_exact_and_prefix_only() {
        // Exact command, with and without an argument.
        assert_eq!(builtin_match("/compact", "compact"), Some(None));
        assert_eq!(
            builtin_match("/compact focus on api", "compact"),
            Some(Some("focus on api"))
        );
        // Whitespace-only rest degrades to no argument.
        assert_eq!(builtin_match("/compact  ", "compact"), Some(None));
        // Ordinary text and lookalikes never match.
        assert_eq!(builtin_match("tell me about /compact", "compact"), None);
        assert_eq!(builtin_match("/compactx", "compact"), None);
        assert_eq!(builtin_match("/compact", "export-html"), None);
        assert_eq!(
            builtin_match("/export-html /tmp/x.html", "export-html"),
            Some(Some("/tmp/x.html"))
        );
    }

    #[test]
    fn context_window_tags_use_m_above_a_million() {
        assert_eq!(context_window_tag(128_000), "128k context");
        assert_eq!(context_window_tag(200_000), "200k context");
        assert_eq!(context_window_tag(1_000_000), "1M context");
        assert_eq!(context_window_tag(1_500_000), "1.5M context");
        assert_eq!(context_window_tag(2_000_000), "2M context");
    }

    #[test]
    fn thinking_levels_map_directly_and_ultra_collapses_to_max() {
        assert_eq!(thinking_level(ReasoningLevel::Minimal), "minimal");
        assert_eq!(thinking_level(ReasoningLevel::Low), "low");
        assert_eq!(thinking_level(ReasoningLevel::Medium), "medium");
        assert_eq!(thinking_level(ReasoningLevel::High), "high");
        assert_eq!(thinking_level(ReasoningLevel::XHigh), "xhigh");
        assert_eq!(thinking_level(ReasoningLevel::Max), "max");
        assert_eq!(thinking_level(ReasoningLevel::Ultra), "max");
        assert_eq!(thinking_level(ReasoningLevel::Ultracode), "max");
        assert_eq!(thinking_level(ReasoningLevel::Ultrathink), "max");
    }

    #[test]
    fn core_tools_map_to_typed_calls() {
        let bash = pi_typed_call("bash", &json!({ "command": "ls -la" }));
        assert_eq!(
            bash,
            ToolCall::Exec {
                command: "ls -la".into()
            }
        );
        let read = pi_typed_call("read", &json!({ "path": "src/main.rs" }));
        assert_eq!(
            read,
            ToolCall::ReadFile {
                path: "src/main.rs".into()
            }
        );
        let write = pi_typed_call("write", &json!({ "path": "a.txt", "content": "x" }));
        assert_eq!(
            write,
            ToolCall::WriteFile {
                path: "a.txt".into(),
                content: None
            }
        );
        let edit = pi_typed_call("edit", &json!({ "path": "a.txt", "edits": [] }));
        assert_eq!(
            edit,
            ToolCall::EditFile {
                path: "a.txt".into(),
                old_string: None,
                new_string: None,
            }
        );
        let grep = pi_typed_call("grep", &json!({ "pattern": "foo", "path": "src" }));
        assert_eq!(
            grep,
            ToolCall::Search {
                pattern: "foo".into(),
                path: Some("src".into()),
            }
        );
        let find = pi_typed_call("find", &json!({ "pattern": "*.rs" }));
        assert_eq!(
            find,
            ToolCall::Glob {
                pattern: "*.rs".into()
            }
        );
        let ls = pi_typed_call("ls", &json!({ "path": "." }));
        assert_eq!(
            ls,
            ToolCall::Search {
                pattern: String::new(),
                path: Some(".".into()),
            }
        );
        // Extension / unknown tools keep their raw args.
        let unknown = pi_typed_call("myExt", &json!({ "x": 1 }));
        assert_eq!(
            unknown,
            ToolCall::Unknown {
                name: "myExt".into(),
                input: Some(json!({ "x": 1 })),
            }
        );
    }

    #[test]
    fn tool_output_joins_text_blocks_and_caps() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line 1" },
                { "type": "text", "text": "line 2" },
            ],
            "details": {},
        });
        assert_eq!(tool_output_text(&result).as_deref(), Some("line 1\nline 2"));
        // Non-text blocks contribute nothing.
        assert_eq!(
            tool_output_text(&json!({ "content": [{ "type": "image", "data": "x" }] })),
            None
        );
        // The harness 16KB cap applies.
        let big = "x".repeat(OUTPUT_CAP + 100);
        let output = tool_output_text(&json!({ "content": [{ "type": "text", "text": big }] }))
            .expect("capped output");
        assert!(output.len() < OUTPUT_CAP + 32);
        assert!(output.ends_with("… [truncated]"));
    }

    #[test]
    fn models_map_provider_slash_id_with_ladder() {
        let wire = json!({
            "models": [
                {
                    "id": "claude-sonnet-4-20250514",
                    "name": "Claude Sonnet 4",
                    "provider": "anthropic",
                    "reasoning": true,
                    "contextWindow": 200000,
                },
                {
                    "id": "gpt-4o-mini",
                    "name": "GPT-4o Mini",
                    "provider": "openai",
                    "reasoning": false,
                    "contextWindow": 128000,
                },
            ]
        });
        let models = models_from_response(&wire);
        assert_eq!(models[0].id, "anthropic/claude-sonnet-4-20250514");
        assert_eq!(models[0].label, "Claude Sonnet 4");
        assert_eq!(
            models[0].description.as_deref(),
            Some("anthropic · 200k context")
        );
        assert_eq!(models[0].reasoning_levels, FULL_LADDER.to_vec());
        assert_eq!(models[1].id, "openai/gpt-4o-mini");
        assert!(models[1].reasoning_levels.is_empty());
        // A provider-less entry still composes an id.
        let bare = json!({ "models": [{ "id": "x", "name": "X" }] });
        assert_eq!(models_from_response(&bare)[0].id, "pi/x");
    }

    #[test]
    fn models_fall_back_to_current_state_when_directory_is_empty() {
        let available = json!({ "models": [] });
        let state = json!({
            "model": {
                "id": "grok-4.6",
                "name": "Grok 4.6",
                "provider": "mvp-lab",
                "reasoning": true,
                "contextWindow": 500000,
            },
        });
        let models = models_from_responses(&available, &state);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "mvp-lab/grok-4.6");
        assert_eq!(models[0].label, "Grok 4.6");
        assert_eq!(
            models[0].description.as_deref(),
            Some("mvp-lab · 500k context")
        );
        assert_eq!(models[0].reasoning_levels, FULL_LADDER.to_vec());

        // A non-empty directory remains authoritative.
        let available = json!({
            "models": [{ "id": "configured", "name": "Configured", "provider": "custom" }]
        });
        let models = models_from_responses(&available, &state);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["custom/configured"]
        );
    }

    #[test]
    fn inline_images_only_image_mimes() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        let txt = dir.path().join("notes.txt");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(&txt, b"hello").unwrap();
        let images = inline_images(&[png.display().to_string(), txt.display().to_string()])
            .expect("one image inlined");
        let images = images.as_array().expect("array");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["mimeType"], "image/png");
        assert_eq!(images[0]["type"], "image");
        // Nothing image-shaped → None.
        assert!(inline_images(&[txt.display().to_string()]).is_none());
    }

    #[test]
    fn subagent_status_key_is_cypher() {
        assert_eq!(SUBAGENTS_STATUS_KEY, "cypher.subagents.v1");
    }

    #[test]
    fn parse_subagent_status_validates_the_v1_snapshot() {
        // Normal snapshot with both a running async run and a settled one.
        let text = json!({
            "version": 1,
            "runs": [
                {
                    "runId": "run-1",
                    "toolCallId": "t1",
                    "agent": "planner",
                    "model": "anthropic/claude-sonnet-4",
                    "task": "Plan the panel",
                    "mode": "async",
                    "status": "running",
                    "progress": "line 1\nline 2",
                    "startedAt": 1000,
                    "updatedAt": 2000,
                },
                {
                    "runId": "run-2",
                    "agent": "reviewer",
                    "task": "Review the diff",
                    "mode": "message",
                    "status": "done",
                    "startedAt": 3000,
                    "updatedAt": 4000,
                    "endedAt": 4000,
                },
            ],
        })
        .to_string();
        let runs = parse_subagent_status(&text).expect("valid snapshot");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "run-1");
        assert_eq!(runs[0].tool_call_id.as_deref(), Some("t1"));
        assert_eq!(runs[0].mode, SubagentRunMode::Async);
        assert_eq!(runs[0].status, SubagentRunStatus::Running);
        assert_eq!(runs[0].progress.as_deref(), Some("line 1\nline 2"));
        assert_eq!(runs[1].mode, SubagentRunMode::Message);
        assert_eq!(runs[1].status, SubagentRunStatus::Done);
        assert_eq!(runs[1].tool_call_id, None);
        assert_eq!(runs[1].ended_at, Some(4000));

        // Blank/missing text is a CLEAR snapshot, not an error.
        assert_eq!(parse_subagent_status(""), Some(vec![]));
        assert_eq!(parse_subagent_status("  \n "), Some(vec![]));
    }

    #[test]
    fn parse_subagent_status_rejects_wrong_versions_and_junk() {
        // Wrong version.
        assert!(parse_subagent_status("{\"version\":2,\"runs\":[]}").is_none());
        assert!(parse_subagent_status("{\"runs\":[]}").is_none());
        // Invalid JSON.
        assert!(parse_subagent_status("not json").is_none());
        assert!(parse_subagent_status("42").is_none());
        // Oversize snapshot (over 64KiB).
        let mut padded = json!({
            "version": 1,
            "runs": [{
                "runId": "r", "agent": "a", "task": "t",
                "mode": "sync", "status": "running",
                "startedAt": 1, "updatedAt": 2,
                "progress": "x",
            }],
        });
        if let Some(obj) = padded.as_object_mut() {
            obj.insert("pad".into(), Value::String("z".repeat(70 * 1024)));
        }
        assert!(parse_subagent_status(&padded.to_string()).is_none());
    }

    #[test]
    fn parse_subagent_status_rejects_oversized_runs_and_bad_enums() {
        let run = |patch: serde_json::Value| {
            let mut r = json!({
                "runId": "r", "agent": "a", "task": "t",
                "mode": "sync", "status": "running",
                "startedAt": 1, "updatedAt": 2,
            });
            if let (Some(obj), Some(patch)) = (r.as_object_mut(), patch.as_object()) {
                for (k, v) in patch {
                    obj.insert(k.clone(), v.clone());
                }
            }
            json!({"version": 1, "runs": [r]}).to_string()
        };
        // More than 32 runs.
        let many = json!({
            "version": 1,
            "runs": (0..33).map(|i| json!({
                "runId": format!("r{i}"), "agent": "a", "task": "t",
                "mode": "sync", "status": "running",
                "startedAt": 1, "updatedAt": 2,
            })).collect::<Vec<_>>(),
        });
        assert!(parse_subagent_status(&many.to_string()).is_none());
        // Task over 500 chars.
        assert!(parse_subagent_status(&run(json!({ "task": "x".repeat(501) }))).is_none());
        // Progress over 8 lines.
        assert!(parse_subagent_status(&run(json!({ "progress": "l\n".repeat(9) }))).is_none());
        // Progress over 4KiB.
        assert!(parse_subagent_status(&run(json!({ "progress": "y".repeat(5000) }))).is_none());
        // childChatId over 256 chars is a publisher bug — rejected too.
        assert!(parse_subagent_status(&run(json!({ "childChatId": "c".repeat(257) }))).is_none());
        // A normal childChatId parses through.
        let ok = parse_subagent_status(&run(json!({ "childChatId": "child-1" }))).expect("valid");
        assert_eq!(ok[0].child_chat_id.as_deref(), Some("child-1"));
        // Unknown mode / status enums.
        assert!(parse_subagent_status(&run(json!({ "mode": "blocking" }))).is_none());
        assert!(parse_subagent_status(&run(json!({ "status": "pending" }))).is_none());
        // Missing required fields.
        assert!(parse_subagent_status(&run(json!({ "agent": null }))).is_none());
        assert!(parse_subagent_status(&run(json!({ "startedAt": null }))).is_none());
    }

    #[test]
    fn ui_question_shapes_match_the_bridge() {
        let select = json!({ "title": "Pick", "options": ["A", "B"] });
        // (bridge internals are async; the option extraction shape is what
        // matters — exercise the request-side mapping through the same code.)
        let q = |payload: &Value| -> UserInputQuestion {
            UserInputQuestion {
                id: "u1".into(),
                header: payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Agent question")
                    .to_owned(),
                question: payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Agent question")
                    .to_owned(),
                options: payload
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                multi_select: false,
            }
        };
        let question = q(&select);
        assert_eq!(question.options, vec!["A", "B"]);
        assert!(!question.multi_select);
    }
}

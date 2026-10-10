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
//! Event mapping (see the table in `docs/design/pi-rpc.md`): text/thinking
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
mod discovery;
pub mod fork;
mod models;
mod session;
mod slash;
mod spawn;
mod throughput;
mod wire;

use discovery::synthesize_commands;
use models::{
    DiscoveredModels, MODEL_CATALOG_POLL, MODEL_CATALOG_STABLE, catalog_covers,
    expected_model_providers, models_from_response, models_from_responses,
};
use session::*;
use spawn::{resolve_executable, write_temp_prompt};
use wire::{
    INPUT_TRANSLATION_STATUS_KEY, SUBAGENTS_STATUS_KEY, TRANSLATION_STATUS_KEY, mcp_server_names,
    parse_input_translation_status, parse_subagent_status, parse_translation_status, pi_typed_call,
    tool_output_text, without_codemode_header,
};

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
    SlashCommand, SteeringMode, UserInputQuestion,
};

use crate::pi::client::{Incoming, PiClient};
use crate::process::{Signal, crash_message, send_signal, shutdown_child};
use crate::{Harness, HarnessError, RunControls, RunHostContext, parse_commands};

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

/// Rotate the assistant message id; returns (previous, next).
fn rotate(id: &mut String) -> (String, String) {
    let prev = std::mem::replace(id, new_message_id());
    (prev, id.clone())
}

async fn send(tx: &mpsc::Sender<Result<AgentEvent, HarnessError>>, ev: AgentEvent) -> bool {
    tx.send(Ok(ev)).await.is_ok()
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
        if let Some(models) = crate::lock(&self.models_cache).clone() {
            return Ok(models);
        }
        let discovered = self.discover_models().await?;
        if discovered.from_catalog && !discovered.models.is_empty() {
            *crate::lock(&self.models_cache) = Some(discovered.models.clone());
        }
        Ok(discovered.models)
    }

    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        if let Some(discovered) = self.cached_commands() {
            return Ok(synthesize_commands(&discovered));
        }
        let discovered = self.discover_commands().await?;
        *crate::lock(&self.commands) = Some(discovered.clone());
        Ok(synthesize_commands(&discovered))
    }

    fn invalidate_discovery(&self) {
        *crate::lock(&self.commands) = None;
        *crate::lock(&self.models_cache) = None;
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

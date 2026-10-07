//! ACP harness: spawns an Agent Client Protocol agent (JSON-RPC 2.0 over
//! stdio, protocol v1) and maps its session updates onto [`AgentEvent`]s. One
//! implementation covers every ACP agent; [`AcpHarness::grok`] configures it
//! for xAI's Grok Build (`grok agent stdio`), the first registered agent —
//! [`AcpHarness::hermes`] (Nous Research, `hermes acp`) and
//! [`AcpHarness::cursor`] (`cursor-agent acp`) followed. (pi moved off ACP
//! onto its native RPC harness — see `crates/harness/src/pi/`.)
//!
//! - `initialize` (protocolVersion 1, fs/terminal capabilities declined) →
//!   `session/new`, or `session/load` with a fresh-session fallback when
//!   resuming; replayed history during a load is dropped (the doc already
//!   holds it).
//! - `session/prompt` owns the turn: its response's `stopReason` ends the
//!   turn (`cancelled` → Interrupted, `refusal` → Errored, else Completed).
//! - `session/update` notifications normalize per [`normalize::map_update`]:
//!   message/thought chunks, tool calls with capped inline output + diffs,
//!   plans → Todo, `available_commands_update` → [`AgentEvent::AvailableCommands`].
//! - Permission requests are auto-accepted with the agent's preferred allow
//!   option — parity with the claude/codex harnesses' unattended yolo mode.
//! - Cursor's `cursor/*` extension methods get answered rather than refused:
//!   `ask_question` and `create_plan` BLOCK the turn until the client replies,
//!   so an unanswered one wedges the run. `ask_question` routes to the input
//!   bridge, `create_plan` auto-accepts, and todos render as a chip.
//! - Steering: agents advertising the `_session/steering` extension
//!   (`initialize._meta.steering.supported`) get mid-turn injection; others
//!   (Grok today) queue steers and deliver them as the next `session/prompt`
//!   at the turn boundary. The session stays parked between turns while the
//!   steering mailbox lives, like the codex harness.
//! - Interrupt: `session/cancel`, escalating SIGTERM → SIGKILL; the stream
//!   always ends with `Done { status: Interrupted }`.

mod cursor;
mod models;
pub(crate) mod normalize;
mod session;
mod specs;

use models::*;
use session::*;
pub use specs::*;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ModelOption, ModelOptionChoice, ReasoningLevel,
    RunRequest, SlashCommand, SteeringMode, UserInputAnswer, UserInputQuestion,
};

use crate::jsonrpc::{Incoming, RpcClient};
use crate::{Harness, HarnessError, RunControls, Signal, send_signal, shutdown_child};
use normalize::{cursor_todo_events, map_update, parse_commands, preferred_allow_option};

/// A resolved launch: a concrete program, or a managed npm adapter that may
/// still need installing (see [`AcpHarness::resolve_program`]).
enum Launch {
    Program(PathBuf, Vec<String>),
    Managed {
        pin: crate::adapter_install::NpmPin,
        bin_name: &'static str,
        args: Vec<String>,
    },
}

/// The ACP harness. Construct with [`AcpHarness::grok`]; tests point it at a
/// fake agent with [`AcpHarness::with_executable`].
pub struct AcpHarness {
    spec: AcpAgentSpec,
    executable: Option<PathBuf>,
    /// Grace between `session/cancel` and SIGTERM.
    interrupt_grace: Duration,
    /// Grace between SIGTERM and SIGKILL.
    kill_grace: Duration,
    /// Bound on the initialize → session handshake; a hang past it errors the
    /// run instead of spinning "Working" forever.
    handshake_timeout: Duration,
    /// Discovery result cache: the advertised commands survive across calls.
    commands: tokio::sync::OnceCell<Vec<SlashCommand>>,
    /// Model discovery cache: only a successful, non-empty probe is cached,
    /// so a mis-authed agent retries on the next picker open.
    models_cache: tokio::sync::OnceCell<Vec<Model>>,
}

impl AcpHarness {
    fn with_spec(spec: AcpAgentSpec) -> Self {
        Self {
            spec,
            executable: None,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(3),
            // Generous: the handshake is local work for every agent
            // (session/load replays from disk), so a hang past this is a
            // wedged agent, not a slow one.
            handshake_timeout: Duration::from_secs(120),
            commands: tokio::sync::OnceCell::new(),
            models_cache: tokio::sync::OnceCell::new(),
        }
    }

    /// Claude Code over ACP — the org-maintained `claude-agent-acp` adapter
    /// on the Claude Agent SDK.
    pub fn claude() -> Self {
        Self::with_spec(claude_spec())
    }

    /// Codex over ACP — the org-maintained `codex-acp` adapter wrapping the
    /// codex app-server.
    pub fn codex() -> Self {
        Self::with_spec(codex_spec())
    }

    /// Cursor Agent (`cursor-agent acp`) — Cursor's native ACP server.
    pub fn cursor() -> Self {
        Self::with_spec(cursor_spec())
    }

    /// Grok Build (`grok agent stdio`) — xAI's native ACP agent.
    pub fn grok() -> Self {
        Self::with_spec(grok_spec())
    }

    /// Hermes Agent (`hermes acp`) — Nous Research's native ACP server.
    pub fn hermes() -> Self {
        Self::with_spec(hermes_spec())
    }

    /// Use a fixed agent binary instead of PATH/known-location resolution.
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

    /// Test seam: the program `run` would spawn (the adapter binary, or —
    /// for a managed npm adapter — its installed entry, else npm as the
    /// installer that would run first).
    #[doc(hidden)]
    pub fn launch_program(&self) -> Result<PathBuf, HarnessError> {
        match self.resolve_launch()? {
            Launch::Program(program, _) => Ok(program),
            Launch::Managed { pin, bin_name, .. } => {
                match crate::adapter_install::installed_entry(&pin, bin_name) {
                    Some(entry) => Ok(entry),
                    None => crate::adapter_install::find_npm()
                        .ok_or_else(|| HarnessError::NotInstalled(self.spec.install_hint.into())),
                }
            }
        }
    }

    /// Resolve what to spawn: an explicit/installed adapter binary, or the
    /// managed install of the spec's pinned npm package. `NotInstalled` only
    /// when neither the binary nor the machinery to install it (npm) exists.
    fn resolve_launch(&self) -> Result<Launch, HarnessError> {
        let spec_args: Vec<String> = self.spec.args.iter().map(|a| a.to_string()).collect();
        if let Some(p) = &self.executable {
            return Ok(Launch::Program(p.clone(), spec_args));
        }
        if let Some(p) = std::env::var_os(self.spec.env_override)
            && !p.is_empty()
        {
            return Ok(Launch::Program(PathBuf::from(p), spec_args));
        }
        if let Some(found) = find_on_paths(self.spec.executable, (self.spec.extra_paths)()) {
            return Ok(Launch::Program(found, spec_args));
        }
        if let Some(pkg) = self.spec.npm_package {
            let pin = crate::adapter_install::NpmPin::parse(pkg);
            if crate::adapter_install::installed_entry(&pin, self.spec.executable).is_some()
                || crate::adapter_install::find_npm().is_some()
            {
                return Ok(Launch::Managed {
                    pin,
                    bin_name: self.spec.executable,
                    args: spec_args,
                });
            }
        }
        Err(HarnessError::NotInstalled(self.spec.install_hint.into()))
    }

    /// Resolve to a concrete (program, args), running the managed install if
    /// it hasn't completed yet. `block_on_install: false` (discovery paths)
    /// never waits on npm: it kicks the install in the background and errors
    /// out, so a picker open falls back to the static catalog instead of
    /// stalling for however long a 500MB dependency tree takes to land.
    async fn resolve_program(
        &self,
        block_on_install: bool,
    ) -> Result<(PathBuf, Vec<String>), HarnessError> {
        match self.resolve_launch()? {
            Launch::Program(program, args) => Ok((program, args)),
            Launch::Managed {
                pin,
                bin_name,
                args,
            } => {
                let entry = match crate::adapter_install::installed_entry(&pin, bin_name) {
                    Some(entry) => entry,
                    None if block_on_install => {
                        crate::adapter_install::ensure_installed(
                            pin,
                            bin_name,
                            self.spec.display_name,
                        )
                        .await?
                    }
                    None => {
                        let display_name = self.spec.display_name;
                        tokio::spawn(async move {
                            if let Err(e) = crate::adapter_install::ensure_installed(
                                pin,
                                bin_name,
                                display_name,
                            )
                            .await
                            {
                                tracing::warn!(
                                    target: "cypher_harness::adapter_install",
                                    "background adapter install failed: {e}"
                                );
                            }
                        });
                        return Err(HarnessError::Protocol(format!(
                            "{} adapter is installing in the background",
                            self.spec.display_name
                        )));
                    }
                };
                let (program, mut node_args) = crate::adapter_install::launch_for_entry(&entry)?;
                node_args.extend(args);
                Ok((program, node_args))
            }
        }
    }

    async fn spawn_agent(
        &self,
        cwd: Option<&str>,
        block_on_install: bool,
    ) -> Result<(Child, crate::StderrTail), HarnessError> {
        let (exe, args) = self.resolve_program(block_on_install).await?;
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        crate::compose_child_path(&mut cmd, &exe);
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
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
                    tracing::debug!(target: "cypher_harness::acp", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }
        Ok((child, stderr_tail))
    }

    /// Short-lived discovery run for [`Harness::commands`]: initialize, scan
    /// the response, then try one unauthenticated `session/new` and wait
    /// briefly for `available_commands_update`. Best-effort — an agent that
    /// refuses sessions before login still surfaces whatever the handshake
    /// advertised.
    async fn discover_commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        let (mut child, _stderr) = self.spawn_agent(None, false).await?;
        let (client, mut incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let discovery = async {
            let init = client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            let mut commands = scan_available_commands(&init);
            if commands.is_empty() {
                let cwd = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                let session = client
                    .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
                    .await;
                if session.is_ok() {
                    // The update usually arrives within milliseconds of the
                    // session response; 2s bounds a quiet agent.
                    let deadline = tokio::time::sleep(Duration::from_secs(2));
                    tokio::pin!(deadline);
                    loop {
                        tokio::select! {
                            inc = incoming.recv() => match inc {
                                Some(Incoming::Notification { method, params })
                                    if method == "session/update" =>
                                {
                                    let update = params.get("update").cloned().unwrap_or(Value::Null);
                                    if update.get("sessionUpdate").and_then(Value::as_str)
                                        == Some("available_commands_update")
                                    {
                                        commands = parse_commands(update.get("availableCommands"));
                                        break;
                                    }
                                }
                                Some(Incoming::Request { id, .. }) => {
                                    client.respond_error(&id, -32601, "unsupported during discovery");
                                }
                                Some(_) => {}
                                None => break,
                            },
                            _ = &mut deadline => break,
                        }
                    }
                }
            }
            Ok::<Vec<SlashCommand>, HarnessError>(commands)
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("command discovery timed out".into())),
        }
    }

    /// One short-lived probe for the agent's real model list: initialize →
    /// `session/new`, then read the response's first-class `models`
    /// (SessionModelState) with the `model` config option as fallback. The
    /// wire is the source of truth — the spec's static catalog only enriches
    /// matching entries and names the pick when the agent advertises nothing.
    async fn discover_models(&self) -> Result<Vec<Model>, HarnessError> {
        let (mut child, _stderr) = self.spawn_agent(None, false).await?;
        let (client, _incoming) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => RpcClient::new(stdin, stdout),
            _ => {
                shutdown_child(&mut child, self.kill_grace).await;
                return Err(HarnessError::Protocol("agent child has no stdio".into()));
            }
        };
        let discovery = async {
            client
                .request("initialize", initialize_params(self.spec.id))
                .await?;
            let cwd = std::env::var("HOME").unwrap_or_else(|_| "/".into());
            let session = client
                .request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))
                .await?;
            let mut models = models_from_session(&session, &(self.spec.models)());
            // Cursor: parameterized picker (base ids + optimize_for / effort /
            // fast) when the client opts in; exploded-variant fallback otherwise.
            if self.spec.id == HarnessId::Cursor {
                models = cursor::enrich_models(models, &session);
            }
            // Prompt-convention modes (Claude Ultrathink) extend any real
            // ladder — never an effort-less model's empty one.
            for model in &mut models {
                if !model.reasoning_levels.is_empty() {
                    for extra in self.spec.ladder_extras {
                        if !model.reasoning_levels.contains(extra) {
                            model.reasoning_levels.push(*extra);
                        }
                    }
                }
            }
            Ok::<Vec<Model>, HarnessError>(models)
        };
        let result = tokio::time::timeout(Duration::from_secs(10), discovery).await;
        shutdown_child(&mut child, self.kill_grace).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(HarnessError::Protocol("model discovery timed out".into())),
        }
    }
}

#[async_trait]
impl Harness for AcpHarness {
    fn id(&self) -> HarnessId {
        self.spec.id
    }
    fn display_name(&self) -> &str {
        self.spec.display_name
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        self.spec.steering_mode
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.spec.reasoning_levels
    }

    /// The agent's own CLI, not the adapter: `claude` counts as installed even
    /// when `claude-agent-acp` would arrive via npx, and an npx-reachable
    /// adapter does NOT count when the CLI itself is missing. Explicit
    /// executables (tests, `*_EXECUTABLE` overrides) always count.
    fn installed(&self) -> bool {
        if self.executable.is_some() {
            return true;
        }
        if std::env::var_os(self.spec.env_override).is_some_and(|v| !v.is_empty()) {
            return true;
        }
        find_on_paths(self.spec.cli_executable, (self.spec.cli_extra_paths)()).is_some()
    }

    /// ACP is the source of truth: a short-lived probe reads the agent's
    /// advertised model list (cached on success). The spec's static catalog
    /// answers when the agent advertises nothing or the probe fails — and an
    /// absent binary still surfaces as NotInstalled, like before.
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        self.resolve_launch()?;
        if let Some(models) = self.models_cache.get() {
            return Ok(models.clone());
        }
        match self.discover_models().await {
            Ok(models) if !models.is_empty() => {
                let _ = self.models_cache.set(models.clone());
                Ok(self.models_cache.get().cloned().unwrap_or(models))
            }
            Ok(_) | Err(_) => Ok((self.spec.models)()),
        }
    }

    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        self.commands
            .get_or_try_init(|| self.discover_commands())
            .await
            .cloned()
    }

    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (mut child, stderr_tail) = self.spawn_agent(Some(&request.cwd), true).await?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Protocol("agent child has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Protocol("agent child has no stdout".into()))?;
        let (client, incoming) = RpcClient::new(stdin, stdout);
        let (event_tx, event_rx) = mpsc::channel::<Result<AgentEvent, HarnessError>>(256);
        tokio::spawn(run_session(Session {
            child,
            client,
            incoming,
            event_tx,
            controls,
            request,
            harness: self.spec.id,
            agent_name: self.spec.display_name,
            prompt_transform: self.spec.prompt_transform,
            effort_values: self.spec.effort_values,
            interrupt_grace: self.interrupt_grace,
            kill_grace: self.kill_grace,
            handshake_timeout: self.handshake_timeout,
            stderr_tail,
        }));

        Ok(futures::stream::unfold(event_rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        })
        .boxed())
    }
}

#[cfg(test)]
mod tests;

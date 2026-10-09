//! cypher-harness — one interface over coding agents: the native Pi harness
//! (`pi --mode rpc`, the [`pi`] module) and a mock for tests and dev rigs.
//! Decision record: docs/research/pi-rpc.md.

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};
pub use tokio_util::sync::CancellationToken;

use cypher_proto::{
    AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SlashCommand, SteeringMode,
    UserInputAnswer, UserInputQuestion,
};

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness binary not found: {0}")]
    NotInstalled(String),
    /// The harness exists only as a decode-compatible id (a retired driver);
    /// the message is user-facing as-is.
    #[error("{0}")]
    Unsupported(String),
    #[error("harness protocol error: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A steer prompt pushed into a live run; delivered at the harness's steering boundary.
pub struct SteerMessage {
    pub prompt: String,
    pub message_id: Option<String>,
}

/// Private, ephemeral UI channel for settings commands. Never journal these
/// payloads: OAuth dialogs and responses may contain authorization codes.
pub struct SlashUi {
    /// `(id, payload)` of each dialog (`input`) and non-error `notify`.
    pub requests: mpsc::Sender<(String, serde_json::Value)>,
    pub responses: mpsc::Receiver<(String, serde_json::Value)>,
    pub cancel: CancellationToken,
}

/// Host-side run context: which chat this run belongs to and whether the
/// engine hosts it as a Cypher child subagent. A NON-SERIALIZED internal seam
/// (never rides the wire — `RunRequest` stays clean); the engine builds it
/// from the chat row at dispatch time. Discovery processes never see it (they
/// have no `RunControls`), so a parent id can never leak into a probe.
#[derive(Debug, Clone, Default)]
pub struct RunHostContext {
    /// The chat id this run belongs to (injected as `CYPHER_CHAT_ID` into the
    /// child pi process; the subagents extension publishes it in its
    /// `cypher.subagents.v1` projection as `childChatId`). Discovery processes
    /// never see it (they have no `RunControls`), so a parent id can never
    /// leak into a probe.
    pub chat_id: Option<String>,
    /// Cypher child-subagent env (present only for child chats): the persisted
    /// agent profile plus the messaging-channel identity for the initial run.
    pub child: Option<ChildRunEnv>,
}

/// The child-subagent runtime env the engine derives from the chat row's
/// persisted [`cypher_proto::ChildChat`] metadata + the engine-local channel
/// info. Bounded struct (never an arbitrary env map); the pi harness injects
/// it as env + CLI flags into the child pi process so the extension loads in
/// child mode (`PI_SUBAGENT_ROLE=child`) and registers the messaging tools.
/// The messaging channel is HOST-LOCAL and present only for the initial run:
/// `channel_root: None` (later child turns, restarts) means the child has no
/// message channel — the extension's messaging tools then report unavailable.
#[derive(Debug, Clone)]
pub struct ChildRunEnv {
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub channel_root: Option<String>,
    pub run_id: String,
    pub agent: String,
    pub child_index: u32,
}

/// Host-side controls handed to a run: input-request bridge + steering mailbox.
pub struct RunControls {
    /// The run sends questions and awaits answers (blocks the agent, mirrors cypher).
    pub request_input: Box<
        dyn Fn(Vec<UserInputQuestion>) -> oneshot::Receiver<Vec<UserInputAnswer>> + Send + Sync,
    >,
    /// Steer prompts consumed at step/turn boundaries.
    pub steering: mpsc::Receiver<SteerMessage>,
    /// Cancel to interrupt the live run: the harness sends its protocol-level
    /// interrupt, then escalates to SIGTERM/SIGKILL on the child after a grace
    /// period. The run's stream ends with `Done { status: Interrupted }`.
    pub interrupt: CancellationToken,
    /// Host-side run context (chat identity + child env). Non-serialized
    /// internal seam — see [`RunHostContext`].
    pub host: RunHostContext,
}

#[async_trait]
pub trait Harness: Send + Sync {
    fn id(&self) -> HarnessId;
    fn display_name(&self) -> &str;
    fn supports_steering(&self) -> bool;
    fn steering_mode(&self) -> SteeringMode;
    fn reasoning_levels(&self) -> &[ReasoningLevel];
    /// Whether the agent's own CLI is present on this device — the settings
    /// gate for enabling the harness. A filesystem probe, never a spawn.
    /// Defaults to true for harnesses without a CLI to check (mock).
    fn installed(&self) -> bool {
        true
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError>;
    /// Slash commands the agent advertises; empty
    /// for harnesses without them. May spawn a short-lived discovery process.
    async fn commands(&self) -> Result<Vec<SlashCommand>, HarnessError> {
        Ok(Vec::new())
    }
    /// Run one extension slash command in a short-lived child (no chat).
    /// Used by Settings → MCP so OAuth goes through the same Pi command path
    /// as the TUI. Default: unsupported.
    async fn run_slash(&self, _prompt: &str) -> Result<String, HarnessError> {
        Err(HarnessError::Protocol(
            "slash execution is unsupported for this harness".into(),
        ))
    }
    async fn run_slash_interactive(
        &self,
        _prompt: &str,
        _ui: SlashUi,
    ) -> Result<String, HarnessError> {
        Err(HarnessError::Protocol(
            "Interactive MCP sign-in requires an updated Pi harness.".into(),
        ))
    }
    /// Drop cached model/command discovery so the next probe reflects a
    /// changed agent config (Pi package enablement). No-op for harnesses
    /// without a cache.
    fn invalidate_discovery(&self) {}
    /// Run one (persistent) session; the stream ends with `AgentEvent::Done`.
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError>;

    /// Session Fork (v1) — Pi-only. Materialize a NEW persisted harness
    /// session for a boundary on the source session WITHOUT mutating the
    /// source session file or any live client. The result's session path is
    /// `None` for an EMPTY-CONTEXT fork before the first user (pi persists
    /// that file only when the first user message lands). The default
    /// implementation answers Unsupported.
    async fn fork_session(
        &self,
        _request: cypher_proto::PiSessionForkRequest,
    ) -> Result<cypher_proto::PiSessionForkResult, HarnessError> {
        Err(HarnessError::Protocol(
            "session fork is unsupported for this harness (Pi only in v1)".into(),
        ))
    }
}

pub mod mock;
pub mod pi;
pub mod shell_env;

/// Bin directories where npm-installed CLIs land under Node version managers.
/// GUI launches never see these on PATH — the managers shape PATH in shell
/// init (fnm's per-shell multishells, nvm's shell function), which a
/// Dock/Finder-launched app never runs.
pub(crate) fn node_version_manager_bins() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    // fnm: `aliases/default` is a stable symlink to the active default
    // installation (the multishell PATH entries are ephemeral, per-shell).
    let mut fnm_roots: Vec<PathBuf> = std::env::var_os("FNM_DIR")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(home) = &home {
        fnm_roots.push(home.join(".local").join("share").join("fnm"));
        fnm_roots.push(home.join("Library").join("Application Support").join("fnm"));
        fnm_roots.push(home.join(".fnm"));
    }
    for root in fnm_roots {
        dirs.push(root.join("aliases").join("default").join("bin"));
    }
    if let Some(home) = &home {
        // volta / bun keep real shims in a fixed bin dir; pnpm has a global bin.
        dirs.push(home.join(".volta").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join("Library").join("pnpm"));
        dirs.push(home.join(".local").join("share").join("pnpm"));
        // nvm: every installed version's bin, newest first.
        let nvm = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm) {
            let mut versions: Vec<PathBuf> =
                entries.flatten().map(|e| e.path().join("bin")).collect();
            versions.sort();
            versions.reverse();
            dirs.append(&mut versions);
        }
    }
    dirs
}

/// Fixed npm-global / Homebrew / `~/.local` bin dirs. `find_on_paths` looks
/// here after PATH + the login-shell snapshot; `compose_child_path` puts the
/// same dirs on the child so `spawn("npm")` can see what resolution saw.
pub(crate) fn well_known_cli_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".npm-global").join("bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Resolve a CLI the way every harness does: process PATH, the login-shell
/// PATH snapshot (zshrc/zprofile, including pnpm), well-known npm-global
/// bins, and node version-manager bins (fnm/nvm/volta/pnpm/bun).
pub fn resolve_cli(name: &str) -> Option<std::path::PathBuf> {
    find_on_paths(name, npm_global_bins(name))
}

/// PATH + login-shell + extra dirs + node-version-manager scan for a binary.
fn find_on_paths(exe: &str, extra: Vec<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe))
                .collect()
        })
        .unwrap_or_default();
    if let Some(shell_path) = shell_env::login_shell_path() {
        candidates.extend(
            std::env::split_paths(shell_path)
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.join(exe)),
        );
    }
    candidates.extend(extra);
    candidates.extend(node_version_manager_bins().into_iter().map(|d| d.join(exe)));
    candidates.into_iter().find(|p| p.exists())
}

/// `exe` inside each of the [`well_known_cli_dirs`].
fn npm_global_bins(exe: &str) -> Vec<std::path::PathBuf> {
    well_known_cli_dirs()
        .into_iter()
        .map(|dir| dir.join(exe))
        .collect()
}

/// Compose the child's PATH: the resolved executable's directory first, then
/// our own PATH, then the login-shell PATH snapshot, then the same well-known
/// and version-manager dirs `resolve_cli` searches — deduped. npm-shim CLIs
/// are `#!/usr/bin/env node` scripts whose `node` lives beside them in the
/// version manager's bin dir, and the CLIs themselves shell out to tools
/// (git, rg, node, npm) that a GUI/service launch's own PATH may lack.
pub fn compose_child_path(cmd: &mut tokio::process::Command, exe: &std::path::Path) {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        paths.push(dir.to_path_buf());
    }
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    if let Some(shell_path) = shell_env::login_shell_path() {
        paths.extend(std::env::split_paths(shell_path));
    }
    paths.extend(well_known_cli_dirs());
    paths.extend(node_version_manager_bins());
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
}

/// Rolling tail of a child's stderr, shared between the reader task and the
/// crash-message composer: an unexpected exit surfaces "<name> exited
/// unexpectedly (<status>): <last stderr lines>" instead of a bare shrug —
/// the proper background-crash message old cypher showed (user requirement).
#[derive(Clone, Default)]
pub(crate) struct StderrTail(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    const KEEP_LINES: usize = 6;
    const KEEP_BYTES: usize = 700;

    pub(crate) fn push(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tail.push_back(line.chars().take(Self::KEEP_BYTES).collect());
        while tail.len() > Self::KEEP_LINES {
            tail.pop_front();
        }
    }

    /// The captured tail as one display string, `None` when nothing arrived.
    pub(crate) fn snapshot(&self) -> Option<String> {
        let tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tail.is_empty() {
            return None;
        }
        let mut joined = tail.iter().cloned().collect::<Vec<_>>().join("\n");
        joined.truncate(Self::KEEP_BYTES * 2);
        Some(joined)
    }
}

/// "exit code 137" / "signal 9 (killed)" / "unknown" — the status half of a
/// crash message, from a `try_wait` result after the stream ended.
pub(crate) fn describe_exit(status: Option<std::process::ExitStatus>) -> String {
    let Some(status) = status else {
        return "still running".into();
    };
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    "unknown exit".into()
}

/// The full crash message: status plus the stderr tail when there is one.
pub(crate) fn crash_message(
    name: &str,
    status: Option<std::process::ExitStatus>,
    stderr: &StderrTail,
) -> String {
    let status = describe_exit(status);
    match stderr.snapshot() {
        Some(tail) => format!("{name} exited unexpectedly ({status}): {tail}"),
        None => format!("{name} exited unexpectedly ({status})"),
    }
}

/// Byte cap applied to tool output text at the harness boundary. The doc-side
/// fold applies its own (smaller) cap before anything persists; this one only
/// bounds what crosses the event stream.
pub(crate) const OUTPUT_CAP: usize = 16 * 1024;

/// Truncate on a char boundary, marking the cut so the UI can say "truncated".
pub(crate) fn cap_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    out.push_str("\n… [truncated]");
    out
}

/// Decode a slash-command array (`{name, description, input: {hint}}`);
/// nameless entries are dropped.
pub(crate) fn parse_commands(value: Option<&serde_json::Value>) -> Vec<SlashCommand> {
    use serde_json::Value;
    let str_field =
        |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    value
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let name = str_field(c, "name");
            (!name.is_empty()).then(|| SlashCommand {
                name,
                description: str_field(c, "description"),
                input_hint: c
                    .get("input")
                    .and_then(|i| i.get("hint"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Child lifecycle
// ---------------------------------------------------------------------------

/// Reap the child: graceful SIGTERM first, SIGKILL after `kill_grace`.
/// (`kill_on_drop` remains the last-resort backstop.)
pub(crate) async fn shutdown_child(
    child: &mut tokio::process::Child,
    kill_grace: std::time::Duration,
) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if let Some(pid) = child.id() {
        send_signal(pid, Signal::Term);
        if tokio::time::timeout(kill_grace, child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
pub(crate) fn send_signal(pid: u32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: plain kill(2) on a pid we spawned and have not yet reaped.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
pub(crate) fn send_signal(_pid: u32, _signal: Signal) {
    // No SIGTERM off unix; `start_kill`/`kill_on_drop` handle termination.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_with_hint() {
        let commands = serde_json::json!([
            { "name": "compact", "description": "Compact the session" },
            { "name": "goal", "description": "Set a goal", "input": { "hint": "the goal" } },
            { "description": "nameless is dropped" },
        ]);
        assert_eq!(
            parse_commands(Some(&commands)),
            vec![
                SlashCommand {
                    name: "compact".into(),
                    description: "Compact the session".into(),
                    input_hint: None,
                },
                SlashCommand {
                    name: "goal".into(),
                    description: "Set a goal".into(),
                    input_hint: Some("the goal".into()),
                },
            ]
        );
        assert!(parse_commands(None).is_empty());
    }
}

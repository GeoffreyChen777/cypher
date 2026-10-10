//! cypher-harness — one interface over coding agents: the native Pi harness
//! (`pi --mode rpc`, the [`pi`] module) and a mock for tests and dev rigs.
//! Protocol notes: docs/design/pi-rpc.md.

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
mod process;
pub mod shell_env;

pub use process::{compose_child_path, resolve_cli};

/// Lock a mutex, ignoring poisoning: every critical section here leaves its
/// data consistent, so a panic elsewhere must not cascade into the run.
pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

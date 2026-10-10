//! One live pi session: the state the run loop owns and the files that drive
//! it — `setup` (handshake, model selection, built-in slash commands), `run`
//! (the event loop and its agent-event arms), `steer` (mailbox routing) and
//! `ui` (extension dialogs and status projections).

mod run;
mod setup;
mod steer;
mod ui;

use super::*;

pub(super) use run::run_session;
pub(super) use setup::BuiltinIntercept;
#[cfg(test)]
pub(super) use {run::inline_images, setup::builtin_match, ui::ui_response_payload};

use steer::{NextTurn, RoutedSteer};

pub(super) struct Session {
    pub(super) child: Child,
    pub(super) client: PiClient,
    pub(super) incoming: mpsc::Receiver<Incoming>,
    pub(super) event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) controls: RunControls,
    pub(super) request: RunRequest,
    pub(super) interrupt_grace: Duration,
    pub(super) kill_grace: Duration,
    pub(super) handshake_timeout: Duration,
    pub(super) no_activity_grace: Duration,
    pub(super) liveness_probe_interval: Duration,
    pub(super) model_catalog_wait: Duration,
    pub(super) stderr_tail: crate::process::StderrTail,
    /// Which synthesized built-in commands this run intercepts (computed from
    /// the discovery cache at run start).
    pub(super) intercept: BuiltinIntercept,
    /// Temp file holding the child agent's persisted system prompt
    /// (`--append-system-prompt`), removed when the run ends.
    pub(super) temp_prompt: Option<PathBuf>,
    /// Configured MCP server names, read at run start, so MCP tool calls
    /// show the server under the name Settings uses ([`mcp_tool_parts`]).
    pub(super) mcp_servers: Vec<String>,
}

type RequestInputFn = Box<
    dyn Fn(
            Vec<UserInputQuestion>,
        ) -> tokio::sync::oneshot::Receiver<Vec<cypher_proto::UserInputAnswer>>
        + Send
        + Sync,
>;

/// What the main loop does after an arm.
enum Flow {
    Continue,
    Break,
}

/// The main loop's state: the run's fixed context plus everything the
/// `select!` arms mutate. One method per arm, each returning whether the run
/// goes on.
struct PiRun {
    client: PiClient,
    event_tx: mpsc::Sender<Result<AgentEvent, HarnessError>>,
    session_file: String,
    mcp_servers: Vec<String>,
    request_input: std::sync::Arc<RequestInputFn>,
    intercept: BuiltinIntercept,
    no_activity_grace: Duration,
    interrupt_grace: Duration,
    kill_grace: Duration,
    /// LIVENESS PROBE: after this long without a pi event mid-turn, ask pi
    /// whether it is still working ([`Self::liveness_watched`]). The engine's
    /// turn-quiesce watchdog only sees stream silence, and a silent pi is
    /// the normal shape of a long reasoning step, a cold prefill or a retry
    /// backoff — its heartbeat must keep arriving well inside the engine's
    /// shortest window, or a live turn is parked as if its Done were lost.
    liveness_interval: Duration,
    /// Fires [`Self::liveness_interval`] after the last pi event.
    liveness_at: std::pin::Pin<Box<tokio::time::Sleep>>,
    /// The in-flight `get_state` probe's id. Sent ordered, so its answer is
    /// read only after every event pi wrote ahead of it — an "idle" answer
    /// can never overtake the `agent_settled` it would contradict.
    liveness_probe: Option<String>,
    /// A run pi started on its own — a background task's wake, with no
    /// prompt outstanding (`in_turn` false). Its `agent_settled` closes it
    /// with a Done of its own instead of leaving the engine to guess from
    /// silence; a prompt dispatched meanwhile takes the run over.
    self_run: bool,
    assistant_message_id: String,
    /// The current assistant message's streamed text (Done's `result` and
    /// the error text for an `error` stopReason).
    last_assistant_text: String,
    /// The last assistant message's stopReason ("stop"/"length"/"error"/
    /// "aborted"); Completed for anything but error/aborted.
    last_stop_reason: String,
    last_error_message: Option<String>,
    interrupted: bool,
    interrupt_sent: bool,
    done_sent: bool,
    /// Live-progress throttle: toolCallId → last FORWARDED ToolProgress. A
    /// tool_execution_update is forwarded only when ≥[`PROGRESS_THROTTLE`]
    /// elapsed since the last forward for that id (first always forwards).
    /// `progress_ended` marks tools whose end we've seen — late/duplicate
    /// updates after end are dropped (the doc fold would ignore them anyway
    /// once resolved; this stops the harness from even emitting them).
    progress_last: HashMap<String, Instant>,
    progress_ended: HashSet<String>,
    /// The working trailer's tok/s, estimated from the streamed deltas.
    throughput: throughput::ThroughputMeter,
    /// False until the FIRST agent event of any kind arrives. While it stays
    /// false the run is proven inert (no agent activity, e.g. an extension
    /// command whose handler only notifies) and the `no_activity` timer ends it.
    agent_started: bool,
    in_turn: bool,
    steering_open: bool,
    /// Steers pi has QUEUED but not yet delivered (one per assistant message;
    /// pi's default steering mode is one-at-a-time). Texts are kept so a steer
    /// the turn settles before delivering can be retried as an idle prompt
    /// (an idle pi only QUEUES steers).
    steers_queued: VecDeque<String>,
    /// Routed messages an extension consumed mid-turn (`handled`). Each still
    /// owes the engine its Steered boundary: at the next assistant message (the
    /// next step's output belongs below the message), or before the turn's Done.
    handled_steers: usize,
    /// The in-flight routed steer (its response arrives in order on
    /// `incoming`), plus followers awaiting their turn.
    steer_call: Option<RoutedSteer>,
    steer_backlog: VecDeque<String>,
    /// Whether this pi reports dispositions (≥ 0.99), learned from the first
    /// prompt's response — which always resolves before any mid-turn routing.
    /// Mid-turn messages then ride an atomic `prompt` instead of a raw `steer`.
    atomic_steer: bool,
    /// In-flight `prompt` RPC: the first turn starts here, and parked-turn
    /// restarts reuse the same slot. Serialized with steer calls (never both
    /// in flight); followers queue in `prompt_backlog` until the turn settles.
    idle_prompt: Option<BoxFuture<'static, Result<Value, HarnessError>>>,
    /// True if a blocking dialog (select/input/editor/confirm) or a notify
    /// landed while the prompt RPC was in flight. Real pi only ACKs extension
    /// commands after the handler returns, so an ACK with that UI and no agent
    /// lifecycle means the command is done — do not wait the 2s no-activity
    /// grace (that spin after closing a picker). Transient TUI furniture
    /// (`setStatus`/`setWidget`/`setTitle`/`set_editor_text`) never counts:
    /// the goal, MCP and subagents extensions push status updates at startup
    /// and mid-turn, and treating those as "UI happened" collapsed the grace
    /// to zero on ordinary prompts, so the harness Done'd the turn before the
    /// agent's first event.
    had_ui: bool,
    /// The zero-grace shortcut is for extension slash commands only: a plain
    /// prompt always starts an agent turn, so it keeps the full grace even if
    /// a real dialog fires during its preflight.
    /// Runtimes without dispositions only leave the slash prefix to go on.
    prompt_is_command: bool,
    prompt_backlog: VecDeque<NextTurn>,
    /// Interrupt escalation: abort, then SIGTERM → SIGKILL if the agent
    /// doesn't wind down.
    escalation: Option<tokio::task::JoinHandle<()>>,
    /// Started when the main loop begins (right after the prompt was accepted):
    /// if pi never emits a single agent event, the run terminates with
    /// Done{Completed} rather than sit "Working" forever. Any agent-lifecycle
    /// event disarms it (informational events do not). Late sendMessage work
    /// arriving after this fires is dropped — a documented degradation. A
    /// parked-turn restart re-arms it (a fresh sleep) only once its prompt is
    /// ACCEPTED.
    no_activity: std::pin::Pin<Box<tokio::time::Sleep>>,
}

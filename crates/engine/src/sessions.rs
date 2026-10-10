//! SessionsEngine — per-chat agent runs: dispatch, steering, interrupts, input bridging,
//! journal + broadcast fan-out, and 120ms coalesced doc streaming.
//!
//! Pragmatic port of zeron's `sessions.ts`:
//! - every `AgentEvent` is (a) appended to the on-disk run journal, (b) broadcast to
//!   in-process subscribers, (c) folded via `fold_event_into_parts` and diffed into the
//!   chat's `SessionDoc` through `SegmentWriter` on a coalesced `STREAM_COMMIT_MS` timer;
//! - the user message entry is pushed to the doc immediately on dispatch (id = the
//!   command's client-minted message id, so optimistic echoes never flicker);
//! - a `Steered` event splits the assistant entry at the exact boundary;
//! - recovery (interrupt or a stale journal at boot) stamps the streaming entry `aborted`.
//!
//! Scope notes: sessions are keyed by chat id (one live run per chat). Zeron's pulse
//! loop is ported as the 15s liveness heartbeat in `drive_run`; its stall watchdog is
//! deliberately NOT ported (rejected in review — agents may legitimately wait on
//! something for far longer than any timeout, and a live child IS the working signal).
//! Every dying path must instead carry its own visible error (child crash with stderr,
//! spawn failure, stream error, engine-restart recovery).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use chrono::Utc;
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use cypher_doc::{
    DocError, MessageComment, MessagePart, MessageRole, MessageStatus, STREAM_COMMIT_MS,
    SegmentWriter, SessionDoc, fold_event_into_parts, sanitize_tool_call,
};
use cypher_harness::{
    CancellationToken, ChildRunEnv, Harness, RunControls, RunHostContext, SteerMessage,
};
use cypher_proto::{
    AgentEvent, AnsweredModel, ChatConfig, ContextUsage, DoneStatus, HarnessId, ReasoningLevel,
    RunRequest, Session, SessionStatus, SubagentRun, SubagentRunStatus, Throughput,
    UserInputAnswer, UserInputQuestion,
};

use crate::doc_host::{ChatDocHandle, DocHost};
use crate::registry::HarnessRegistry;
use crate::run_journal::RunJournal;
use crate::{EngineError, new_id, now_ms};

mod run_task;
use run_task::*;

/// One journaled event: the durable seq plus the event, as broadcast to subscribers.
#[derive(Debug, Clone)]
pub struct JournaledEvent {
    pub seq: u64,
    pub event: AgentEvent,
}

/// Outcome of a steer attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// Delivered into the live run's steering mailbox.
    Accepted,
    /// No live steerable run — the caller should dispatch the prompt as a new turn.
    NotSteerable,
}

type PendingInputs = Arc<Mutex<HashMap<String, oneshot::Sender<Vec<UserInputAnswer>>>>>;

/// The model settings a run's harness process was launched with. A parked
/// (persistent) process keeps them for its whole life, so a turn asking for
/// different ones must not be routed into it — see
/// [`SessionsEngine::retire_stale_run`].
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchConfig {
    pub harness: HarnessId,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    pub model_options: serde_json::Map<String, serde_json::Value>,
}

impl LaunchConfig {
    pub fn of_request(harness: HarnessId, request: &RunRequest) -> Self {
        Self {
            harness,
            model: request.model.clone(),
            reasoning: request.reasoning,
            model_options: request.model_options.clone(),
        }
    }

    pub fn of_chat(config: &ChatConfig) -> Self {
        Self {
            harness: config.harness,
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
        }
    }

    /// Stamp these settings onto a request built from an older config.
    pub fn apply_to(&self, request: &mut RunRequest) {
        request.harness = Some(self.harness);
        request.model = self.model.clone();
        request.reasoning = self.reasoning;
        request.model_options = self.model_options.clone();
    }
}

/// A harness-native session id plus the cwd it was created under. Harness
/// session stores are cwd-scoped (claude keys conversations by project
/// directory — zeron sessions.ts:563 "harness session stores are keyed by
/// cwd"), so resume is only injected for runs launched from the same cwd.
#[derive(Debug, Clone)]
struct HarnessSessionRef {
    session_id: String,
    cwd: String,
}

struct RunHandle {
    run_id: String,
    steerable: bool,
    steer_tx: mpsc::Sender<SteerMessage>,
    /// Harness-level cancellation (protocol interrupt + child teardown).
    interrupt_token: CancellationToken,
    /// Engine-level cancel: arms the run task's grace deadline so a harness that
    /// ignores its token can never strand the run.
    cancel: watch::Sender<bool>,
    engine_tx: mpsc::UnboundedSender<AgentEvent>,
    pending_inputs: PendingInputs,
    /// Steers accepted into the mailbox but not yet confirmed by a `Steered`
    /// event — the at-least-once ledger. A run can die with accepted steers
    /// still in its mailbox (idle reaper vs. a routed send; a mid-turn error
    /// discarding queued boundary steers): the run task drains this at exit
    /// and re-dispatches each entry as a fresh turn, so an accepted message
    /// can never silently evaporate from a transcript that shows it as sent.
    routed_steers: Arc<Mutex<std::collections::VecDeque<RoutedSteer>>>,
    /// What the harness process was launched with (routed turns reuse it).
    launch: LaunchConfig,
}

/// One accepted-but-unconfirmed steer: enough to re-dispatch it verbatim.
/// `prompt` is the VISIBLE prompt (the doc user entry); `agent_prompt` the
/// optional EFFECTIVE override the harness should receive.
#[derive(Debug, Clone)]
struct RoutedSteer {
    prompt: String,
    agent_prompt: Option<String>,
    message_id: String,
}

struct Inner {
    device_id: String,
    journal: Arc<RunJournal>,
    registry: Arc<HarnessRegistry>,
    /// Set-once (first wins), cleared on runtime retirement: sessions and
    /// doc-host reference each other through Arcs, so this back-edge must be
    /// severable for a replaced engine graph to drop.
    doc_host: Mutex<Option<DocHost>>,
    /// chat_id → live run.
    runs: Mutex<HashMap<String, RunHandle>>,
    /// chat_id → broadcast hub (retained across runs so subscribers survive turns).
    hubs: Mutex<HashMap<String, broadcast::Sender<JournaledEvent>>>,
    statuses: Mutex<HashMap<String, Session>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    /// Host-LOCAL messaging channel identity for child chats, keyed by child
    /// chat id. The channel root is an absolute host-local path
    /// (`/tmp/pi-subagents-messages/<session>`); it is NEVER persisted or
    /// synced (stale after a reboot, outside the workspace sync boundary). An
    /// entry lives only long enough for the initial queued run — registered
    /// by `StartSubagent` and CONSUMED at first dispatch. Bounded: capped at
    /// [`MAX_LOCAL_CHILD_CHANNELS`], and removed on child-chat delete/rollback.
    child_channels: Mutex<HashMap<String, LocalChildChannel>>,
    /// Last dispatched request per chat — the steer→new-turn fallback re-derives its
    /// run config from this (chat config rows land with the workspace doc in M4).
    last_requests: Mutex<HashMap<String, RunRequest>>,
    /// Harness-native session ids per chat (resume continuity across turns) —
    /// the live-process cache over the durable copy on the workspace chat row
    /// (zeron kept the same pair on `chats.harness_session_id`). An empty
    /// session id is the "do not resume" tombstone after a rejected resume.
    harness_sessions: Mutex<HashMap<String, HarnessSessionRef>>,
    /// Auto-titler for untitled chats (wired at engine assembly; absent in bare tests).
    titles: OnceLock<crate::titles::TitleGenerator>,
    /// Fired with `(chat_id, cwd)` when a user prompt starts a turn (fresh
    /// dispatch or accepted steer) — the diff sync snapshots the checkout tree
    /// for the Changes pane's "Latest turn" scope. Absent in bare tests.
    turn_listener: OnceLock<TurnListener>,
    /// Temporary Side Chat ids: in-memory status + harness-session
    /// continuity ONLY. Everything that would make a chat observable or durable
    /// — the public `sessions_tx` watch, workspace session rows, the run
    /// journal, the auto-titler, turn-start snapshots, and persistent
    /// harness-session row writes — is suppressed for these ids. Registered by
    /// the side-chat manager on start, removed on promote (statuses then flow
    /// through the public paths) or dispose.
    ephemeral: Mutex<HashSet<String>>,
    /// Private status watch per ephemeral chat — the Side Chat panel's only
    /// status channel while temporary (public WatchSessions never sees these).
    /// The Sender is retained here so late subscribers still see the last
    /// transition; entries are removed on promote/dispose.
    ephemeral_tx: Mutex<HashMap<String, watch::Sender<Option<Session>>>>,
    /// Bumped when Pi packages change. A live turn that started on an older
    /// epoch does not park: the next send respawns against the new config.
    plugin_epoch: AtomicU64,
    /// Fixed watchdog windows; unset reads [`QuiesceWindows::from_env`] per run.
    quiesce: OnceLock<QuiesceWindows>,
}

/// The turn-quiesce watchdog's silence windows (see the run task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuiesceWindows {
    /// Silence after completed output that parks a turn; `None` disables the
    /// watchdog. Default 5min: long silent thinking with no reasoning events
    /// must not drop the spinner.
    pub turn: Option<std::time::Duration>,
    /// The shorter window for a SELF-CONTINUED turn. A turn the agent starts
    /// on its own (background-task wake) never receives a turn-end Done: no
    /// prompt is outstanding to settle. The watchdog is that turn shape's
    /// ONLY settle path, so the normal window read as minutes of
    /// stuck-Working after every background notification. The in-flight fold
    /// gate still protects running tools; reasoning heartbeats push the
    /// window during real thinking. `None` falls back to `turn`. Default 20s.
    pub self_turn: Option<std::time::Duration>,
}

impl QuiesceWindows {
    /// `CYPHER_TURN_QUIESCE_MS` and `CYPHER_SELF_TURN_QUIESCE_MS` override the
    /// defaults; 0 disables (an explicit `CYPHER_TURN_QUIESCE_MS=0` disables
    /// the watchdog entirely).
    pub fn from_env() -> Self {
        let window = |name: &str, default: std::time::Duration| match cypher_env::var(name)
            .and_then(|v| v.parse::<u64>().ok())
        {
            Some(0) => None,
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            None => Some(default),
        };
        Self {
            turn: window("TURN_QUIESCE_MS", std::time::Duration::from_secs(300)),
            self_turn: window("SELF_TURN_QUIESCE_MS", std::time::Duration::from_secs(20)),
        }
    }
}

/// Host-local messaging channel for one child chat's INITIAL run (see
/// [`Inner::child_channels`]). Not serialized, never synced.
#[derive(Debug, Clone)]
pub struct LocalChildChannel {
    pub channel_root: String,
    pub child_index: u32,
    /// The run's messaging address when it differs from the agent name (the
    /// extension mints `planner#1a2b3c4d` for a second concurrent planner).
    /// The channel directory and every routed message key on it, while the
    /// synced row keeps the agent name the Inspector shows.
    pub address: Option<String>,
}

/// Cap on live local child-channel entries (bounded memory; a hostile flood
/// of child starts can never grow the map unboundedly).
const MAX_LOCAL_CHILD_CHANNELS: usize = 256;

/// Turn-start hook: called with `(chat_id, cwd)`.
pub type TurnListener = Arc<dyn Fn(&str, &str) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct SessionsEngine {
    inner: Arc<Inner>,
}

impl SessionsEngine {
    pub fn new(
        device_id: String,
        journal: Arc<RunJournal>,
        registry: Arc<HarnessRegistry>,
    ) -> Self {
        let (sessions_tx, _) = watch::channel(Vec::new());
        Self {
            inner: Arc::new(Inner {
                device_id,
                journal,
                registry,
                doc_host: Mutex::new(None),
                runs: Mutex::new(HashMap::new()),
                hubs: Mutex::new(HashMap::new()),
                statuses: Mutex::new(HashMap::new()),
                sessions_tx,
                child_channels: Mutex::new(HashMap::new()),
                last_requests: Mutex::new(HashMap::new()),
                harness_sessions: Mutex::new(HashMap::new()),
                titles: OnceLock::new(),
                turn_listener: OnceLock::new(),
                ephemeral: Mutex::new(HashSet::new()),
                ephemeral_tx: Mutex::new(HashMap::new()),
                plugin_epoch: AtomicU64::new(0),
                quiesce: OnceLock::new(),
            }),
        }
    }

    /// Wire the doc host (called once at engine assembly; the two services are mutually
    /// referential by design — sessions stream into docs, docs execute commands here).
    pub fn set_doc_host(&self, host: DocHost) {
        // First set wins (the OnceLock contract this slot replaced).
        let mut slot = lock(&self.inner.doc_host);
        if slot.is_none() {
            *slot = Some(host);
        }
    }

    /// Sever the doc-host back-edge (runtime retirement; the doc host's
    /// `shutdown_workers` clears its own sessions edge). Every access site
    /// already treats a missing doc host as "not wired".
    pub fn clear_doc_host(&self) {
        lock(&self.inner.doc_host).take();
    }

    /// Wire the chat auto-titler (called once at engine assembly). After each
    /// completed exchange the run task fires it for still-untitled chats.
    pub fn set_titles(&self, titles: crate::titles::TitleGenerator) {
        let _ = self.inner.titles.set(titles);
    }

    /// Test seam: fix the watchdog windows instead of reading the env per run.
    pub fn set_quiesce_windows(&self, windows: QuiesceWindows) {
        let _ = self.inner.quiesce.set(windows);
    }

    /// Wire the turn-start listener (called once at engine assembly).
    pub fn set_turn_listener(&self, listener: TurnListener) {
        let _ = self.inner.turn_listener.set(listener);
    }

    // ── temporary Side Chat support ─────────────────────────────

    /// Mark `chat_id` as a temporary Side Chat: its status/harness-session
    /// continuity stays in memory while every workspace/journal/observability
    /// write is suppressed (see [`Inner::ephemeral`]). Called by the side-chat
    /// manager on start. Idempotent.
    pub fn register_ephemeral(&self, chat_id: &str) {
        lock(&self.inner.ephemeral).insert(chat_id.to_string());
        // Eagerly create the PRIVATE status sender: a publish that races
        // ahead of the panel's subscribe must never be lost. A very fast
        // first send can mark Working→Idle before `WatchSideChatStatus`
        // lands, and the watch sender retains the current value for the
        // late subscriber.
        lock(&self.inner.ephemeral_tx)
            .entry(chat_id.to_string())
            .or_insert_with(|| watch::channel(None).0);
    }

    /// Remove `chat_id` from the ephemeral set (promotion: statuses now flow
    /// through the public watch + workspace rows) and drop its private status
    /// channel. The in-memory `statuses` entry survives, so the promoted chat
    /// keeps its live status on the next public publish. Idempotent.
    pub fn unregister_ephemeral(&self, chat_id: &str) {
        lock(&self.inner.ephemeral).remove(chat_id);
        lock(&self.inner.ephemeral_tx).remove(chat_id);
    }

    /// The Side Chat panel's PRIVATE status watch: `None` until the first
    /// transition (or after dispose), then the chat's live [`Session`]. Never
    /// appears in the public `watch_sessions` stream while ephemeral.
    pub fn watch_ephemeral(&self, chat_id: &str) -> watch::Receiver<Option<Session>> {
        let mut map = lock(&self.inner.ephemeral_tx);
        map.entry(chat_id.to_string())
            .or_insert_with(|| watch::channel(None).0)
            .subscribe()
    }

    /// Dispose path: drop the in-memory status entry entirely. A temporary
    /// Side Chat must leave no trace — after `unregister_ephemeral` its id is
    /// no longer filtered from public publishes, so without this the stale
    /// status would resurface in `WatchSessions` on the next transition of
    /// any chat. (Promotion does NOT call this: the promoted chat keeps its
    /// live status.)
    pub fn drop_status(&self, chat_id: &str) {
        lock(&self.inner.statuses).remove(chat_id);
    }

    /// Watcher-count helper for the stale reaper: how many live status-watch
    /// receivers this ephemeral chat has. A Side Chat panel holds one while
    /// open; the count drops to zero once the tab closes (or the watch RPC
    /// was lost).
    pub fn ephemeral_watcher_count(&self, chat_id: &str) -> usize {
        lock(&self.inner.ephemeral_tx)
            .get(chat_id)
            .map_or(0, |tx| tx.receiver_count())
    }

    /// Promotion hook: remove the chat from the ephemeral
    /// set (statuses now flow through the public paths) and publish its
    /// CURRENT status to the public `WatchSessions` watch + the workspace
    /// session row immediately — the panel switches to the normal surface at
    /// promotion, and a stale private-only status must never linger
    /// unpublished. Deliberately NOT called on dispose (dispose drops the
    /// status entirely via [`Self::drop_status`]).
    pub fn promote_ephemeral(&self, chat_id: &str) {
        self.unregister_ephemeral(chat_id);
        let session = lock(&self.inner.statuses).get(chat_id).cloned();
        if let Some(session) = session {
            self.inner.publish_session(chat_id, &session);
        }
    }

    /// Promotion backfill: stamp the (now normal) chat row with the harness
    /// session recorded in memory while the chat was temporary. No-op when
    /// none was recorded. Idempotent.
    pub fn persist_harness_session(&self, chat_id: &str) {
        let known = lock(&self.inner.harness_sessions).get(chat_id).cloned();
        if let Some(known) = known
            && !known.session_id.is_empty()
            && let Some(ws) = self.inner.workspace()
        {
            ws.set_chat_harness_session(chat_id, &known.session_id, &known.cwd);
        }
    }

    /// Session Rewind: re-point a chat at a DIFFERENT harness session — the
    /// truncated one the rewind materialized — or tombstone it (`None`) when
    /// the rewind left an empty context and pi has nothing persisted yet.
    ///
    /// Both the live-process cache AND the durable chat row are written:
    /// [`Inner::resume_for`] reads the cache first and the journal only when
    /// both are absent, so a stale entry in either would resume the
    /// PRE-rewind session and resurrect the removed turns.
    pub fn rebind_harness_session(&self, chat_id: &str, session_id: Option<&str>, cwd: &str) {
        // The empty string is the established "do not resume" tombstone on
        // both the map and the row.
        let session_id = session_id.unwrap_or_default();
        lock(&self.inner.harness_sessions).insert(
            chat_id.to_string(),
            HarnessSessionRef {
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
            },
        );
        if self.inner.is_ephemeral(chat_id) {
            return; // temporary Side Chat: the row is stamped at promotion
        }
        if let Some(ws) = self.inner.workspace() {
            ws.set_chat_harness_session(chat_id, session_id, cwd);
        }
    }

    fn note_turn_start(&self, chat_id: &str, cwd: &str) {
        if self.inner.is_ephemeral(chat_id) {
            return; // temporary Side Chat: no checkout snapshot for the diff pane
        }
        if let Some(listener) = self.inner.turn_listener.get() {
            listener(chat_id, cwd);
        }
    }

    fn doc_handle(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        let host = self
            .inner
            .doc_host()
            .ok_or_else(|| EngineError::Other("doc host not wired into sessions engine".into()))?;
        host.open(chat_id)
    }

    /// Status watch: the full session list, re-sent on every transition.
    pub fn watch_sessions(&self) -> watch::Receiver<Vec<Session>> {
        self.inner.sessions_tx.subscribe()
    }

    pub fn session_status(&self, chat_id: &str) -> Option<Session> {
        lock(&self.inner.statuses).get(chat_id).cloned()
    }

    /// Live subagent projection (pi `cypher.subagents.v1`): mirror the latest
    /// snapshot onto the chat's session row — `subagents` + `updated_at` ONLY.
    /// Deliberately no status transition and no `started_at` change: a
    /// background subagent finishing must not flip a parked session Working
    /// nor reset the elapsed timer. The `updated_at` bump keeps the row
    /// inside the UI's 45s staleness window while only a subagent is active.
    pub fn set_subagents(&self, chat_id: &str, runs: Vec<SubagentRun>) {
        self.inner.set_subagents(chat_id, runs);
    }

    /// Register a child chat's host-local messaging channel (initial run
    /// only). Never persisted/synced — see [`Inner::child_channels`].
    pub fn register_child_channel(
        &self,
        chat_id: &str,
        channel_root: &str,
        child_index: u32,
        address: Option<&str>,
    ) {
        let mut map = lock(&self.inner.child_channels);
        if map.len() >= MAX_LOCAL_CHILD_CHANNELS {
            // Bounded cap: evict an arbitrary stale entry (the channel is
            // short-lived and consumed at first dispatch; the eviction is a
            // memory bound, not an LRU guarantee).
            if let Some(oldest) = map.keys().next().cloned() {
                map.remove(&oldest);
            }
        }
        map.insert(
            chat_id.to_string(),
            LocalChildChannel {
                channel_root: channel_root.to_string(),
                child_index,
                address: address.map(str::to_owned),
            },
        );
    }

    /// Drop a child chat's local channel (delete/rollback teardown), returning it.
    pub fn remove_child_channel(&self, chat_id: &str) -> Option<LocalChildChannel> {
        lock(&self.inner.child_channels).remove(chat_id)
    }

    /// Owner-death terminalization: the harness run for `chat_id` has truly
    /// ended — any subagent run still projected `Running` flips to `Error`
    /// (the owner that would have published its terminal state is gone). The
    /// session row/watch/workspace mirror updates; `status`/`started_at` stay
    /// untouched. Called ONLY when the real harness owner ends (drive_run's
    /// final exit, startup/early-fatal returns) — never on a parked Done,
    /// where a legal background subagent stays Running on the open stream.
    pub fn fail_orphaned_subagents(&self, chat_id: &str, reason: &str) {
        self.inner.fail_orphaned_subagents(chat_id, reason);
    }

    /// Boot recovery: sweep THIS device's durable session rows and terminalize
    /// any subagent run still projected `Running` (the previous engine died
    /// with them in flight — they can never settle on their own). Remote
    /// device rows are untouched: their owners may still be live. Pure
    /// projection fix — `status`/`started_at` never change. Called right after
    /// [`Self::recover_stale`] at engine assembly.
    pub fn recover_orphaned_subagents(&self) -> Result<usize, EngineError> {
        const REASON: &str = "Subagent owner ended before engine restart";
        let Some(ws) = self.inner.workspace() else {
            return Ok(0);
        };
        let rows = ws.read_sessions()?;
        let mut fixed = 0usize;
        for mut row in rows {
            if row.device_id != self.inner.device_id {
                continue; // another device's row: its owner may still be live
            }
            let failed = fail_running_subagents(&mut row.subagents, now_ms(), REASON);
            if failed > 0 {
                ws.record_session(&row);
                fixed += failed;
            }
        }
        if fixed > 0 {
            tracing::info!(fixed, "orphaned subagent runs terminalized on boot");
        }
        Ok(fixed)
    }

    /// The chat's Pi session file, when its harness session is one. Pi names
    /// a session by its file's absolute path; other harnesses' ids are not
    /// paths, so anything but an absolute `.jsonl` path is no Pi session.
    /// Blocking: may scan the chat's journal.
    pub fn pi_session_file(&self, chat_id: &str) -> Option<std::path::PathBuf> {
        let (session_id, _) = self.inner.known_harness_session(chat_id)?;
        let path = std::path::PathBuf::from(session_id);
        (path.is_absolute() && path.extension().is_some_and(|ext| ext == "jsonl")).then_some(path)
    }

    /// Any run currently working or blocked on input — the auto-updater's
    /// "don't restart from under a session" gate.
    pub fn any_active(&self) -> bool {
        lock(&self.inner.statuses).values().any(|s| {
            matches!(
                s.status,
                cypher_proto::SessionStatus::Working | cypher_proto::SessionStatus::AwaitingInput
            )
        })
    }

    /// The last request dispatched for a chat (steer→new-turn fallback).
    pub fn last_request(&self, chat_id: &str) -> Option<RunRequest> {
        lock(&self.inner.last_requests).get(chat_id).cloned()
    }

    /// Subscribe to a chat's live event stream: returns the journal replay after
    /// `after_seq` plus a live receiver. Subscribe-then-replay ordering means overlap
    /// (dedupe by seq) rather than gaps.
    pub fn subscribe(
        &self,
        chat_id: &str,
        after_seq: u64,
    ) -> Result<(Vec<JournaledEvent>, broadcast::Receiver<JournaledEvent>), EngineError> {
        let rx = {
            let mut hubs = lock(&self.inner.hubs);
            hubs.entry(chat_id.to_string())
                .or_insert_with(|| broadcast::channel(1024).0)
                .subscribe()
        };
        let replay = self
            .inner
            .journal
            .replay(chat_id, after_seq)?
            .into_iter()
            .map(|(seq, event)| JournaledEvent { seq, event })
            .collect();
        Ok((replay, rx))
    }

    /// Start (or route) a run for `chat_id`.
    ///
    /// - The user message entry is written to the doc immediately (id = `message_id`).
    /// - A live steerable run receives the prompt as its next turn via the mailbox
    ///   (zeron's persistent-session routing); otherwise any live run is interrupted
    ///   first — never two runtimes driving one chat.
    pub async fn dispatch(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: RunRequest,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        self.dispatch_augmented(chat_id, harness_id, request, None, message_id)
            .await
    }

    /// [`Self::dispatch`] with an EFFECTIVE harness prompt override (the
    /// Comment feature): the doc user entry keeps `request.prompt` (visible
    /// truth) while the agent receives `agent_prompt` when present.
    pub async fn dispatch_augmented(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        request: RunRequest,
        agent_prompt: Option<String>,
        message_id: Option<String>,
    ) -> Result<String, EngineError> {
        self.dispatch_with(
            chat_id,
            harness_id,
            request,
            agent_prompt,
            message_id,
            false,
        )
        .await
    }

    /// [`Self::dispatch`] with the startup-crash retry marker: the retry
    /// re-dispatches with `startup_retry = true`, which makes that attempt
    /// final (its own startup death surfaces instead of retrying again).
    /// Boxed future: `drive_run` re-enters this for that retry, and the
    /// erasure breaks the opaque-type cycle the recursion would otherwise form.
    fn dispatch_with<'a>(
        &'a self,
        chat_id: &'a str,
        harness_id: HarnessId,
        request: RunRequest,
        agent_prompt: Option<String>,
        message_id: Option<String>,
        startup_retry: bool,
    ) -> futures::future::BoxFuture<'a, Result<String, EngineError>> {
        Box::pin(self.dispatch_inner(
            chat_id,
            harness_id,
            request,
            agent_prompt,
            message_id,
            startup_retry,
        ))
    }

    async fn dispatch_inner(
        &self,
        chat_id: &str,
        harness_id: HarnessId,
        mut request: RunRequest,
        agent_prompt: Option<String>,
        mut message_id: Option<String>,
        startup_retry: bool,
    ) -> Result<String, EngineError> {
        // Read before stripping: the transcript shows the quote the user saw.
        let comments = MessageComment::from_agent_prompt(agent_prompt.as_deref().unwrap_or(""));
        let agent_prompt = agent_prompt_for(harness_id, agent_prompt);
        // Project-less chats store cwd `~` (the creating device can't know the
        // host's home); expand it here, on the host, where the run spawns.
        request.cwd = crate::repos::expand_home(&request.cwd);
        // A quick chat's scratch folder lives under the host's temp dir,
        // which a reboot may have emptied: recreate it so the run spawns.
        crate::scratch::ensure_for_run(chat_id, &request.cwd);
        // Visible prompt = the doc/user entry truth; the harness gets the
        // augmented effective prompt when `agent_prompt` is present.
        let visible_prompt = request.prompt.clone();
        let effective = agent_prompt
            .clone()
            .unwrap_or_else(|| visible_prompt.clone());
        // Every dispatched prompt is a turn — routed steer or fresh run alike.
        self.note_turn_start(chat_id, &request.cwd);
        // A parked process launched with other model settings must not take
        // this turn: end it here so the turn below spawns with the new ones.
        self.retire_stale_run(chat_id, &LaunchConfig::of_request(harness_id, &request))
            .await?;
        let routed = lock(&self.inner.runs).get(chat_id).map(|h| {
            (
                h.run_id.clone(),
                h.steerable,
                h.steer_tx.clone(),
                h.routed_steers.clone(),
            )
        });
        if let Some((run_id, steerable, steer_tx, ledger)) = routed {
            let user_id = message_id.clone().unwrap_or_else(new_id);
            // The ledger entry and the mailbox send are atomic under the
            // ledger lock: the entry goes in BEFORE try_send, so the run
            // task can never observe an accepted mailbox message without its
            // ledger entry (the parked-pi path emits Steered immediately on
            // mailbox receive, and the exit drain must always find what it
            // owns). A failed send rolls the exact entry back while still
            // holding the lock, then falls through to a fresh run below.
            let sent = if steerable {
                let mut ledger = lock(&ledger);
                ledger.push_back(RoutedSteer {
                    prompt: request.prompt.clone(),
                    agent_prompt: agent_prompt.clone(),
                    message_id: user_id.clone(),
                });
                let message = SteerMessage {
                    prompt: effective.clone(),
                    message_id: message_id.clone(),
                };
                let ok = steer_tx.try_send(message).is_ok();
                if !ok {
                    // Mailbox closed (runtime mid-teardown / non-steering
                    // harness): drop the exact entry we just added.
                    ledger.retain(|s| s.message_id != user_id);
                }
                ok
            } else {
                false
            };
            if sent {
                let handle = self.doc_handle(chat_id)?;
                handle.write_user_prompt(&user_id, &visible_prompt, &comments, now_ms())?;
                if self.is_live(chat_id, &run_id) {
                    // Working BEFORE the lastMessageAt bump: both ride the
                    // workspace doc from this one peer, so causal order makes it
                    // impossible for an observer to hold [new message, old status]
                    // — that gap read as unseen-with-no-live-run = a phantom
                    // "completed" flash on every remote send.
                    self.set_status(chat_id, SessionStatus::Working, false);
                    self.inner.note_message(chat_id, &visible_prompt);
                    return Ok(run_id);
                }
                // The run died around the send. If its exit drain already
                // claimed the entry, that re-dispatch owns the message —
                // otherwise reclaim it and fall through to a fresh run.
                let reclaimed = {
                    let mut ledger = lock(&ledger);
                    let before = ledger.len();
                    ledger.retain(|s| s.message_id != user_id);
                    ledger.len() != before
                };
                if !reclaimed {
                    self.inner.note_message(chat_id, &visible_prompt);
                    return Ok(run_id);
                }
                // Keep the already-written doc entry's id for the fresh run
                // below (write_user_message dedupes by id).
                message_id = Some(user_id);
            }
            // Mailbox closed (runtime mid-teardown / non-steering harness) or
            // the routed run died with the message reclaimed: replace it.
            self.interrupt(chat_id).await?;
        }

        let harness = self.inner.registry.resolve(harness_id)?;
        let handle = self.doc_handle(chat_id)?;
        let user_id = message_id.unwrap_or_else(new_id);
        handle.write_user_prompt(&user_id, &visible_prompt, &comments, now_ms())?;

        // Engine-owned resume (zeron sessions.ts:736 — every dispatch read the
        // chat's stored harness session): callers always send `resume: None`;
        // the engine threads the chat's prior harness session back in so a new
        // process (app restart) continues the same harness conversation. The
        // startup-crash retry injects too — a stale id is the harness's
        // problem now (`session/load` falls back to `session/new` internally),
        // and starting the retry fresh silently dropped a good conversation.
        let mut resume_injected = false;
        if request.resume.is_none() {
            request.resume = self.inner.resume_for(chat_id, &request.cwd);
            resume_injected = request.resume.is_some();
        }
        lock(&self.inner.last_requests).insert(chat_id.to_string(), request.clone());

        let run_id = new_id();
        let (steer_tx, steer_rx) = mpsc::channel::<SteerMessage>(32);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (engine_tx, engine_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let pending_inputs: PendingInputs = Arc::new(Mutex::new(HashMap::new()));

        // Input bridge: the harness asks questions; we mint the request id, park the
        // resolver for `respond_input`, and surface the event through the run pipeline.
        let request_input = {
            let pending = pending_inputs.clone();
            let engine_tx = engine_tx.clone();
            Box::new(move |questions: Vec<UserInputQuestion>| {
                let (tx, rx) = oneshot::channel();
                let request_id = new_id();
                lock(&pending).insert(request_id.clone(), tx);
                let _ = engine_tx.send(AgentEvent::InputRequested {
                    request_id,
                    questions,
                });
                rx
            })
        };
        let interrupt_token = CancellationToken::new();
        let controls = RunControls {
            request_input,
            steering: steer_rx,
            interrupt: interrupt_token.clone(),
            host: self.inner.host_context(chat_id),
        };

        lock(&self.inner.runs).insert(
            chat_id.to_string(),
            RunHandle {
                run_id: run_id.clone(),
                steerable: harness.supports_steering(),
                steer_tx,
                interrupt_token,
                cancel: cancel_tx,
                engine_tx,
                pending_inputs,
                routed_steers: Arc::new(Mutex::new(std::collections::VecDeque::new())),
                launch: LaunchConfig::of_request(harness_id, &request),
            },
        );
        self.set_status(chat_id, SessionStatus::Working, true);
        // AFTER Working (same causal-order guarantee as the steer path): the
        // lastMessageAt bump must never be observable ahead of the live run.
        self.inner.note_message(chat_id, &visible_prompt);

        // Name the chat NOW, off the first prompt — not after the first
        // exchange completes ("called New session for a long time for no
        // reason"; the titler only needs the prompt and skips titled chats;
        // the Done-time call below stays as the retry for a failed
        // generation).
        if !self.inner.is_ephemeral(chat_id)
            && let Some(titles) = self.inner.titles.get()
        {
            titles.maybe_generate(chat_id, harness_id, &visible_prompt, &request.cwd);
        }

        tokio::spawn(drive_run(
            self.inner.clone(),
            chat_id.to_string(),
            run_id.clone(),
            harness,
            request,
            handle.doc_arc(),
            controls,
            engine_rx,
            cancel_rx,
            RunResumeState {
                user_message_id: user_id,
                resume_injected,
                startup_retry,
                agent_prompt,
            },
        ));
        Ok(run_id)
    }

    /// Push a steer prompt into the live run's mailbox. `NotSteerable` when no live
    /// steerable run exists — the caller (command executor) dispatches a new turn.
    pub async fn steer(
        &self,
        chat_id: &str,
        prompt: &str,
        message_id: Option<String>,
    ) -> Result<SteerOutcome, EngineError> {
        self.steer_augmented(chat_id, prompt, None, message_id)
            .await
    }

    /// [`Self::steer`] with an EFFECTIVE harness prompt override (the Comment
    /// feature): the doc entry keeps `prompt` while the harness mailbox
    /// receives `agent_prompt` when present — alignment stripped for any
    /// running harness but Pi ([`agent_prompt_for`]).
    pub async fn steer_augmented(
        &self,
        chat_id: &str,
        prompt: &str,
        agent_prompt: Option<String>,
        message_id: Option<String>,
    ) -> Result<SteerOutcome, EngineError> {
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .filter(|h| h.steerable)
            .map(|h| {
                (
                    h.run_id.clone(),
                    h.steer_tx.clone(),
                    h.routed_steers.clone(),
                    h.launch.harness,
                )
            });
        let Some((run_id, steer_tx, ledger, harness)) = target else {
            return Ok(SteerOutcome::NotSteerable);
        };
        let comments = MessageComment::from_agent_prompt(agent_prompt.as_deref().unwrap_or(""));
        let user_id = message_id.clone().unwrap_or_else(new_id);
        // Accepted: the ledger entry and the mailbox send are atomic under
        // the ledger lock — the entry goes in BEFORE try_send, so the run
        // task can never observe an accepted mailbox message without its
        // ledger entry (the parked-pi path emits Steered immediately on
        // mailbox receive, and the exit drain must always find what it
        // owns). A failed send rolls the exact entry back while still
        // holding the lock. After that, the user entry (client-minted id),
        // then Working BEFORE the lastMessageAt bump — same causal-order
        // invariant as the dispatch route (an observer must never hold [new
        // message, settled status]: the phantom "completed" flash).
        let effective =
            agent_prompt_for(harness, agent_prompt.clone()).unwrap_or_else(|| prompt.to_string());
        let sent = {
            let mut ledger = lock(&ledger);
            // Unstripped: an orphan re-dispatch strips for its own harness.
            ledger.push_back(RoutedSteer {
                prompt: prompt.to_string(),
                agent_prompt: agent_prompt.clone(),
                message_id: user_id.clone(),
            });
            let message = SteerMessage {
                prompt: effective,
                message_id: message_id.clone(),
            };
            let ok = steer_tx.try_send(message).is_ok();
            if !ok {
                // Mailbox closed (runtime mid-teardown / non-steering
                // harness): drop the exact entry we just added.
                ledger.retain(|s| s.message_id != user_id);
            }
            ok
        };
        if !sent {
            return Ok(SteerOutcome::NotSteerable);
        }
        let handle = self.doc_handle(chat_id)?;
        handle.write_user_prompt(&user_id, prompt, &comments, now_ms())?;
        // A routed steer is a turn too. Fired here (not only on the confirmed
        // path) — a reclaim falls back to dispatch, which just re-snapshots.
        if let Some(request) = self.last_request(chat_id) {
            self.note_turn_start(chat_id, &request.cwd);
        }
        if self.is_live(chat_id, &run_id) {
            self.set_status(chat_id, SessionStatus::Working, false);
            self.inner.note_message(chat_id, prompt);
            return Ok(SteerOutcome::Accepted);
        }
        // The run died around the send. Exit drain claimed the entry → its
        // re-dispatch owns the message; still ours → reclaim and report
        // NotSteerable so the executor falls back to a fresh dispatch
        // (same message id — the doc entry dedupes).
        let reclaimed = {
            let mut ledger = lock(&ledger);
            let before = ledger.len();
            ledger.retain(|s| s.message_id != user_id);
            ledger.len() != before
        };
        if reclaimed {
            return Ok(SteerOutcome::NotSteerable);
        }
        self.inner.note_message(chat_id, prompt);
        Ok(SteerOutcome::Accepted)
    }

    /// Interrupt the live run, if any. The run settles with a synthetic
    /// `Done{interrupted}` and its streaming entry stamped `aborted`; this waits
    /// (bounded) for that settlement so callers observe a consistent doc.
    pub async fn interrupt(&self, chat_id: &str) -> Result<bool, EngineError> {
        let target = lock(&self.inner.runs).get(chat_id).map(|h| {
            (
                h.run_id.clone(),
                h.interrupt_token.clone(),
                h.cancel.clone(),
                h.pending_inputs.clone(),
            )
        });
        let Some((run_id, token, cancel, pending)) = target else {
            return Ok(false);
        };
        // Unpark any blocked question FIRST (mirrors zeron: harness teardown can await a
        // parked question callback — a run stuck on a question would deadlock the stop).
        let parked: Vec<_> = lock(&pending).drain().map(|(_, tx)| tx).collect();
        for tx in parked {
            let _ = tx.send(Vec::new());
        }
        // Harness-level interrupt (protocol + child teardown) …
        token.cancel();
        // … plus the engine-side grace deadline in the run task, so a harness that
        // ignores its token still settles with a synthesized Done{interrupted}.
        let _ = cancel.send(true);
        // Bounded settle wait (the run task appends Done + stamps `aborted`).
        for _ in 0..500 {
            if !self.is_live(chat_id, &run_id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(true)
    }

    /// Resolve a pending `request_input` question set. Returns `false` when no such
    /// request is pending (unknown id, or the run already settled).
    pub fn respond_input(
        &self,
        chat_id: &str,
        request_id: &str,
        answers: Vec<UserInputAnswer>,
    ) -> Result<bool, EngineError> {
        let target = lock(&self.inner.runs)
            .get(chat_id)
            .map(|h| (h.pending_inputs.clone(), h.engine_tx.clone()));
        let Some((pending, engine_tx)) = target else {
            return Ok(false);
        };
        let Some(resolver) = lock(&pending).remove(request_id) else {
            return Ok(false);
        };
        let _ = resolver.send(answers);
        let _ = engine_tx.send(AgentEvent::InputResolved {
            request_id: request_id.to_string(),
        });
        Ok(true)
    }

    /// Session Fork quiesce: cancel a PARKED (Idle) attached run so the fork
    /// helper can operate on the source session with no warm child in the
    /// way — following the idle reaper's CLEAN path (harness-level
    /// cancellation, the run settles as Idle — never an aborted stamp, the
    /// transcript is untouched) and waiting boundedly for the run to be
    /// removed. Working/AwaitingInput runs must be rejected by the caller
    /// BEFORE this is called; this method only acts on an Idle parked run
    /// and is a no-op when no run is attached.
    pub async fn quiesce_idle_for_fork(&self, chat_id: &str) -> Result<(), EngineError> {
        let target = lock(&self.inner.runs).get(chat_id).map(|h| {
            (
                h.run_id.clone(),
                h.interrupt_token.clone(),
                h.cancel.clone(),
            )
        });
        let Some((run_id, token, cancel)) = target else {
            return Ok(()); // nothing attached — nothing to quiesce
        };
        // The idle reaper's exact teardown: cancel the harness-level token;
        // the run task's parked loop breaks with SessionStatus::Idle (clean
        // end, no aborted stamp) and removes the run in its exit path.
        token.cancel();
        let _ = cancel.send(true);
        for _ in 0..500 {
            if !self.is_live(chat_id, &run_id) {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err(EngineError::Other(format!(
            "source run did not quiesce in time: {chat_id}"
        )))
    }

    /// Keep a chat's live run from serving a turn that wants different model
    /// settings than its harness process was launched with (a mid-session
    /// model/reasoning/harness switch). A PARKED run ends cleanly — the idle
    /// reaper's path, no aborted stamp — so the caller's turn spawns fresh
    /// (engine-owned resume keeps the conversation). A BUSY run keeps its
    /// current turn (anything routed into it now still lands there); its
    /// launch config is unchanged, so the first send after it parks ends it.
    /// Either way the retained run config takes the new settings, so an
    /// orphaned-steer re-dispatch or steer fallback uses them too.
    pub async fn retire_stale_run(
        &self,
        chat_id: &str,
        wanted: &LaunchConfig,
    ) -> Result<(), EngineError> {
        let target = lock(&self.inner.runs).get(chat_id).and_then(|h| {
            (h.launch != *wanted).then(|| {
                (
                    h.run_id.clone(),
                    h.interrupt_token.clone(),
                    h.launch.clone(),
                )
            })
        });
        let Some((run_id, token, launched)) = target else {
            return Ok(());
        };
        if let Some(request) = lock(&self.inner.last_requests).get_mut(chat_id) {
            wanted.apply_to(request);
        }
        let parked = self
            .session_status(chat_id)
            .is_some_and(|session| session.status == SessionStatus::Idle);
        if !parked {
            return Ok(());
        }
        tracing::info!(
            chat = %chat_id,
            from = ?launched.model,
            to = ?wanted.model,
            "model settings changed; ending parked session so the next turn respawns"
        );
        // Harness token only: flipping the engine cancel watch would stamp
        // the parked turn aborted.
        token.cancel();
        for _ in 0..500 {
            if !self.is_live(chat_id, &run_id) {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tracing::warn!(chat = %chat_id, "parked session ignored clean end; interrupting");
        self.interrupt(chat_id).await.map(drop)
    }

    /// Pi package enablement changed: drop parked children so the next send
    /// respawns against the new settings, and mark live turns so they do not
    /// park when they finish. Clean end — no aborted stamp on the transcript.
    pub async fn recycle_idle_sessions(&self) {
        self.inner.plugin_epoch.fetch_add(1, Ordering::SeqCst);
        let idle: Vec<(String, CancellationToken, String)> = {
            let statuses = lock(&self.inner.statuses);
            lock(&self.inner.runs)
                .iter()
                .filter(|&(id, _handle)| {
                    statuses
                        .get(id)
                        .is_some_and(|session| session.status == SessionStatus::Idle)
                })
                .map(|(id, handle)| {
                    (
                        id.clone(),
                        handle.interrupt_token.clone(),
                        handle.run_id.clone(),
                    )
                })
                .collect()
        };
        for (chat_id, token, run_id) in idle {
            // Idle-reaper path: cancel the harness child only. Do not flip the
            // engine cancel watch — that would stamp the parked turn aborted.
            token.cancel();
            for _ in 0..500 {
                if !self.is_live(&chat_id, &run_id) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
    }

    /// Boot recovery: for every journal whose last event is not `Done` (a run died
    /// mid-stream), stamp this device's abandoned `streaming` doc entries `aborted`
    /// with a VISIBLE "Run interrupted by engine restart" error part, close the
    /// journal with a synthetic `Done{interrupted}` — and then PICK THE RUN BACK
    /// UP: a fresh crashed turn with revival budget left is re-dispatched against
    /// the remembered harness session (zeron: "not just eulogized";
    /// `MAX_AUTO_RESUME` = 3 consecutive revivals, fresh = crashed < 12h ago).
    pub fn recover_stale(&self) -> Result<usize, EngineError> {
        const MAX_AUTO_RESUME: u32 = 3;
        const RESUME_FRESH_MS: i64 = 12 * 60 * 60 * 1000;

        let stale = self.inner.journal.stale_sessions()?;
        let mut recovered = 0usize;
        for chat_id in stale {
            if lock(&self.inner.runs).contains_key(&chat_id) {
                continue; // a live run owns this journal
            }
            let handle = self.doc_handle(&chat_id)?;
            // Harness continuity first: the crashed run's session id may only
            // exist in the journal (the debounced workspace-row write may
            // never have landed) — remember it so the revived run resumes the
            // same harness conversation (zeron recoverDraft, sessions.ts:538).
            if let Some((session_id, cwd)) = self.inner.journal_harness_session(&chat_id) {
                self.inner
                    .remember_harness_session(&chat_id, &session_id, &cwd);
            }
            // The revival prompt: the last user message (idempotent re-dispatch
            // under the SAME id — `write_user_message` dedupes by id, so the
            // transcript never shows a duplicate).
            let prompt = handle.doc().read_entries().ok().and_then(|entries| {
                entries
                    .iter()
                    .rev()
                    .find(|e| e.role == MessageRole::User)
                    .and_then(|e| {
                        e.parts.iter().find_map(|p| match p {
                            MessagePart::Text { text, .. } => Some((e.id.clone(), text.clone())),
                            _ => None,
                        })
                    })
            });
            let attempts = self.inner.journal.resume_attempts(&chat_id);
            let fresh = handle
                .doc()
                .read_entries()
                .ok()
                .and_then(|entries| {
                    entries
                        .iter()
                        .rev()
                        .find(|e| e.status == Some(MessageStatus::Streaming))
                        .map(|e| now_ms() - e.created_at < RESUME_FRESH_MS)
                })
                .unwrap_or(false);
            let will_resume = fresh && prompt.is_some() && attempts < MAX_AUTO_RESUME;

            let note = if will_resume {
                "Run interrupted by engine restart — resuming"
            } else {
                "Run interrupted by engine restart"
            };
            let done = AgentEvent::Done {
                status: DoneStatus::Interrupted,
                result: None,
                error: Some(note.into()),
                session_id: None,
            };
            self.inner.publish(&chat_id, &done);
            let stamped = handle.mark_abandoned_streams(note)?.len();
            self.set_status(&chat_id, SessionStatus::Idle, false);
            tracing::info!(chat = %chat_id, stamped, will_resume, attempts, "recovered stale session journal");
            recovered += 1;

            if !will_resume {
                continue;
            }
            let attempt = self.inner.journal.note_resume_attempt(&chat_id);
            let (user_id, prompt_text) = prompt.expect("gated by will_resume");
            let sessions = self.clone();
            tokio::spawn(async move {
                let Some(host) = sessions.inner.doc_host() else {
                    return;
                };
                let request = sessions
                    .last_request(&chat_id)
                    .or_else(|| host.request_from_chat_row(&chat_id, &prompt_text))
                    // Last resort: the journal's own cwd (zeron's draft config)
                    // — a crash can predate the debounced workspace-row write.
                    .or_else(|| {
                        let (_, cwd) = sessions.inner.journal_harness_session(&chat_id)?;
                        Some(RunRequest {
                            prompt: String::new(),
                            harness: None,
                            model: None,
                            reasoning: None,
                            model_options: Default::default(),
                            cwd,
                            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                            auto_approve: false,
                            attachments: Vec::new(),
                            pending_attachments: Vec::new(),
                            resume: None,
                            worktree: None,
                        })
                    });
                let Some(mut request) = request else {
                    tracing::warn!(chat = %chat_id, "auto-resume skipped: no run config");
                    return;
                };
                request.prompt = prompt_text;
                request.resume = None; // dispatch re-injects the remembered session
                request.attachments = Vec::new();
                let harness_id = host.harness_for_request(&chat_id, &request);
                match sessions
                    .dispatch(&chat_id, harness_id, request, Some(user_id))
                    .await
                {
                    Ok(_) => {
                        tracing::info!(chat = %chat_id, attempt, "auto-resumed crashed run")
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err, "auto-resume dispatch failed")
                    }
                }
            });
        }
        Ok(recovered)
    }

    /// Graceful shutdown: interrupt every live run so streaming entries settle.
    pub async fn shutdown(&self) {
        let chats: Vec<String> = lock(&self.inner.runs).keys().cloned().collect();
        for chat_id in chats {
            if let Err(err) = self.interrupt(&chat_id).await {
                tracing::warn!(chat = %chat_id, error = %err, "shutdown interrupt failed");
            }
        }
    }

    fn is_live(&self, chat_id: &str, run_id: &str) -> bool {
        lock(&self.inner.runs)
            .get(chat_id)
            .is_some_and(|h| h.run_id == run_id)
    }

    fn set_status(&self, chat_id: &str, status: SessionStatus, fresh_start: bool) {
        self.inner.set_status(chat_id, status, fresh_start);
    }

    /// Session Fork (v1): is `chat_id` a temporary Side Chat (host-memory
    /// only, no durable workspace row)? Forks of a temporary chat are refused
    /// — the source must be a durable root chat whose transcript and Pi
    /// session live on disk.
    pub fn is_ephemeral(&self, chat_id: &str) -> bool {
        self.inner.is_ephemeral(chat_id)
    }
}

impl Inner {
    fn quiesce_windows(&self) -> QuiesceWindows {
        self.quiesce
            .get()
            .copied()
            .unwrap_or_else(QuiesceWindows::from_env)
    }

    /// Journal + broadcast one event (the two unconditional legs of the pipeline).
    /// For temporary Side Chats the journal is skipped (host-memory only — no
    /// durable run journal until promotion); the hub broadcast (the private
    /// subscribe/watch path) still runs.
    fn publish(&self, chat_id: &str, event: &AgentEvent) -> u64 {
        if self.is_ephemeral(chat_id) {
            if let Some(hub) = lock(&self.hubs).get(chat_id) {
                let _ = hub.send(JournaledEvent {
                    seq: 0,
                    event: event.clone(),
                });
            }
            return 0;
        }
        let seq = match self.journal.append(chat_id, event) {
            Ok(seq) => seq,
            Err(err) => {
                tracing::error!(chat = %chat_id, error = %err, "journal append failed");
                0
            }
        };
        if let Some(hub) = lock(&self.hubs).get(chat_id) {
            let _ = hub.send(JournaledEvent {
                seq,
                event: event.clone(),
            });
        }
        seq
    }

    /// True when `chat_id` is a temporary Side Chat (see [`Inner::ephemeral`]).
    fn is_ephemeral(&self, chat_id: &str) -> bool {
        lock(&self.ephemeral).contains(chat_id)
    }

    /// Publish a session status: ephemeral chats update ONLY their private
    /// watch (the Side Chat panel); normal chats update the public sessions
    /// watch AND the workspace session row (remote sidebars).
    fn publish_session(&self, chat_id: &str, session: &Session) {
        if !self.publish_local(chat_id, session) {
            return;
        }
        if let Some(ws) = self.workspace() {
            ws.record_session(session);
            ws.notify_session_event(session);
        }
    }

    /// The engine-local half of [`Self::publish_session`]: refresh the
    /// ephemeral watch or the public `WatchSessions` list, without touching
    /// the workspace registry. Returns whether the chat is public (i.e.
    /// whether a registry mirror would apply).
    fn publish_local(&self, chat_id: &str, session: &Session) -> bool {
        if self.is_ephemeral(chat_id) {
            if let Some(tx) = lock(&self.ephemeral_tx).get(chat_id) {
                let _ = tx.send_replace(Some(session.clone()));
            }
            return false;
        }
        let ephemeral = lock(&self.ephemeral);
        let mut list: Vec<Session> = lock(&self.statuses)
            .values()
            .filter(|s| !ephemeral.contains(&s.chat_id))
            .cloned()
            .collect();
        list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        drop(ephemeral);
        // send_replace: keep the current value fresh even with no receivers,
        // so late WatchSessions subscribers see the last transition.
        self.sessions_tx.send_replace(list);
        true
    }

    /// Bump the session's freshness on stream activity WITHOUT a status
    /// transition. Long silent-LOOKING stretches (thinking heartbeats, a big
    /// tool input being generated) still carry events — the UI's 45s
    /// staleness gate must not flip "Working" off mid-run. Throttled: a
    /// workspace-doc mirror per delta would be far too chatty.
    fn touch_session(&self, chat_id: &str) {
        // Freshness only: the row's fields are unchanged, so every touch is a
        // durable registry write (≈5 DO rows, broadcast to every device) that
        // says nothing but "still alive". `SESSION_STALE_MS` (45s) is the
        // deadline it feeds, so the cadence only has to stay comfortably
        // inside that window — at 20s a touch can be lost entirely and the
        // next one still lands 5s before the session would read as dead.
        const TOUCH_THROTTLE_MS: i64 = 20_000;
        let now = Utc::now();
        // The statuses guard is RELEASED before publish (publish re-locks
        // it to build the public list — a std Mutex is not reentrant).
        let session = {
            let mut statuses = lock(&self.statuses);
            let Some(entry) = statuses.get_mut(chat_id) else {
                return;
            };
            let age = now
                .signed_duration_since(entry.updated_at)
                .num_milliseconds();
            if age < TOUCH_THROTTLE_MS {
                return;
            }
            entry.updated_at = now;
            entry.clone()
        };
        self.publish_session(chat_id, &session);
    }

    /// The gauge this device last wrote to the chat's durable session row.
    /// Seeds a row the live map doesn't have yet (the first transition after
    /// an engine restart), so that transition's registry write doesn't drop
    /// the ring locally before the next reading lands.
    fn durable_context_usage(&self, chat_id: &str) -> Option<ContextUsage> {
        let ws = self.workspace()?;
        let rows = ws.watch_session_rows();
        let rows = rows.borrow();
        rows.iter()
            .find(|s| s.chat_id == chat_id && s.device_id == self.device_id)
            .and_then(|s| s.context_usage)
    }

    /// Context-window gauge (ACP `usage_update`, pi session stats): update
    /// the chat's session row's `context_usage` ONLY. Unlike subagent status
    /// it is not a liveness signal, so `updated_at` stays put — a gauge
    /// landing after a turn settled must not make a parked row read fresh.
    /// An unchanged reading publishes nothing (Claude streams one per
    /// message delta).
    ///
    /// Every reading reaches this engine's own watchers at once. The synced
    /// registry row (other devices' rings) is written sparingly: while the
    /// turn is Working the reading rides the row's next write — the 20s
    /// freshness touch or the settle transition — and only a reading that
    /// lands on a settled row (Pi's post-settle and post-compaction reads)
    /// costs a write of its own.
    fn set_context_usage(&self, chat_id: &str, usage: ContextUsage) {
        let seed = self.durable_context_usage(chat_id);
        let session = {
            let mut statuses = lock(&self.statuses);
            let entry = statuses
                .entry(chat_id.to_string())
                .or_insert_with(|| Session {
                    chat_id: chat_id.to_string(),
                    device_id: self.device_id.clone(),
                    status: SessionStatus::Idle,
                    started_at: None,
                    updated_at: Utc::now(),
                    subagents: Vec::new(),
                    context_usage: seed,
                    throughput: None,
                });
            if entry.context_usage == Some(usage) {
                return;
            }
            entry.context_usage = Some(usage);
            entry.clone()
        };
        if session.status == SessionStatus::Working {
            self.publish_local(chat_id, &session);
        } else {
            self.publish_session(chat_id, &session);
        }
    }

    /// Live throughput (pi `cypher.throughput.v1`): the working trailer's
    /// tok/s. Like the context gauge's in-turn readings, each one reaches
    /// only this engine's watchers — a reading every half second of streaming
    /// is not worth a synced write. Other devices get the latest one when the
    /// row is next written anyway (the 20s freshness touch, a subagent
    /// snapshot, a transition), and the settle write clears it. No
    /// `updated_at` bump: it is not a liveness signal, and a missing row is
    /// never created for it.
    fn set_throughput(&self, chat_id: &str, throughput: Option<Throughput>) {
        let session = {
            let mut statuses = lock(&self.statuses);
            let Some(entry) = statuses.get_mut(chat_id) else {
                return;
            };
            if entry.throughput.is_none() && throughput.is_none() {
                return;
            }
            entry.throughput = throughput;
            entry.clone()
        };
        self.publish_local(chat_id, &session);
    }

    /// Live subagent projection (pi `cypher.subagents.v1`): update the chat's
    /// session row's `subagents` + `updated_at` ONLY — no status transition,
    /// no `started_at` change, and a missing row is created Idle (a subagent
    /// can be the chat's only activity). The `updated_at` bump doubles as the
    /// staleness heartbeat so a run whose ONLY activity is subagent status
    /// never reads stale.
    fn set_subagents(&self, chat_id: &str, runs: Vec<SubagentRun>) {
        let now = Utc::now();
        let seed = self.durable_context_usage(chat_id);
        // Statuses guard released before publish (publish re-locks it).
        let session = {
            let mut statuses = lock(&self.statuses);
            let entry = statuses
                .entry(chat_id.to_string())
                .or_insert_with(|| Session {
                    chat_id: chat_id.to_string(),
                    device_id: self.device_id.clone(),
                    status: SessionStatus::Idle,
                    started_at: None,
                    updated_at: now,
                    subagents: Vec::new(),
                    context_usage: seed,
                    throughput: None,
                });
            entry.subagents = runs;
            entry.updated_at = now;
            entry.clone()
        };
        self.publish_session(chat_id, &session);
    }

    fn set_status(&self, chat_id: &str, status: SessionStatus, fresh_start: bool) {
        let now = Utc::now();
        let seed = self.durable_context_usage(chat_id);
        let (session, entered_attention) = {
            let mut statuses = lock(&self.statuses);
            // A run that starts waiting on the user or fails is new activity
            // even when no message text lands (a question, a crash): the
            // badge on every device keys off `lastMessageAt` vs the synced
            // seen marker. A first-ever row is a restore, never a transition.
            let entered_attention = matches!(
                status,
                SessionStatus::AwaitingInput | SessionStatus::Errored
            ) && statuses.get(chat_id).is_some_and(|s| s.status != status);
            let entry = statuses
                .entry(chat_id.to_string())
                .or_insert_with(|| Session {
                    chat_id: chat_id.to_string(),
                    device_id: self.device_id.clone(),
                    status,
                    started_at: None,
                    updated_at: now,
                    subagents: Vec::new(),
                    context_usage: seed,
                    throughput: None,
                });
            // `started_at` is the elapsed-timer base and must only ever mean
            // "this turn". Entering Working from a settled state always
            // restamps (a steer into a parked session is a NEW turn — reusing
            // the old base showed the previous turn's 30min on send), and a
            // settled session drops its base entirely so no later reader can
            // resurrect a stale elapsed.
            let was_active = matches!(
                entry.status,
                SessionStatus::Working | SessionStatus::AwaitingInput
            );
            entry.status = status;
            entry.updated_at = now;
            // Throughput belongs to one turn, like `started_at`: a settled
            // session must not keep the last tok/s, nor a new turn open with
            // the previous one's token count.
            match status {
                SessionStatus::Working if fresh_start || !was_active => {
                    entry.started_at = Some(now);
                    entry.throughput = None;
                }
                SessionStatus::Working | SessionStatus::AwaitingInput => {}
                SessionStatus::Idle | SessionStatus::Errored => {
                    entry.started_at = None;
                    entry.throughput = None;
                }
            }
            (entry.clone(), entered_attention)
        };
        // Statuses guard released before publish (publish re-locks it).
        self.publish_session(chat_id, &session);
        // AFTER the status (the send path's causal order): an observer never
        // holds [new activity, old Working] as a phantom completion.
        if entered_attention
            && !self.is_ephemeral(chat_id)
            && let Some(ws) = self.workspace()
        {
            ws.touch_chat_activity(chat_id);
        }
    }

    /// The doc host, once wired. `None` before assembly or after retirement.
    fn doc_host(&self) -> Option<DocHost> {
        lock(&self.doc_host).clone()
    }

    fn workspace(&self) -> Option<crate::workspace_host::WorkspaceHost> {
        self.doc_host().and_then(|host| host.workspace().cloned())
    }

    /// Host-side run context for one chat: its identity plus — for Cypher-hosted
    /// child subagent chats — the persisted child env the pi harness applies
    /// (system prompt/tools/model/thinking) and the host-LOCAL messaging
    /// channel for the INITIAL run. A NON-serialized internal seam: the child
    /// semantics come from the chat row, never from `RunRequest` (no arbitrary
    /// persisted env maps on the wire), and the channel (an absolute host path)
    /// is engine-local — consumed here so later child turns keep the profile
    /// but have no channel (messaging tools then honestly report unavailable).
    fn host_context(&self, chat_id: &str) -> RunHostContext {
        let mut ctx = RunHostContext {
            chat_id: Some(chat_id.to_string()),
            child: None,
        };
        if let Some(ws) = self.workspace()
            && let Ok(Some(chat)) = ws.chat(chat_id)
            && let Some(child) = chat.child
        {
            let channel = lock(&self.child_channels).remove(chat_id);
            let (channel_root, child_index, address) = match channel {
                Some(c) => (Some(c.channel_root), c.child_index, c.address),
                None => (None, 0, None),
            };
            ctx.child = Some(ChildRunEnv {
                system_prompt: child.profile.system_prompt,
                tools: child.profile.tools,
                model: child.profile.model,
                thinking: child.profile.thinking,
                channel_root,
                run_id: child.parent_run_id,
                agent: address.unwrap_or(child.agent),
                child_index,
            });
        }
        ctx
    }

    /// Sidebar freshness: push a message-persist preview into the chat's workspace row.
    fn note_message(&self, chat_id: &str, text: &str) {
        if text.is_empty() || self.is_ephemeral(chat_id) {
            return; // temporary Side Chats never touch workspace rows
        }
        if let Some(ws) = self.workspace() {
            ws.note_message(chat_id, text);
        }
    }

    /// Record the chat's harness-native session id (and its cwd): live-process
    /// cache plus the durable workspace chat row — the row is what survives an
    /// engine restart (zeron sessions.ts:1039).
    fn remember_harness_session(&self, chat_id: &str, session_id: &str, cwd: &str) {
        if session_id.is_empty() {
            return;
        }
        lock(&self.harness_sessions).insert(
            chat_id.to_string(),
            HarnessSessionRef {
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
            },
        );
        if self.is_ephemeral(chat_id) {
            // Temporary Side Chat: the in-memory map keeps resume continuity
            // within this process; the durable row is stamped at promotion.
            return;
        }
        if let Some(ws) = self.workspace() {
            ws.set_chat_harness_session(chat_id, session_id, cwd);
        }
    }

    // NB: there is deliberately no `forget_harness_session` anymore. The old
    // tombstone fired on "run died before SessionStarted", which — since the
    // ACP conversion made stale ids a harness-internal fallback — only ever
    // meant a child STARTUP failure, and permanently severed good
    // conversations. A truly stale id simply
    // yields a fresh session whose SessionStarted overwrites the row.

    /// The session id to resume for a run in `chat_id` launching from `cwd`
    /// (zeron sessions.ts:736, looked up on every dispatch):
    /// live-process cache → workspace chat row → journal scan (the crash path
    /// where the debounced row write never landed — SessionStarted/Done events
    /// are journaled per event, flushed immediately). Cwd-gated throughout:
    /// harness session stores are keyed by cwd, so a session created elsewhere
    /// never rides `--resume`. An empty stored id is the explicit tombstone —
    /// no resume, no falling through to staler sources.
    fn resume_for(&self, chat_id: &str, cwd: &str) -> Option<String> {
        let (session_id, session_cwd) = self.known_harness_session(chat_id)?;
        let cwd_ok = session_cwd.is_empty() || session_cwd == cwd;
        (!session_id.is_empty() && cwd_ok).then_some(session_id)
    }

    /// The chat's harness session id and the cwd it was created in, from the
    /// first source that knows it: live-process cache → workspace chat row →
    /// journal scan. The first source decides, tombstone (empty id) included.
    fn known_harness_session(&self, chat_id: &str) -> Option<(String, String)> {
        if let Some(known) = lock(&self.harness_sessions).get(chat_id).cloned() {
            return Some((known.session_id, known.cwd));
        }
        if let Some(ws) = self.workspace()
            && let Some((session_id, session_cwd)) = ws.chat_harness_session(chat_id)
        {
            return Some((session_id, session_cwd.unwrap_or_default()));
        }
        let (session_id, session_cwd) = self.journal_harness_session(chat_id)?;
        // Cache the journal hit (memory + row) so later dispatches skip the scan.
        self.remember_harness_session(chat_id, &session_id, &session_cwd);
        Some((session_id, session_cwd))
    }

    /// The last harness session id named anywhere in the chat's journal, with
    /// the cwd of the `SessionStarted` that governs it. `Done.session_id`
    /// inherits the cwd of the most recent `SessionStarted` (same run).
    fn journal_harness_session(&self, chat_id: &str) -> Option<(String, String)> {
        let events = match self.journal.replay(chat_id, 0) {
            Ok(events) => events,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "journal scan for harness session failed");
                return None;
            }
        };
        let mut current_cwd = String::new();
        let mut found: Option<(String, String)> = None;
        for (_, event) in events {
            match event {
                AgentEvent::SessionStarted {
                    session_id, cwd, ..
                } => {
                    current_cwd = cwd;
                    if !session_id.is_empty() {
                        found = Some((session_id, current_cwd.clone()));
                    }
                }
                AgentEvent::Done {
                    session_id: Some(session_id),
                    ..
                } if !session_id.is_empty() => {
                    found = Some((session_id, current_cwd.clone()));
                }
                _ => {}
            }
        }
        found
    }

    fn remove_run(&self, chat_id: &str, run_id: &str) {
        let mut runs = lock(&self.runs);
        if runs.get(chat_id).is_some_and(|h| h.run_id == run_id) {
            runs.remove(chat_id);
        }
    }

    /// Owner-death sweep for one chat's live projection: flip every `Running`
    /// subagent to `Error` with a bounded reason (the harness owner that would
    /// have published its terminal state is gone). Only the projection
    /// changes — session `status`/`started_at` are never touched. Updates the
    /// in-memory row + watch + workspace mirror. No-op when nothing is running.
    fn fail_orphaned_subagents(&self, chat_id: &str, reason: &str) {
        let now = now_ms();
        // Statuses guard released before publish (publish re-locks it).
        let session = {
            let mut statuses = lock(&self.statuses);
            let Some(entry) = statuses.get_mut(chat_id) else {
                return;
            };
            if fail_running_subagents(&mut entry.subagents, now, reason) == 0 {
                return;
            }
            entry.updated_at = Utc::now();
            entry.clone()
        };
        self.publish_session(chat_id, &session);
    }
}

/// Pure owner-death terminalization of one run list: only `Running` runs flip
/// to `Error` with `updated_at`/`ended_at` stamped now and `progress` set to a
/// length-controlled reason; settled runs (Done/Error) are untouched. Returns
/// how many runs were failed. Never a status/started_at change at the session
/// level — that lives with the caller.
fn fail_running_subagents(runs: &mut [SubagentRun], now_ms: i64, reason: &str) -> usize {
    // Bound the reason so a long owner-death message can never blow the
    // snapshot's per-run progress cap (the publisher caps at 4KiB; this is
    // a defensive reader-side trim for engine-synthesized reasons only).
    let reason: String = reason.chars().take(200).collect();
    let mut failed = 0usize;
    for run in runs {
        if run.status != SubagentRunStatus::Running {
            continue;
        }
        run.status = SubagentRunStatus::Error;
        run.updated_at = now_ms;
        run.ended_at = Some(now_ms);
        run.progress = Some(reason.clone());
        failed += 1;
    }
    failed
}

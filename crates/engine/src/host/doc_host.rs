//! DocHost — per-chat `SessionDoc` handles: snapshot persistence (debounced), edge room
//! sync (offline-tolerant), and the HOST-ONLY durable command executor.
//!
//! Ported from zeron's session docs and command executor (spec: ARCHITECTURE §2
//! "command plane"):
//! - the doc IS the outbox: commands and user entries commit locally and sync whenever a
//!   room connection exists; the engine is fully functional with sync disabled;
//! - on every doc change (local commit or remote import) the handle re-emits the joined
//!   transcript to watchers, drains pending commands, and schedules a snapshot save;
//! - command drain: evaluate via `evaluate_command` (with the DocsStore processed
//!   ledger), mark processed BEFORE execute, execute through the sessions engine, then
//!   write the outcome status back into the doc as the sole outcome writer.
//!
//! Chat ownership is gated on the workspace doc (`chats[chat_id].deviceId`), with
//! claim-on-first-command for unknown chats. Queueing a command for a chat hosted on
//! another device POSTs a durable nudge to that device's room (§7 cold-chat delivery);
//! the host's relay receives it and warm-opens the doc, which drains the queue.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use cypher_doc::{
    COMMAND_DEFAULT_TTL_MS, CommandBasedOn, CommandDisposition, DocError, EvaluationContext,
    MessageComment, MessagePart, MessageRole, MessageStatus, SessionCommandEntry,
    SessionCommandPayload, SessionCommandStatus, SessionDoc, SessionMessageEntry, evaluate_command,
    join_continuation_entries,
};
use cypher_proto::{HarnessId, UserInputAnswer, UserInputQuestion};
use cypher_sync::DocsStore;

use crate::EngineError;
use crate::host::workspace_host::WorkspaceHost;
use crate::session::engine::{SessionsEngine, SteerOutcome};
use crate::util::lock;
use crate::util::{new_id, now_ms};

mod chat2_sync;
mod commands;

/// Debounce window for local snapshot saves after a doc change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;

/// Warm-doc LRU: how many unwatched, run-less docs stay fully open. Everything
/// beyond this (and beyond [`cypher_doc::DOC_LRU_BYTE_BUDGET`]) is evicted
/// oldest-access-first — reopening from the SQLite snapshot measured within
/// ~11ms of a warm doc, so the cap trades no perceptible open latency.
const WARM_DOC_CAP: usize = 12;

/// Resident-memory estimate per compressed snapshot byte. Loro snapshots are
/// columnar+compressed; the in-memory doc plus mirror runs well above the blob
/// size. A rough multiplier is enough here — the budget is a safety ceiling,
/// the count cap does the day-to-day work.
const RESIDENT_BYTES_PER_SNAPSHOT_BYTE: usize = 6;

/// Floor per open doc (room socket buffers, tasks) regardless of content size.
const DOC_RESIDENT_FLOOR_BYTES: usize = 512 * 1024;

/// Docs touched this recently are never evicted. Closes the open→attach race:
/// `open()` returns a handle, and until the caller's `watch_messages` lands
/// the doc is unwatched and unpinned — a concurrent eviction would orphan the
/// watcher on a roomless doc that renders once and never updates again.
const EVICT_MIN_IDLE_MS: i64 = 30_000;

/// Edge connection config. The bearer is a **provider**, never a snapshot:
/// every room (re)connect and HTTP request re-reads it, so WorkOS access-token
/// refreshes (~1h expiry) take effect without an engine restart. Dev bearers
/// (which never expire) ride the same seam as a [`cypher_rpc::StaticToken`].
#[derive(Clone)]
pub struct EdgeConfig {
    pub preview: Option<cypher_sync::preview_link::PreviewOptions>,
    /// Edge base URL (`http(s)://…`); rewritten to `ws(s)` for the room socket.
    pub url: String,
    /// Fresh-bearer provider (the relay's `TokenSource`), consulted per
    /// connect/request. `None` from the provider = signed out.
    pub token: Arc<dyn cypher_rpc::TokenSource>,
    /// This engine's device id, carried on room dials (`&device=`) so the
    /// edge can attribute sockets in logs instead of guessing devices from
    /// rotating IPv6 privacy addresses. Empty = omitted (tests).
    pub device_id: String,
    /// The viewport's pending activity refresh, read by the registry presence
    /// beat so that refresh costs no request of its own. Default = an unshared
    /// slot that stays empty (tests, previews, headless sidecars).
    pub viewport_activity: crate::host::viewport_activity::ViewportActivity,
}

impl std::fmt::Debug for EdgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeConfig")
            .field("url", &self.url)
            .field("token", &"<provider>")
            .finish()
    }
}

impl EdgeConfig {
    pub fn new(url: impl Into<String>, token: Arc<dyn cypher_rpc::TokenSource>) -> Self {
        Self {
            preview: None,
            url: url.into(),
            token,
            device_id: String::new(),
            viewport_activity: Default::default(),
        }
    }

    /// Attribute this engine's room sockets in edge logs.
    /// Share the viewport's activity slot so the presence beat can carry
    /// its periodic refresh instead of spending an HTTP request per beat.
    pub fn with_viewport_activity(
        mut self,
        activity: crate::host::viewport_activity::ViewportActivity,
    ) -> Self {
        self.viewport_activity = activity;
        self
    }

    pub fn with_device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = device_id.into();
        self
    }

    /// Test seam: a fixed bearer that never expires.
    pub fn with_static_token(url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::new(url, Arc::new(cypher_rpc::StaticToken(token.into())))
    }

    /// The current bearer, refreshed by the provider if stale. `None` = signed out.
    pub async fn bearer(&self) -> Option<String> {
        self.token.token().await
    }

    pub fn token_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.token.subscribe()
    }

    /// A per-dial room URL provider for `path` (e.g. `/chat2/{chatId}/ws`):
    /// the bearer is re-fetched before every connect, so reconnects after a
    /// token expiry present a fresh `?token=` instead of the boot-time one.
    pub fn room_url(&self, path: impl Into<String>) -> Arc<dyn cypher_sync::UrlProvider> {
        let ws_base = self.url.replacen("http", "ws", 1);
        Arc::new(EdgeRoomUrl {
            base: format!("{}{}", ws_base.trim_end_matches('/'), path.into()),
            token: self.token.clone(),
            device_id: self.device_id.clone(),
        })
    }
}

struct EdgeRoomUrl {
    base: String,
    token: Arc<dyn cypher_rpc::TokenSource>,
    device_id: String,
}

impl cypher_sync::UrlProvider for EdgeRoomUrl {
    fn url(&self) -> futures::future::BoxFuture<'static, Result<String, cypher_sync::SyncError>> {
        let token = self.token.clone();
        let base = self.base.clone();
        let device = self.device_id.clone();
        Box::pin(async move {
            let token = token.token().await.ok_or_else(|| {
                cypher_sync::SyncError::Auth("no access token (signed out)".into())
            })?;
            let mut url = format!("{base}?token={token}");
            if !device.is_empty() {
                url.push_str(&format!("&device={device}"));
            }
            Ok(url)
        })
    }
}

#[derive(Debug, Clone)]
pub struct DocHostConfig {
    pub device_id: String,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// When present, each opened chat joins its edge session room. `None` = fully
    /// offline operation (local snapshots only).
    pub edge: Option<EdgeConfig>,
}

struct DocHostInner {
    store: Arc<DocsStore>,
    config: DocHostConfig,
    /// Set-once (first wins), cleared by `shutdown_workers`: sessions and
    /// doc-host reference each other through Arcs, so a retired runtime's
    /// graph only drops once this back-edge is severed.
    sessions: Mutex<Option<SessionsEngine>>,
    workspace: OnceLock<WorkspaceHost>,
    /// Worktree materialization for Run commands (see `set_repos`).
    repos: OnceLock<crate::git::repos::Repos>,
    /// Cancels every worker spawned through `spawn_worker` — the loops'
    /// own exit conditions (weak handle death, closed channels) don't cover
    /// runtime replacement, where Edge-capable tasks must stop doing
    /// network work even while something still pins the graph.
    shutdown: CancellationToken,
    /// Tracks every spawned worker so `shutdown_workers` can await them.
    tasks: TaskTracker,
    handles: Mutex<HashMap<String, Arc<ChatDocHandle>>>,
    /// Command ids between the durable processed-ledger claim and their
    /// outcome write. A pending command in the ledger but not in this set
    /// after a restart is a dead attempt from the mark/execute crash window.
    executing: Mutex<HashSet<String>>,
    /// Shared client for sidecar blob fetches (30s timeout, uploads.rs
    /// discipline — diff_sync's untimed client hung on dead links).
    http: reqwest::Client,
}

fn same_message_identity(left: &SessionCommandPayload, right: &SessionCommandPayload) -> bool {
    match (left, right) {
        (
            SessionCommandPayload::Run {
                message_id: left, ..
            },
            SessionCommandPayload::Run {
                message_id: right, ..
            },
        ) => left == right,
        (
            SessionCommandPayload::Steer {
                message_id: Some(left),
                ..
            },
            SessionCommandPayload::Steer {
                message_id: Some(right),
                ..
            },
        ) => left == right,
        _ => false,
    }
}

#[derive(Clone)]
pub struct DocHost {
    inner: Arc<DocHostInner>,
}

/// One open chat doc: the `SessionDoc`, its change plumbing, and the room client.
pub struct ChatDocHandle {
    preview: OnceLock<Arc<cypher_sync::preview_link::PreviewLink>>,
    chat_id: String,
    device_id: String,
    doc: Arc<SessionDoc>,
    messages_tx: watch::Sender<Vec<SessionMessageEntry>>,
    /// Durable command ledger watch (WatchDocCommands): current value first,
    /// then re-sent on every doc change. Same lazy-mirror discipline as
    /// `messages_tx` — the ledger is only rebuilt while someone watches.
    commands_tx: watch::Sender<Vec<SessionCommandEntry>>,
    /// True when the doc changed while nobody watched the command ledger: the
    /// mirror rebuild is deferred to the next `watch_commands` attach.
    commands_dirty: AtomicBool,
    /// True when the doc changed while nobody watched: the mirror rebuild is
    /// deferred to the next `watch_messages` attach instead of paid per commit.
    mirror_dirty: AtomicBool,
    /// Epoch ms of the last open/watch touch — the LRU eviction key.
    last_access: AtomicI64,
    /// Last known snapshot blob size — the eviction budget estimate's input.
    snapshot_bytes: AtomicUsize,
    /// A threshold checkpoint POST is in flight (the quiesce
    /// tick must not stack concurrent full-snapshot uploads).
    checkpointing: Arc<AtomicBool>,
    /// Temporary Side Chat doc: a fresh in-memory `SessionDoc` with
    /// NO load/save/chat2/edge/eviction. Flipped false at promotion — the same
    /// handle then serves the normal chat (snapshot persisted, chat2 joined,
    /// maintenance runs) so a live run keeps streaming through the transition.
    ephemeral: AtomicBool,
    /// chat2 relay client (docs/chat2-sync.md C3) — populated once the
    /// registry names roomGen 2 for this chat and the join resolves.
    chat2: Mutex<Option<cypher_sync::ChatClient>>,
    /// Local commits made before the relay connects (the dial can take up
    /// to a minute; offline, forever): buffered here by the subscription
    /// below and drained into the client on join (a user
    /// message typed during the dial must not silently never sync).
    chat2_pending_local: Mutex<Vec<Vec<u8>>>,
    /// Local-update feed into the chat2 client (drop = unsubscribe).
    chat2_local_sub: Mutex<Option<loro::Subscription>>,
    /// Doc subscription (drop = unsubscribe) — bumps the change watch on every commit.
    _sub: loro::Subscription,
}

impl ChatDocHandle {
    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    pub fn doc(&self) -> &SessionDoc {
        &self.doc
    }

    pub fn doc_arc(&self) -> Arc<SessionDoc> {
        self.doc.clone()
    }

    /// True for a temporary Side Chat doc: host-memory only until promotion
    /// (no snapshot load/save, no chat2/edge room, no LRU eviction, no
    /// maintenance). See [`DocHost::open_ephemeral`].
    pub fn is_ephemeral(&self) -> bool {
        self.ephemeral.load(Ordering::Acquire)
    }

    /// Joined transcript watch — re-sent on every doc change (WatchDocMessages).
    ///
    /// Attach-time refresh: the mirror is only maintained while watched, so a
    /// doc that changed unwatched materializes here, once, instead of on every
    /// commit it sat through in the background.
    pub fn watch_messages(&self) -> watch::Receiver<Vec<SessionMessageEntry>> {
        self.touch();
        // Attach is a user signal: verify a quiet room is actually alive
        // (a doc-wedged DO keeps answering pings while delivering nothing,
        // and the background probe cadence can be hours out). Coalescing
        // no-op on a healthy or recently-active room.
        if let Some(chat2) = lock(&self.chat2).as_ref() {
            chat2.probe();
        }
        // Subscribe BEFORE the dirty check: a commit racing this attach then
        // sees a live receiver and publishes, instead of re-marking dirty
        // after our refresh and leaving the new watcher a cleared mirror.
        let rx = self.messages_tx.subscribe();
        if self.mirror_dirty.load(Ordering::Acquire) {
            self.publish_messages();
        }
        rx
    }

    /// Durable command ledger watch (WatchDocCommands): current value first,
    /// then re-sent on every doc change. The command ledger is the durable
    /// truth the UI projects Queued/Failed/Retrying from — command-only
    /// commits (a rejection, a retry) wake this watch even though the
    /// transcript delta is empty.
    pub fn watch_commands(&self) -> watch::Receiver<Vec<SessionCommandEntry>> {
        self.touch();
        // Subscribe BEFORE the dirty check, mirroring `watch_messages`: a
        // commit racing this attach then sees a live receiver and publishes.
        let rx = self.commands_tx.subscribe();
        if self.commands_dirty.load(Ordering::Acquire) {
            self.publish_commands();
        }
        rx
    }

    fn touch(&self) {
        self.last_access.store(now_ms(), Ordering::Relaxed);
    }

    pub fn connected(&self) -> bool {
        lock(&self.chat2).is_some()
    }

    /// Hand one local commit to the chat2 client, or park it until the join
    /// lands. Check-and-route happens under ONE client-lock critical section:
    /// the join stores the client and drains the pending buffer under the
    /// same lock, so a commit is either drained there or enqueued directly
    /// here — never dropped between. Called only from the feed
    /// pump task, never from inside a Loro hook (see
    /// [`DocHost::install_chat2_local_feed`]). A `deferred` commit (it only
    /// grew the model's thinking) rides the next push instead of opening one;
    /// before the join everything is sent on join anyway.
    fn route_local_update(&self, bytes: Vec<u8>, deferred: bool) {
        let client_guard = lock(&self.chat2);
        match &*client_guard {
            Some(client) if deferred => client.enqueue_deferred_update(bytes),
            Some(client) => client.enqueue_update(bytes),
            None => lock(&self.chat2_pending_local).push(bytes),
        }
    }

    /// Write a complete user message entry, idempotent by id (the client-minted message
    /// id — a re-executed command or optimistic echo never duplicates the entry).
    pub fn write_user_message(
        &self,
        message_id: &str,
        text: &str,
        created_at: i64,
    ) -> Result<(), DocError> {
        self.write_user_prompt(message_id, text, &[], created_at)
    }

    /// [`Self::write_user_message`] carrying the comments that rode the
    /// prompt (the Comment feature), so the transcript can show them.
    pub fn write_user_prompt(
        &self,
        message_id: &str,
        text: &str,
        comments: &[MessageComment],
        created_at: i64,
    ) -> Result<(), DocError> {
        if self.doc.read_entries()?.iter().any(|e| e.id == message_id) {
            return Ok(());
        }
        self.doc.push_message(&SessionMessageEntry {
            id: message_id.to_string(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "t0".into(),
                text: text.to_string(),
                agent_text: None,
            }],
            created_at,
            device_id: self.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
            completed_at: None,
            comments: comments.to_vec(),
            models: Vec::new(),
        })
    }

    /// Recovery sweep: stamp this device's abandoned `streaming` entries `aborted`, appending
    /// `note` as a visible error part so the transcript says WHY the turn
    /// ended (zeron folded "Run interrupted by backend restart" the same
    /// way). Returns the stamped entries' `(id, created_at)` — recovery uses
    /// them for the resume-freshness check.
    pub fn mark_abandoned_streams(&self, note: &str) -> Result<Vec<(String, i64)>, DocError> {
        let mut stamped = Vec::new();
        for entry in self.doc.read_entries()? {
            if entry.role == MessageRole::Assistant
                && entry.status == Some(MessageStatus::Streaming)
                && entry.device_id == self.device_id
                && self
                    .doc
                    .set_message_status(&entry.id, MessageStatus::Aborted)?
            {
                let part_id = format!("{}-recovery", entry.id);
                if let Err(err) = self.doc.append_error_part(&entry.id, &part_id, note) {
                    tracing::warn!(chat = %self.chat_id, error = %err, "recovery note append failed");
                }
                stamped.push((entry.id.clone(), entry.created_at));
            }
        }
        if !stamped.is_empty() {
            self.publish_messages();
        }
        Ok(stamped)
    }

    fn publish_messages(&self) {
        self.mirror_dirty.store(false, Ordering::Release);
        // Read coverage BEFORE the entries: a newer marker must never retire
        // preview text against an older, asynchronously materialized transcript.
        let coverage = self.preview.get().and_then(|_| self.doc.preview_coverage());
        match self.doc.read_entries() {
            Ok(mut entries) => {
                if let Some(preview) = self.preview.get() {
                    cypher_sync::preview_link::overlay(&mut entries, preview.view(), coverage);
                }
                let joined = join_continuation_entries(entries);
                // send_replace: update the watch even with no subscribers yet, so a
                // late subscriber's first borrow sees the current transcript.
                self.messages_tx.send_replace(joined);
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "transcript read failed");
            }
        }
    }

    /// Per-commit publish path: unwatched docs just mark the mirror dirty —
    /// rebuilding a full transcript nobody reads was a per-tick cost on every
    /// open doc (and kept a second transcript copy hot).
    fn publish_messages_if_watched(&self) {
        if self.messages_tx.receiver_count() == 0 {
            self.mirror_dirty.store(true, Ordering::Release);
            // Shrink the stale mirror: watch_messages rebuilds on attach.
            self.messages_tx.send_replace(Vec::new());
        } else {
            self.publish_messages();
        }
    }

    fn publish_commands(&self) {
        self.commands_dirty.store(false, Ordering::Release);
        match self.doc.read_commands() {
            Ok(commands) => {
                self.commands_tx.send_replace(commands);
            }
            Err(err) => {
                tracing::warn!(chat = %self.chat_id, error = %err, "command ledger read failed");
            }
        }
    }

    /// Per-commit command publish, mirroring [`Self::publish_messages_if_watched`]:
    /// unwatched docs just mark the ledger dirty and hand the next attach a
    /// fresh materialization.
    fn publish_commands_if_watched(&self) {
        if self.commands_tx.receiver_count() == 0 {
            self.commands_dirty.store(true, Ordering::Release);
            // Shrink the stale mirror: watch_commands rebuilds on attach.
            self.commands_tx.send_replace(Vec::new());
        } else {
            self.publish_commands();
        }
    }

    /// Rough resident cost for the LRU budget.
    fn resident_estimate(&self) -> usize {
        (self.snapshot_bytes.load(Ordering::Relaxed) * RESIDENT_BYTES_PER_SNAPSHOT_BYTE)
            .max(DOC_RESIDENT_FLOOR_BYTES)
    }
}

impl DocHost {
    pub fn new(store: Arc<DocsStore>, config: DocHostConfig) -> Self {
        Self {
            inner: Arc::new(DocHostInner {
                store,
                config,
                sessions: Mutex::new(None),
                workspace: OnceLock::new(),
                repos: OnceLock::new(),
                shutdown: CancellationToken::new(),
                tasks: TaskTracker::new(),
                handles: Mutex::new(HashMap::new()),
                executing: Mutex::new(HashSet::new()),
                http: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap_or_else(|_| reqwest::Client::new()),
            }),
        }
    }

    /// Every background task rides the tracker, raced against the shutdown
    /// token: the loops' own exits stay authoritative in normal operation;
    /// the token is the retirement override.
    fn spawn_worker(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = fut => {}
            }
        });
    }

    /// `spawn_worker` for sites that pre-resolve a runtime handle (callers
    /// reachable from bare sync contexts, where `tasks.spawn` would panic).
    fn spawn_worker_on(
        &self,
        runtime: &tokio::runtime::Handle,
        fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let cancel = self.inner.shutdown.clone();
        self.inner.tasks.spawn_on(
            async move {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = fut => {}
                }
            },
            runtime,
        );
    }

    /// The sessions engine, once wired. `None` before assembly or after
    /// `shutdown_workers` — callers treat both as "executor unavailable".
    fn sessions(&self) -> Option<SessionsEngine> {
        lock(&self.inner.sessions).clone()
    }

    /// Wire the sessions engine (engine assembly; see `SessionsEngine::set_doc_host`).
    pub fn set_sessions(&self, sessions: SessionsEngine) {
        {
            // First set wins (the OnceLock contract this slot replaced).
            let mut slot = lock(&self.inner.sessions);
            if slot.is_none() {
                *slot = Some(sessions);
            }
        }
        // Commands may already be pending in warm-opened docs.
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            let host = self.clone();
            self.spawn_worker(async move { host.drain_commands(&handle).await });
        }
    }

    /// Retire this host's workers (runtime replacement, e.g. sign-out): cancel
    /// and await every spawned task, drop every open chat handle (ending the
    /// weak-keyed room/join loops and watcher streams), and sever the sessions
    /// back-edge so the replaced engine graph can actually drop. Idempotent.
    pub async fn shutdown_workers(&self) {
        self.inner.shutdown.cancel();
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
        // Snapshot open docs BEFORE releasing their handles: the handles map
        // holds the only strong doc refs, and an unflushed doc dies with it.
        self.flush_all();
        // Take the map under the lock, drop the handles outside it.
        let handles = std::mem::take(&mut *lock(&self.inner.handles));
        drop(handles);
        lock(&self.inner.sessions).take();
    }

    /// Test seam: a retirement sentinel that reports true once the doc-host graph
    /// has actually been freed.
    #[doc(hidden)]
    pub fn retirement_probe(&self) -> Box<dyn Fn() -> bool + Send + Sync> {
        let weak = Arc::downgrade(&self.inner);
        Box::new(move || weak.upgrade().is_none())
    }

    /// Wire the repos engine (engine assembly) — worktree materialization for
    /// Run commands carrying a [`cypher_proto::WorktreeSpec`].
    pub fn set_repos(&self, repos: crate::git::repos::Repos) {
        let _ = self.inner.repos.set(repos);
    }

    /// Wire the workspace host (engine assembly) — the source of chat-ownership rows.
    pub fn set_workspace(&self, workspace: WorkspaceHost) {
        let _ = self.inner.workspace.set(workspace);
    }

    /// The workspace host, once wired (tests may assemble a DocHost without one).
    pub fn workspace(&self) -> Option<&WorkspaceHost> {
        self.inner.workspace.get()
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    /// Open (or return) the chat's doc handle: load the local snapshot (or init fresh),
    /// start the change-driven task, and join the edge room when configured.
    pub fn open(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        // Every chat syncs through its chat2 room. The registry's `roomGen`
        // stays on the wire (iOS gates on it), but the host no longer reads
        // it: a row still marked gen 1, or no row at all (a chat being born
        // while its CreateChat mint races this open), opens as chat2 too.
        {
            let handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                handle.touch();
                return Ok(handle.clone());
            }
        }
        let stored = self.inner.store.load_snapshot_with_cursor(chat_id)?;
        let mut snapshot_len = 0usize;
        let mut chat2_cursor = 0u64;
        let mut requeue_commands: Vec<SessionCommandEntry> = Vec::new();
        let doc = match stored {
            Some((bytes, cursor, epoch)) if epoch >= crate::host::chat2_host::CHAT2_DOC_EPOCH => {
                snapshot_len = bytes.len();
                chat2_cursor = cursor;
                let raw = loro::LoroDoc::new();
                raw.import(&bytes)
                    .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                SessionDoc::from_doc(raw)
            }
            Some((bytes, _cursor, epoch)) if self.inner.config.edge.is_none() => {
                // Offline/edge-less: adopting would blank a readable
                // transcript with no way to catch up. Keep the old doc
                // read-only-ish; the adopt runs on the next online open.
                tracing::info!(chat = %chat_id, old_epoch = epoch,
                    "chat2 adopt deferred (no edge configured)");
                snapshot_len = bytes.len();
                let raw = loro::LoroDoc::new();
                raw.import(&bytes)
                    .map_err(|e| EngineError::Other(format!("snapshot import failed: {e}")))?;
                SessionDoc::from_doc(raw)
            }
            Some((bytes, _cursor, epoch)) => {
                // Discard-and-adopt: this device's doc predates the
                // chat2 lineage. Keep the old snapshot under a suffixed
                // id for rollback, carry over OUR OWN unresolved
                // commands, and start fresh — the chat2 catch-up
                // (checkpoint + rows) repopulates the transcript. This
                // is the self-repair path: no user action, ever.
                tracing::info!(chat = %chat_id, old_epoch = epoch,
                    "chat2 adopt: discarding pre-chat2 local doc (rollback copy kept)");
                let rollback_id = format!("{chat_id}.pre-chat2");
                // A re-adopt after a mid-catch-up crash reruns this path
                // with a near-empty doc under `chat_id` — the FIRST
                // rollback copy is the real transcript; never overwrite
                // it.
                if matches!(self.inner.store.load_snapshot(&rollback_id), Ok(None)) {
                    let _ = self.inner.store.save_snapshot(&rollback_id, &bytes);
                }
                if let Ok(raw) = {
                    let old = loro::LoroDoc::new();
                    old.import(&bytes).map(|_| old)
                } {
                    let old_doc = SessionDoc::from_doc(raw);
                    if let Ok(commands) = old_doc.read_commands() {
                        requeue_commands = commands
                            .into_iter()
                            .filter(|c| {
                                c.status == SessionCommandStatus::Pending
                                    && c.issued_by == self.inner.config.device_id
                            })
                            .collect();
                    }
                }
                SessionDoc::init(chat_id)?
            }
            None => {
                // Born on chat2 (or a cold reader's first open): stamp
                // the epoch-2 lineage NOW. Plain snapshot saves preserve
                // an existing row's epoch but default a NEW row to 0 —
                // without this stamp, the next open reads "pre-chat2
                // doc" and the adopt DISCARDS everything written
                // since (caught by the restart_resume suite: first-turn
                // transcripts vanished on reopen).
                let doc = SessionDoc::init(chat_id)?;
                if let Ok(snapshot) = doc.export_snapshot() {
                    let _ = self.inner.store.save_snapshot_with_cursor(
                        chat_id,
                        &snapshot,
                        0,
                        crate::host::chat2_host::CHAT2_DOC_EPOCH,
                    );
                }
                doc
            }
        };
        // Replay before installing subscriptions or handing the document to
        // journal recovery. This works offline, not just on a successful dial.
        // Never advance the cloud cursor for local replay.
        for (_, bytes) in self.inner.store.load_outbox(chat_id)? {
            doc.doc()
                .import(&bytes)
                .map_err(|e| EngineError::Other(format!("outbox recovery import failed: {e}")))?;
        }
        let doc = Arc::new(doc);

        let (changed_tx, changed_rx) = watch::channel(0u64);
        let sub = doc.doc().subscribe_root(Arc::new(move |_diff| {
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        // The mirror starts dirty and empty: many opens (command queueing,
        // drains, nudges) never watch the transcript, and the first
        // watch_messages attach materializes it on demand.
        let (messages_tx, _) = watch::channel(Vec::new());
        let (commands_tx, _) = watch::channel(Vec::new());

        let handle = Arc::new(ChatDocHandle {
            preview: OnceLock::new(),
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            commands_tx,
            commands_dirty: AtomicBool::new(true),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(snapshot_len),
            ephemeral: AtomicBool::new(false),
            checkpointing: Arc::new(AtomicBool::new(false)),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
        });
        {
            let mut handles = lock(&self.inner.handles);
            if let Some(existing) = handles.get(chat_id) {
                return Ok(existing.clone()); // racing open — keep the first
            }
            handles.insert(chat_id.to_string(), handle.clone());
        }

        // Edge room join — offline-tolerant AND supervised. `ChatClient` only
        // self-reconnects AFTER a first successful join; a one-shot attempt
        // here (the pre-LRU design) left the doc silently local-only until
        // app restart whenever the dial hit a transient gap — a post-wake
        // network, `Auth::token()` momentarily `None` around a refresh, an
        // edge deploy. The LRU made that dice-roll constant (every reopen),
        // and a watched doc is pinned against eviction, so nothing ever
        // retried: the exact "transcript frozen until restart" report.
        // Retry on the workspace host's capped, jittered backoff; a system
        // wake redials immediately; eviction/purge ends the loop via `weak`.
        if let Some(edge) = &self.inner.config.edge {
            // Subscription BEFORE the dial: every local
            // commit lands in the client when connected, else in the
            // pending buffer the join drains — nothing composed during
            // (or before) the dial is lost to the room.
            self.install_chat2_local_feed(&handle);
            // Re-queue survives the adopt: our own pending commands
            // become fresh entries in the new lineage (the
            // processed_commands ledger still guards double execution).
            // Committed AFTER the local-update subscription above — a
            // commit before it never enters the pending buffer or the
            // client, so the requeued command would sit in the local doc
            // and never reach the room (the host would never see it).
            for command in &requeue_commands {
                let _ = doc.queue_command(command);
            }
            // First contact with the room (cursor 0): everything
            // committed BEFORE the subscription above — SessionDoc::
            // init's container/meta ops, an adopt's fresh doc — is
            // invisible to the push path, yet every later commit
            // causally DEPENDS on it. Rows built on unpushed deps import
            // into peers' loro pending-buffers and never materialize:
            // born-chat2 cross-device runs sat invisible on every other
            // device (host never saw the command, viewers never saw the
            // transcript). Push the doc's full update log as the join's
            // first batch; once acked the cursor moves and this never
            // re-arms.
            if chat2_cursor == 0 {
                match doc
                    .doc()
                    .export(loro::ExportMode::updates(&loro::VersionVector::default()))
                {
                    Ok(bytes) if !bytes.is_empty() => {
                        lock(&handle.chat2_pending_local).push(bytes);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err,
                            "chat2 first-contact export failed; peers may stall on missing deps");
                    }
                }
            }
            self.spawn_chat2_join(edge.clone(), &handle, chat2_cursor);
        }
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        self.evict_over_budget();
        Ok(handle)
    }

    /// Open (or return) a temporary Side Chat doc: a FRESH
    /// in-memory [`SessionDoc`] with no snapshot load/save, no chat2/edge
    /// room, no maintenance and no LRU eviction — host-memory only until
    /// promotion. The handle is registered in the same map so `WatchDocMessages`
    /// (which routes through [`Self::open`]) streams its transcript, and
    /// `purge_chat` tears it down. The worker is the standard `chat_task`,
    /// which skips persistence for ephemeral handles.
    pub fn open_ephemeral(&self, chat_id: &str) -> Result<Arc<ChatDocHandle>, EngineError> {
        {
            let handles = lock(&self.inner.handles);
            if let Some(handle) = handles.get(chat_id) {
                handle.touch();
                return Ok(handle.clone());
            }
        }
        let doc = Arc::new(SessionDoc::init(chat_id)?);
        let (changed_tx, changed_rx) = watch::channel(0u64);
        let sub = doc.doc().subscribe_root(Arc::new(move |_diff| {
            changed_tx.send_modify(|v| *v = v.wrapping_add(1));
        }));
        let (messages_tx, _) = watch::channel(Vec::new());
        let (commands_tx, _) = watch::channel(Vec::new());
        let handle = Arc::new(ChatDocHandle {
            preview: OnceLock::new(),
            chat_id: chat_id.to_string(),
            device_id: self.inner.config.device_id.clone(),
            doc: doc.clone(),
            messages_tx,
            commands_tx,
            commands_dirty: AtomicBool::new(true),
            mirror_dirty: AtomicBool::new(true),
            last_access: AtomicI64::new(now_ms()),
            snapshot_bytes: AtomicUsize::new(0),
            ephemeral: AtomicBool::new(true),
            checkpointing: Arc::new(AtomicBool::new(false)),
            chat2: Mutex::new(None),
            chat2_pending_local: Mutex::new(Vec::new()),
            chat2_local_sub: Mutex::new(None),
            _sub: sub,
        });
        {
            let mut handles = lock(&self.inner.handles);
            if let Some(existing) = handles.get(chat_id) {
                return Ok(existing.clone()); // racing open — keep the first
            }
            handles.insert(chat_id.to_string(), handle.clone());
        }
        self.spawn_worker(chat_task(self.clone(), Arc::downgrade(&handle), changed_rx));
        Ok(handle)
    }

    /// Watcher-count helper for the stale reaper: how many live transcript-
    /// watch receivers this chat's doc has. A Side Chat panel holds one while
    /// open; the count drops to zero once the tab closes (or the watch RPC
    /// was lost).
    pub fn transcript_watcher_count(&self, chat_id: &str) -> usize {
        lock(&self.inner.handles)
            .get(chat_id)
            .map_or(0, |h| h.messages_tx.receiver_count())
    }

    /// Promotion step 1: persist the temporary doc's
    /// transcript as an epoch-2 snapshot. MUST succeed before any row exposes
    /// the chat — a promotion never leaves a row whose transcript could be
    /// lost (a failed save FAILS the promotion rather than warning and
    /// exposing a lost transcript). Idempotent: an already-promoted handle is
    /// a no-op. The [`Self::finish_promotion`] flip + room join runs only
    /// after the workspace row exists.
    pub fn prepare_promotion(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = lock(&self.inner.handles)
            .get(chat_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("no open doc to promote".into()))?;
        if !handle.ephemeral.load(Ordering::Acquire) {
            return Ok(()); // already promoted
        }
        let snapshot = handle.doc.export_snapshot()?;
        self.inner.store.save_snapshot_with_cursor(
            chat_id,
            &snapshot,
            0,
            crate::host::chat2_host::CHAT2_DOC_EPOCH,
        )?;
        Ok(())
    }

    /// Promote a temporary Side Chat doc into a normal chat doc:
    /// flip the SAME handle to non-ephemeral (so the live run keeps streaming
    /// through the transition and future maintenance/flush/eviction treat it
    /// as a real chat), and join the chat2 room. The transcript snapshot is
    /// persisted by [`Self::prepare_promotion`] BEFORE the workspace row
    /// existed. Idempotent — a repeated call after promotion is a no-op.
    /// Filesystem/sidecar side effects performed before promotion are
    /// deliberately NOT rolled back on a later dispose (dispose-after-promote
    /// is a no-op by contract).
    pub fn finish_promotion(&self, chat_id: &str) -> Result<(), EngineError> {
        let handle = lock(&self.inner.handles)
            .get(chat_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("no open doc to promote".into()))?;
        if !handle.ephemeral.load(Ordering::Acquire) {
            return Ok(()); // already promoted
        }
        // Flip BEFORE the room join: the join's own writes/checkpoints must
        // observe a normal (non-ephemeral) handle.
        handle.ephemeral.store(false, Ordering::Release);
        if let Some(edge) = &self.inner.config.edge {
            // Subscription BEFORE the dial: every local commit
            // lands in the client when connected, else in the pending buffer
            // the join drains — nothing composed during the dial is lost.
            self.install_chat2_local_feed(&handle);
            // First contact with the room: everything committed before the
            // subscription above must reach the room as the join's first
            // batch (peers import rows only after their causal deps land).
            match handle
                .doc
                .doc()
                .export(loro::ExportMode::updates(&loro::VersionVector::default()))
            {
                Ok(bytes) if !bytes.is_empty() => {
                    lock(&handle.chat2_pending_local).push(bytes);
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err,
                        "promotion chat2 first-contact export failed");
                }
            }
            self.spawn_chat2_join(edge.clone(), &handle, 0);
        }
        Ok(())
    }

    /// LRU eviction: while the warm set exceeds [`WARM_DOC_CAP`] or the
    /// resident estimate exceeds `DOC_LRU_BYTE_BUDGET`, close the
    /// least-recently-touched unpinned docs. Pinned (never evicted):
    /// - watched docs (`messages_tx` has receivers — a UI transcript);
    /// - docs with a live writer (`Arc<SessionDoc>` held outside the handle —
    ///   a run streaming into it);
    /// - host-side docs with pending commands (the executor owes them work).
    ///
    /// Eviction flushes a final snapshot, so reopen loses nothing; missed
    /// remote updates re-arrive through the room join's VV backfill.
    fn evict_over_budget(&self) {
        let mut by_age: Vec<(i64, String)> = {
            let handles = lock(&self.inner.handles);
            handles
                .values()
                .map(|h| (h.last_access.load(Ordering::Relaxed), h.chat_id.clone()))
                .collect()
        };
        by_age.sort_unstable();
        for (last_access, chat_id) in by_age {
            if now_ms() - last_access < EVICT_MIN_IDLE_MS {
                // Sorted oldest-first: everything after this is younger.
                return;
            }
            let (count, estimate) = {
                let handles = lock(&self.inner.handles);
                (
                    handles.len(),
                    handles
                        .values()
                        .map(|h| h.resident_estimate())
                        .sum::<usize>(),
                )
            };
            if count <= WARM_DOC_CAP && estimate <= cypher_doc::DOC_LRU_BYTE_BUDGET {
                return;
            }
            let evicted = {
                let mut handles = lock(&self.inner.handles);
                match handles.get(&chat_id) {
                    Some(handle)
                        if !self.pinned(handle) && !handle.ephemeral.load(Ordering::Acquire) =>
                    {
                        handles.remove(&chat_id)
                    }
                    _ => None,
                }
            };
            if let Some(handle) = evicted {
                // Final flush outside the map lock; ≤1s of changes could be
                // pending in the snapshot debounce.
                self.save_snapshot(&handle);
                tracing::debug!(chat = %handle.chat_id, "doc evicted (LRU)");
            }
        }
    }

    fn pinned(&self, handle: &Arc<ChatDocHandle>) -> bool {
        if handle.messages_tx.receiver_count() > 0 {
            return true;
        }
        // The handle itself holds one doc ref; more means a live writer.
        if Arc::strong_count(&handle.doc) > 1 {
            return true;
        }
        if self.is_host(&handle.chat_id) {
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);
            match handle.doc.read_commands() {
                Ok(commands) => commands
                    .iter()
                    .any(|c| c.status == SessionCommandStatus::Pending && !is_processed(&c.id)),
                // Unreadable ledger: keep the doc, never evict blind.
                Err(_) => true,
            }
        } else {
            false
        }
    }

    /// Probe every open chat's room (window-focus liveness sweep). Each
    /// room ignores the hint unless it has been broadcast-quiet ≥30s.
    pub fn probe_open_chats(&self) {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            // chat2 rooms verify liveness on user signals — a
            // deaf-but-ponging DO otherwise freezes a watched transcript
            // for the whole background probe quiet window.
            if let Some(chat2) = lock(&handle.chat2).as_ref() {
                chat2.probe();
            }
        }
    }

    /// Per-open-chat room introspection for SyncStatus / `cypher sync`.
    /// `None` room = still dialing (join retry loop) or edge-less.
    pub fn sync_statuses(&self) -> Vec<(String, Option<cypher_sync::ChatStatsSnapshot>)> {
        let handles: Vec<Arc<ChatDocHandle>> =
            lock(&self.inner.handles).values().cloned().collect();
        let mut rows: Vec<(String, Option<cypher_sync::ChatStatsSnapshot>)> = handles
            .iter()
            .map(|h| {
                (
                    h.chat_id.clone(),
                    lock(&h.chat2).as_ref().map(|client| client.stats()),
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Drop a chat's doc unconditionally and delete its local snapshot — the
    /// chat is gone (DeleteChat / DeleteSpace cascade). Watchers see the
    /// stream end; a racing writer keeps its orphaned doc until the run ends.
    pub fn purge_chat(&self, chat_id: &str) {
        let removed = lock(&self.inner.handles).remove(chat_id);
        drop(removed);
        if let Err(err) = self.inner.store.delete_snapshot(chat_id) {
            tracing::warn!(chat = %chat_id, error = %err, "snapshot delete failed");
        }
    }

    /// Host-side seal of one upload against a chat (the UploadCommit handler):
    /// write the durable final path into the doc's `sealedAttachments` map.
    /// The doc commit re-triggers the chat's drain, releasing any Run whose
    /// `pending_attachments` names this upload id — the queue-first ordering's
    /// release valve. Best-effort: a chat this device doesn't host isn't open
    /// here (the uploader targeted the host, so this only happens when a chat
    /// moved hosts mid-upload); the id simply stays unsealed and the Run's
    /// grace window resolves it.
    pub fn seal_attachment(&self, chat_id: &str, upload_id: &str, path: &str, file_name: &str) {
        if !self.is_host(chat_id) {
            tracing::warn!(
                chat = %chat_id,
                upload_id,
                "attachment seal skipped: chat not hosted here"
            );
            return;
        }
        let handle = match self.open(chat_id) {
            Ok(handle) => handle,
            Err(err) => {
                tracing::warn!(chat = %chat_id, upload_id, error = %err, "seal: open failed");
                return;
            }
        };
        if let Err(err) = handle.doc.seal_attachment(upload_id, path, file_name) {
            tracing::warn!(chat = %chat_id, upload_id, error = %err, "seal write failed");
        } else {
            // A newly-created top-level Loro map is not guaranteed to wake
            // every root subscription on older runtimes. Explicitly kick the
            // host drain as well as relying on the doc change notification so
            // a queued Run cannot remain parked after its final upload seals.
            let host = self.clone();
            tokio::spawn(async move {
                host.drain_commands(&handle).await;
            });
        }
    }

    /// Composer path: append an immutable pending command entry (rule 1). Durable by
    /// construction — the change subscription kicks the drain, so a local host executes
    /// immediately and an offline doc simply holds the entry until it syncs.
    pub fn queue_command(
        &self,
        chat_id: &str,
        payload: SessionCommandPayload,
    ) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let id = new_id();
        let now = now_ms();
        let based_on = handle.doc.read_entries()?.last().map(|m| CommandBasedOn {
            turn_id: Some(m.id.clone()),
            frontier: None,
        });
        let is_message = matches!(
            payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        );
        handle.doc.queue_command(&SessionCommandEntry {
            id: id.clone(),
            payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on,
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
            // The first attempt IS the original send: the UI uses sent_at as
            // the stable message-send clock across retries.
            sent_at: Some(now),
        })?;
        // Sending a message revives an archived chat: the user is acting in it
        // again, so the LWW row flips back to active on every device. Best-
        // effort — the command itself is durable regardless.
        if is_message && let Some(workspace) = self.workspace() {
            match workspace.chat(chat_id) {
                Ok(Some(chat)) if chat.archived => {
                    if let Err(err) = workspace.set_chat_archived(chat_id, false) {
                        tracing::warn!(chat = %chat_id, error = %err, "unarchive on send failed");
                    }
                }
                _ => {}
            }
        }
        // §7 durable delivery: when another device hosts this chat, nudge its device
        // room so a cold host opens the doc and drains the queue. Fire-and-forget —
        // the command is durable in the doc either way (a host that opens the chat
        // for any other reason still executes it).
        self.nudge_remote_host(chat_id);
        Ok(id)
    }

    /// Re-issue a failed or expired message command as a fresh durable
    /// attempt. The logical message id remains stable so the executor's
    /// idempotent user-entry write cannot duplicate the transcript, while the
    /// command id is new so the processed ledger does not suppress the retry.
    /// The retry inherits the ORIGINAL `sent_at` (the user's send clock) —
    /// only `issued_at` moves forward.
    pub fn retry_command(&self, chat_id: &str, command_id: &str) -> Result<String, EngineError> {
        let handle = self.open(chat_id)?;
        let commands = handle.doc.read_commands()?;
        let old = commands
            .iter()
            .find(|command| command.id == command_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("command not found".into()))?;
        if !matches!(
            old.status,
            SessionCommandStatus::Rejected | SessionCommandStatus::Expired
        ) {
            return Err(EngineError::Other(
                "only failed or expired commands can be retried".into(),
            ));
        }
        if !matches!(
            &old.payload,
            SessionCommandPayload::Run { .. } | SessionCommandPayload::Steer { .. }
        ) {
            return Err(EngineError::Other(
                "only message commands can be retried".into(),
            ));
        }
        let has_live_attempt = |candidate: &SessionCommandEntry| {
            commands.iter().any(|other| {
                other.id != candidate.id
                    && other.status == SessionCommandStatus::Pending
                    && !self.inner.store.is_processed(&other.id).unwrap_or(false)
                    && same_message_identity(&other.payload, &candidate.payload)
            })
        };
        if has_live_attempt(&old) {
            return Err(EngineError::Other(
                "a retry for this message is already pending".into(),
            ));
        }
        let now = now_ms();
        let retry = SessionCommandEntry {
            id: new_id(),
            payload: old.payload,
            issued_by: self.inner.config.device_id.clone(),
            issued_at: now,
            based_on: handle
                .doc
                .read_entries()?
                .last()
                .map(|message| CommandBasedOn {
                    turn_id: Some(message.id.clone()),
                    frontier: None,
                }),
            expires_at: Some(now + COMMAND_DEFAULT_TTL_MS),
            status: SessionCommandStatus::Pending,
            resolution: None,
            // The user's original send time, not the retry's: the message's
            // place in the transcript/order is set by when it was first sent.
            sent_at: old.sent_at.or(Some(old.issued_at)),
        };
        let retry_id = retry.id.clone();
        handle.doc.queue_command(&retry)?;
        self.nudge_remote_host(chat_id);
        Ok(retry_id)
    }

    /// POST `{edge}/device/{host}/nudge {chatId}` when the chat's workspace row names
    /// another device as host. Best-effort: offline/edge-less engines skip silently.
    fn nudge_remote_host(&self, chat_id: &str) {
        let Some(edge) = self.inner.config.edge.clone() else {
            return;
        };
        let Some(workspace) = self.workspace() else {
            return;
        };
        let host_device = match workspace.chat(chat_id) {
            Ok(Some(chat)) => chat.device_id,
            // Unclaimed chat: whoever drains first claims it — nobody to nudge.
            _ => return,
        };
        if host_device == self.inner.config.device_id {
            return;
        }
        // Only meaningful inside a runtime (RPC handlers, executors); bare sync
        // callers (unit tests) skip rather than panic.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let url = format!(
            "{}/device/{}/nudge",
            edge.url.trim_end_matches('/'),
            host_device
        );
        let chat = chat_id.to_string();
        self.spawn_worker_on(&runtime, async move {
            // Fresh bearer per request — never the boot-time snapshot.
            let Some(bearer) = edge.bearer().await else {
                tracing::warn!(chat = %chat, "nudge skipped: signed out");
                return;
            };
            let send = reqwest::Client::new()
                .post(&url)
                .bearer_auth(&bearer)
                .json(&serde_json::json!({ "chatId": chat }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
            match send {
                Ok(res) if res.status().is_success() => {
                    tracing::info!(chat = %chat, device = %host_device, "host nudged");
                }
                Ok(res) => tracing::warn!(chat = %chat, device = %host_device,
                    status = res.status().as_u16(), "nudge rejected"),
                Err(err) => {
                    tracing::warn!(chat = %chat, error = %err, "nudge failed (best-effort)")
                }
            }
        });
    }

    /// Fetch a sidecar blob by its doc-resident ref (`{chatId}/{partId}` or
    /// `…​.diff`) — the UI's lazy "Show full output" path, served over RPC
    /// because the UI crate has no HTTP client or edge bearer.
    pub async fn fetch_tool_blob(&self, blob_ref: &str) -> Result<String, EngineError> {
        // The `{chatId}/{partId}[.diff]` shape of doc-resident refs; anything
        // else is a forged ref.
        let valid = blob_ref.split_once('/').is_some_and(|(chat, part)| {
            !chat.is_empty()
                && chat.len() <= 128
                && chat
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                && !part.is_empty()
                && part.len() <= 200
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:#~-".contains(&b))
        });
        if !valid {
            return Err(EngineError::Other(format!("bad blob ref: {blob_ref}")));
        }
        let Some(edge) = self.inner.config.edge.clone() else {
            return Err(EngineError::Other("offline: no edge configured".into()));
        };
        let Some(bearer) = edge.bearer().await else {
            return Err(EngineError::Other("signed out".into()));
        };
        // `valid` above guarantees the split; re-split to encode the part
        // segment for transport (PART_RE allows `#`, which a raw URL would
        // truncate as a fragment and silently collide).
        let (chat, part) = blob_ref.split_once('/').expect("validated above");
        let url = format!(
            "{}/blob/{}/{}",
            edge.url.trim_end_matches('/'),
            chat,
            encode_part_segment(part)
        );
        let res = self
            .inner
            .http
            .get(&url)
            .bearer_auth(&bearer)
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("sidecar fetch failed: {e}")))?;
        if !res.status().is_success() {
            return Err(EngineError::Other(format!(
                "sidecar fetch: HTTP {}",
                res.status().as_u16()
            )));
        }
        res.text()
            .await
            .map_err(|e| EngineError::Other(format!("sidecar body read failed: {e}")))
    }

    /// §2.2 writer discipline: we host a chat iff its workspace row's `deviceId` is
    /// ours; a chat with no row is claimable (claim-on-first-command). Without a
    /// wired workspace host (bare-DocHost tests) every open chat is ours — M2's
    /// behavior, now the degenerate case.
    fn is_host(&self, chat_id: &str) -> bool {
        self.workspace().is_none_or(|ws| ws.is_host(chat_id))
    }

    pub(crate) fn preview_run(&self, chat: &str, run: &str) {
        let Some(handle) = lock(&self.inner.handles).get(chat).cloned() else {
            return;
        };
        let Some(preview) = handle.preview.get().cloned() else {
            return;
        };
        preview.set_run(run);
        if !self.preview_is_host(chat) {
            if preview.options().publisher_token.is_some() {
                preview.set_publisher(None);
                if let Some(client) = lock(&handle.chat2).as_ref() {
                    client.redial();
                }
            }
            return;
        }
        // WatchDocMessages may open a newborn chat BEFORE CreateChat inserts
        // its registry row. Upgrade only once local ownership is known, keeping
        // the same ChatClient/outbox rather than discarding pending updates.
        if preview.options().publisher_token.is_none()
            && let Some(token) = self
                .inner
                .config
                .edge
                .as_ref()
                .and_then(|e| e.preview.as_ref())
                .and_then(|p| p.publisher_token.clone())
        {
            preview.set_publisher(Some(token));
            let hook = preview.clone();
            handle
                .doc
                .set_preview_hook(Arc::new(move |entry, parts, done| {
                    hook.stage(entry, parts, done)
                }));
            if let Some(client) = lock(&handle.chat2).as_ref() {
                client.redial();
            }
        }
    }

    /// Force the accumulated durable batch at a business boundary. This only
    /// releases the ChatClient's two-second coalescing gate; the outbox and
    /// ACK rules remain unchanged.
    pub(crate) fn flush_chat_sync(&self, chat: &str) {
        let handle = lock(&self.inner.handles).get(chat).cloned();
        if let Some(handle) = handle
            && let Some(client) = lock(&handle.chat2).as_ref()
        {
            client.flush_pending();
        }
    }

    fn preview_is_host(&self, chat: &str) -> bool {
        // Unlike legacy command claim-on-first-use, preview publishing fails
        // closed for missing/unreadable registry rows in a workspace runtime.
        self.workspace().is_none_or(|ws| {
            ws.chat(chat)
                .ok()
                .flatten()
                .is_some_and(|row| row.device_id == self.inner.config.device_id)
        })
    }

    /// Chat-config harness when the workspace row carries one, else the default.
    pub(crate) fn harness_for(&self, chat_id: &str) -> HarnessId {
        self.workspace()
            .and_then(|ws| ws.chat_config(chat_id))
            .map(|config| config.harness)
            .unwrap_or(self.inner.config.default_harness)
    }

    /// The harness a request dispatches on: the request's own pick when it
    /// carries one (rides the command plane, immune to registry-row races),
    /// else [`Self::harness_for`].
    pub(crate) fn harness_for_request(
        &self,
        chat_id: &str,
        request: &cypher_proto::RunRequest,
    ) -> HarnessId {
        request.harness.unwrap_or_else(|| self.harness_for(chat_id))
    }

    /// Drain pending commands (host-only): evaluate → mark processed BEFORE execute →
    /// execute → write the outcome as the sole outcome writer.
    pub async fn drain_commands(&self, handle: &Arc<ChatDocHandle>) {
        let Some(sessions) = self.sessions() else {
            return; // executor not wired yet (or retired); the set_sessions kick re-drains
        };
        // Temporary Side Chat docs carry no durable command ledger — the
        // side-chat manager dispatches sends directly (no SQLite processed-
        // ledger writes, no claim-on-first-command workspace row).
        if handle.is_ephemeral() {
            return;
        }
        if !self.is_host(&handle.chat_id) {
            return;
        }
        // Entries this pass decided to leave alone (processed dedupe hits).
        let mut skipped: HashSet<String> = HashSet::new();
        loop {
            let commands = match handle.doc.read_commands() {
                Ok(commands) => commands,
                Err(err) => {
                    tracing::warn!(chat = %handle.chat_id, error = %err, "command read failed");
                    return;
                }
            };
            let is_processed = |id: &str| self.inner.store.is_processed(id).unwrap_or(false);

            // Dead-command recovery: a previous process may have committed
            // the processed-ledger claim and died before writing the outcome.
            // Without this sweep every future drain sees the entry as already
            // processed and leaves it Pending forever. The in-memory
            // `executing` set excludes commands currently running in this
            // process.
            //
            // `commands` is a snapshot, and a concurrent drain may resolve a
            // command and leave `executing` after it was taken. So once a
            // candidate is out of `executing`, re-read its status: the
            // executor writes the outcome before it leaves the set, so a
            // command it finished reads resolved here, and a stale Pending
            // never overwrites Applied.
            let dead: Vec<String> = commands
                .iter()
                .filter(|command| {
                    command.status == SessionCommandStatus::Pending
                        && !skipped.contains(&command.id)
                        && is_processed(&command.id)
                        && !lock(&self.inner.executing).contains(&command.id)
                })
                .map(|command| command.id.clone())
                .collect();
            let still_pending: HashSet<String> = if dead.is_empty() {
                HashSet::new()
            } else {
                handle
                    .doc
                    .read_commands()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|command| command.status == SessionCommandStatus::Pending)
                    .map(|command| command.id)
                    .collect()
            };
            for command_id in dead {
                if !still_pending.contains(&command_id) {
                    skipped.insert(command_id);
                    continue;
                }
                tracing::warn!(
                    chat = %handle.chat_id,
                    command = %command_id,
                    "command consumed but never resolved; marking interrupted"
                );
                self.resolve_command(
                    handle,
                    &command_id,
                    SessionCommandStatus::Rejected,
                    Some("interrupted before completion — retry to send again"),
                );
                skipped.insert(command_id);
            }

            let Some(entry) = commands
                .iter()
                .find(|c| {
                    c.status == SessionCommandStatus::Pending
                        && !skipped.contains(&c.id)
                        && !is_processed(&c.id)
                })
                .cloned()
            else {
                return;
            };
            let messages = handle.doc.read_entries().unwrap_or_default();
            let current_turn_id = messages.last().map(|m| m.id.clone());
            let turn_is_past = |turn_id: &str| messages.iter().any(|m| m.id == turn_id);
            let sealed_path = |upload_id: &str| {
                handle
                    .doc
                    .sealed_attachment(upload_id)
                    .ok()
                    .flatten()
                    .map(|(path, _)| path)
            };
            let disposition = evaluate_command(
                &entry,
                &EvaluationContext {
                    is_processed: &is_processed,
                    now_ms: now_ms(),
                    entries: &commands,
                    current_turn_id: current_turn_id.as_deref(),
                    turn_is_past: &turn_is_past,
                    sealed_attachment_path: &sealed_path,
                },
            );
            // Attachments still uploading: hold WITHOUT marking processed so
            // the seal commit (which re-triggers this drain) releases the
            // Run; an expired grace window instead resolves Expired. The
            // `return` (not `continue`) also keeps later commands behind this
            // one — a newer Run must not jump a Run waiting on its uploads.
            if matches!(disposition, CommandDisposition::WaitForAttachments) {
                tracing::debug!(
                    chat = %handle.chat_id,
                    command = %entry.id,
                    "run waiting for attachment seal"
                );
                return;
            }
            // In-flight claim: a concurrent drain must not classify this
            // command as crashed while this task is between mark and resolve.
            if !lock(&self.inner.executing).insert(entry.id.clone()) {
                skipped.insert(entry.id.clone());
                continue;
            }
            // Mark BEFORE executing: a crash mid-execution must never double-run a
            // command whose side effect may already have happened.
            match self.inner.store.mark_processed(&entry.id) {
                Ok(true) => {}
                Ok(false) => {
                    lock(&self.inner.executing).remove(&entry.id);
                    skipped.insert(entry.id.clone());
                    continue;
                }
                Err(err) => {
                    lock(&self.inner.executing).remove(&entry.id);
                    tracing::error!(chat = %handle.chat_id, error = %err, "processed-ledger write failed; halting drain");
                    return;
                }
            }
            match disposition {
                CommandDisposition::Skip => {
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Expired => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Expired, None);
                }
                CommandDisposition::Superseded => {
                    self.resolve_command(handle, &entry.id, SessionCommandStatus::Superseded, None);
                }
                // Returned above (before the processed-ledger mark) — the
                // seal commit re-triggers this drain.
                CommandDisposition::WaitForAttachments => {
                    lock(&self.inner.executing).remove(&entry.id);
                    skipped.insert(entry.id.clone());
                }
                CommandDisposition::Execute => {
                    let (status, resolution) = match self.execute(&sessions, handle, &entry).await {
                        Ok(outcome) => outcome,
                        Err(err) => (SessionCommandStatus::Rejected, Some(err.to_string())),
                    };
                    self.resolve_command(handle, &entry.id, status, resolution.as_deref());
                }
            }
            lock(&self.inner.executing).remove(&entry.id);
        }
    }

    fn save_snapshot(&self, handle: &ChatDocHandle) {
        // Temporary Side Chats never persist until promotion (host-memory
        // only by contract — dispose leaves no durable remnants).
        if handle.ephemeral.load(Ordering::Acquire) {
            return;
        }
        match handle.doc.export_snapshot() {
            Ok(bytes) => {
                handle.snapshot_bytes.store(bytes.len(), Ordering::Relaxed);
                if let Err(err) = self.inner.store.save_snapshot(&handle.chat_id, &bytes) {
                    tracing::warn!(chat = %handle.chat_id, error = %err, "snapshot save failed");
                }
            }
            Err(err) => {
                tracing::warn!(chat = %handle.chat_id, error = %err, "snapshot export failed");
            }
        }
    }

    /// Persist every open doc now (shutdown path; bypasses the debounce).
    /// Temporary Side Chat docs are skipped — the side-chat manager disposes
    /// them at shutdown, and `save_snapshot` guards ephemeral handles anyway.
    pub fn flush_all(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            self.save_snapshot(&handle);
        }
    }

    /// Close all account-scoped room memberships before graceful engine
    /// draining. Auth-aware join supervisors will not install a late client.
    pub fn disconnect_edge(&self) {
        let handles: Vec<_> = lock(&self.inner.handles).values().cloned().collect();
        for handle in handles {
            lock(&handle.chat2).take();
            lock(&handle.chat2_local_sub).take();
        }
    }
}

/// The attachment-refs trailer the host appends to a Run's visible prompt
/// (and its effective annotated prompt) from SEALED final paths at execute
/// time — the shared [`cypher_proto::attachment_refs`] transport, so it is
/// byte-identical to the composer's `with_attachments`. Pending refs never
/// reach this point: the composer queues the Run without a trailer, and only
/// sealed paths enter the prompt here. Pure.
pub fn attachment_refs_trailer(text: &str, paths: &[String]) -> String {
    cypher_proto::attachment_refs::with_refs(text, paths)
}

/// The resumed-turn prompt for answers to a question whose run died: each
/// answer paired with its question text so the reattached conversation reads
/// naturally. Pure.
pub fn respond_input_prompt(
    questions: &[UserInputQuestion],
    answers: &[UserInputAnswer],
) -> String {
    let mut lines = vec!["Answering your earlier question:".to_string()];
    for answer in answers {
        let picked = answer.labels.join(", ");
        let question = questions
            .iter()
            .find(|q| q.id == answer.question_id)
            .map(|q| q.question.trim())
            .filter(|q| !q.is_empty());
        match question {
            Some(question) => lines.push(format!("{question} — {picked}")),
            None => lines.push(picked),
        }
    }
    lines.join("\n")
}

/// Percent-encode one URL path segment of a sidecar part id. PART_RE's
/// alphabet includes `#` and `:` — legal in R2 keys and doc refs, but a raw
/// `#` in a URL is a fragment delimiter (the request would silently hit the
/// truncated key, colliding parts). The Worker decodes before validating.
fn encode_part_segment(part_id: &str) -> String {
    let mut out = String::with_capacity(part_id.len());
    for byte in part_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod part_segment_tests {
    use super::encode_part_segment;

    #[test]
    fn hash_and_colon_are_escaped_unreserved_pass_through() {
        assert_eq!(encode_part_segment("m1#c1"), "m1%23c1");
        assert_eq!(encode_part_segment("tool:call_9"), "tool%3Acall_9");
        assert_eq!(encode_part_segment("plain-id_0.diff~"), "plain-id_0.diff~");
    }
}

/// Per-chat background task: reacts to doc changes (local commits and remote imports)
/// by re-publishing the transcript watch, draining commands, and debouncing snapshots.
/// Holds only a weak handle so a dropped host tears the task down.
async fn chat_task(host: DocHost, weak: Weak<ChatDocHandle>, mut changed_rx: watch::Receiver<u64>) {
    // Initial pass: the snapshot may already carry pending commands. The
    // mirror stays lazy — it materializes on the first watch attach.
    {
        let Some(handle) = weak.upgrade() else { return };
        host.drain_commands(&handle).await;
    }
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // doc handle (and its change sender) is gone
                }
                let Some(handle) = weak.upgrade() else { break };
                handle.publish_messages_if_watched();
                handle.publish_commands_if_watched();
                host.drain_commands(&handle).await;
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(handle) = weak.upgrade() else { break };
                host.save_snapshot(&handle);
                // chat2 host duties ride the same quiesce tick (C3).
                host.chat2_maintenance(&handle).await;
                // Post-quiesce eviction pass: sizes just refreshed.
                host.evict_over_budget();
            }
        }
    }
}

#[cfg(test)]
mod loro_hook_tests {
    use super::*;

    struct SignedOut;

    #[async_trait::async_trait]
    impl cypher_rpc::TokenSource for SignedOut {
        async fn token(&self) -> Option<String> {
            None
        }
    }

    fn edge_host(dir: &std::path::Path) -> DocHost {
        let store = Arc::new(DocsStore::open(dir).unwrap());
        DocHost::new(
            store,
            DocHostConfig {
                device_id: "dev".into(),
                default_harness: HarnessId::Pi,
                edge: Some(EdgeConfig {
                    preview: None,
                    url: "http://127.0.0.1:9".into(),
                    token: Arc::new(SignedOut),
                    device_id: "dev".into(),
                    viewport_activity: Default::default(),
                }),
            },
        )
    }

    /// The headless hang, reduced: a thread that holds the chat2 client
    /// lock must never stall a commit on the same doc. The Loro local-update
    /// hook only forwards to the feed channel; the client/pending routing
    /// happens on the pump task afterwards, under the lock but outside Loro.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_commit_never_waits_on_the_chat2_client_lock() {
        let dir = tempfile::tempdir().unwrap();
        let host = edge_host(dir.path());
        let handle = host.open("chat-a").unwrap();
        let parked_before = lock(&handle.chat2_pending_local).len();

        // Park the client lock on a plain thread for the whole commit.
        let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = handle.clone();
        let parker = std::thread::spawn(move || {
            let _guard = lock(&holder.chat2);
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        locked_rx.recv().unwrap();

        let doc = handle.doc_arc();
        let commit = tokio::task::spawn_blocking(move || {
            doc.doc().get_map("meta").insert("probe", "v").unwrap();
            doc.doc().commit();
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), commit)
            .await
            .expect("a local commit must not wait on the chat2 client lock")
            .unwrap();

        release_tx.send(()).unwrap();
        parker.join().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while lock(&handle.chat2_pending_local).len() <= parked_before {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the feed pump never routed the commit into the pending buffer"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        drop(handle);
        host.shutdown_workers().await;
    }
}

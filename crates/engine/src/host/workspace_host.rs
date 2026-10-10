//! WorkspaceHost — owns the per-user workspace **registry** (docs/
//! registry-sync.md): local snapshot persistence, edge room sync
//! (`/registry/{orgId}/ws` → room `reg1/{orgId}/{userId}`, offline-tolerant —
//! spaces/sessions are private to their owner, never org-visible), the device
//! registry row for THIS device, and the typed watch channels the
//! WatchChats/WatchDevices/WatchSessions RPC streams are fed from.
//!
//! Writer discipline (kept from the doc schema): this host writes its own device row,
//! its own session-status rows, and rows for chats it hosts; renames/archives and
//! device/space deletes are LWW sets accepted from any device (the Mutate surface).
//! Unpairing another device tombstones its registry row; that machine observes the
//! tombstone, signs out, and continues in local-only mode. Deleting THIS device is
//! refused — sign out is the way to leave.
//!
//! Liveness: `lastSeenAt` is a row write on boot/shutdown ONLY — the periodic 15s
//! heartbeat rides the room's presence frames (memory-only on the DO), so staying
//! online never grows server state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use cypher_doc::{DeletedDevice, DeletedSpace, REGISTRY_DOC_ID, RegistryDoc};
use cypher_proto::{
    Chat, ChatConfig, ChildAgentProfile, ChildChat, Device, HarnessId, SandboxLevel, Session,
    Space, SubagentRunMode,
};
use cypher_sync::{DocsStore, RegistryClient, RegistryTransport, RegistryTuning};

use crate::EngineError;
use crate::host::doc_host::EdgeConfig;
use crate::util::lock;
use crate::util::now_ms;

mod entities;
mod presence;
mod sync;
mod transport;

use presence::{PresenceWatch, RelayProbeRetry, relay_probe_task};
use sync::workspace_task;
use transport::{EdgeRegistryTransport, token_revoked};

/// Outcome of the idempotent [`WorkspaceHost::create_child_chat`] — lets the
/// `StartSubagent` handler distinguish a NEW child (whose initial durable Run
/// it must queue) from an idempotent retry of `(parent_chat_id, parent_run_id)`
/// (whose run was already queued once — the caller must NOT queue a second).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildChatOutcome {
    /// A fresh child row was created (its initial run still needs queuing).
    Created(String),
    /// The `(parent, run)` pair already had a child row; nothing was written.
    Existing(String),
}

impl ChildChatOutcome {
    pub fn id(&self) -> &str {
        match self {
            ChildChatOutcome::Created(id) | ChildChatOutcome::Existing(id) => id,
        }
    }

    pub fn created(&self) -> bool {
        matches!(self, ChildChatOutcome::Created(_))
    }
}

/// Org used when none is configured (matches the edge's dev-mode `user@org` bearers).
pub const DEFAULT_ORG_ID: &str = "dev-org";
/// User used when none is configured (dev mode without a bearer).
pub const DEFAULT_USER_ID: &str = "dev-user";
/// Presence beat cadence.
const PRESENCE_INTERVAL_MS: u64 = 15_000;
/// Initial-join retry backoff (base, cap). A first registry-room join that
/// fails must not strand the device offline until an app restart — retry until
/// it lands. Jittered so N devices restarting together don't resynchronize
/// their retries into a thundering herd on the cold DO.
pub(crate) const JOIN_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(500);
pub(crate) const JOIN_RETRY_CAP: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) async fn token_changed(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    match changes {
        Some(changes) => {
            let _ = changes.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Cheap decorrelation jitter (0–500ms) without pulling in a rng — derived from
/// the sub-nanosecond wall clock. Mirrors the device relay's `jitter()`.
pub(crate) fn join_retry_jitter() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::time::Duration::from_millis(u64::from(nanos) % 500)
}

#[derive(Debug, Clone)]
pub struct WorkspaceHostConfig {
    pub device_id: String,
    /// Human name for this device's registry row (hostname by default).
    pub device_name: String,
    /// `std::env::consts::OS`-style platform string.
    pub platform: String,
    pub org_id: String,
    /// The signed-in user — registries are per-user (`reg1/{orgId}/{userId}`):
    /// spaces/sessions are private to their owner, never org-visible.
    pub user_id: String,
    /// When present, the host joins `/registry/{orgId}/ws`. `None` = fully offline
    /// (local snapshots only; the registry still drives everything device-side).
    pub edge: Option<EdgeConfig>,
    /// Fresh sign-in this process: revive a tombstoned device row instead of
    /// treating the tombstone as an eviction. Consumed once at first reconcile.
    pub allow_device_rejoin: bool,
}

struct WorkspaceHostInner {
    store: Arc<DocsStore>,
    config: WorkspaceHostConfig,
    reg: Arc<Mutex<RegistryDoc>>,
    chats_tx: watch::Sender<Vec<Chat>>,
    devices_tx: watch::Sender<Vec<Device>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    spaces_tx: watch::Sender<Vec<Space>>,
    room: Mutex<Option<Arc<RegistryClient>>>,
    /// Bumped on every registry change (local mutation or applied server
    /// frame) — drives republish + the snapshot debounce in `workspace_task`.
    changed_tx: watch::Sender<u64>,
    /// Latched after the first authoritative server state applies this boot.
    /// An offline/local registry is authoritative from its first snapshot;
    /// an online replica must not sweep apparently missing rows before this
    /// latch is set.
    registry_synced: AtomicBool,
    /// This device was unpaired (our registry row is tombstoned). The engine
    /// signs out so the machine continues in local-only mode.
    evicted: AtomicBool,
    /// Boot announce already ran (or was skipped because we were evicted).
    announced: AtomicBool,
    evicted_tx: watch::Sender<bool>,
    /// Freshest presence heartbeat (ms) we have EVER observed per device. The
    /// room's presence map forgets entries after its 30s TTL and starts empty
    /// on a (re)join, so without this cache a receive-side hiccup snaps a
    /// device's overlay back to its boot-time row `lastSeenAt` — an instant
    /// (and false) "offline" badge for a host that beat 20s ago.
    presence_seen: Mutex<std::collections::HashMap<String, i64>>,
    relay_probe_backoff: Mutex<std::collections::HashMap<String, RelayProbeRetry>>,
    relay_probe_wake: Arc<tokio::sync::Notify>,
    /// Called with a device id whenever its presence heartbeat proves it alive —
    /// wired to `LinkCache::reset_cooldown` so a peer that comes back is dialed
    /// immediately instead of waiting out the failure backoff.
    peer_alive: Mutex<Option<PeerAliveHook>>,
    notification_event: Mutex<Option<NotificationEventHook>>,
    /// Deaf-socket tripwire state — see `check_presence_deafness`.
    presence_watch: Mutex<PresenceWatch>,
}

/// "This peer is alive" callback (device id) — see `WorkspaceHost::set_peer_alive_hook`.
pub type PeerAliveHook = Arc<dyn Fn(&str) + Send + Sync>;
pub type NotificationEventHook = Arc<dyn Fn(&Session) + Send + Sync>;

#[derive(Clone)]
pub struct WorkspaceHost {
    inner: Arc<WorkspaceHostInner>,
}

impl WorkspaceHost {
    /// Load (or init) the registry, upsert this device's row, start
    /// the change-driven task, and join the edge registry room when configured.
    pub fn open(store: Arc<DocsStore>, config: WorkspaceHostConfig) -> Result<Self, EngineError> {
        let mut doc = match store.load_snapshot(REGISTRY_DOC_ID)? {
            Some(bytes) => RegistryDoc::from_bytes(&bytes, &config.device_id)
                .map_err(|e| EngineError::Other(format!("registry snapshot load failed: {e}")))?,
            None => RegistryDoc::new(&config.device_id),
        };

        // Boot: announce our device row immediately when there's no edge (local
        // / tests). Synced runtimes wait for the first authoritative registry
        // state: a tombstone means we were unpaired and must NOT revive the row.
        if config.edge.is_none() {
            announce_device(&mut doc, &config)?;
        }

        let state = doc.read_all()?;
        let (chats_tx, _) = watch::channel(state.chats);
        let (devices_tx, _) = watch::channel(state.devices);
        let (sessions_tx, _) = watch::channel(state.sessions);
        let (spaces_tx, _) = watch::channel(state.spaces);
        let (changed_tx, changed_rx) = watch::channel(0u64);
        let (evicted_tx, _) = watch::channel(false);
        let registry_synced = config.edge.is_none();
        let announced = config.edge.is_none();

        let host = Self {
            inner: Arc::new(WorkspaceHostInner {
                store,
                config,
                reg: Arc::new(Mutex::new(doc)),
                chats_tx,
                devices_tx,
                sessions_tx,
                spaces_tx,
                room: Mutex::new(None),
                changed_tx,
                registry_synced: AtomicBool::new(registry_synced),
                evicted: AtomicBool::new(false),
                announced: AtomicBool::new(announced),
                evicted_tx,
                presence_seen: Mutex::new(std::collections::HashMap::new()),
                relay_probe_backoff: Mutex::new(std::collections::HashMap::new()),
                relay_probe_wake: Arc::new(tokio::sync::Notify::new()),
                peer_alive: Mutex::new(None),
                notification_event: Mutex::new(None),
                presence_watch: Mutex::new(PresenceWatch::default()),
            }),
        };
        // Persist immediately: after this boot the migration source is never
        // read again, so the registry snapshot must exist even if the process
        // dies before the first debounced save.
        host.inner.save_snapshot();
        host.join_room();
        if let Some(edge) = &host.inner.config.edge {
            // The activity RPC lives on the auth service, which has no route
            // to this host; the slot both already share is the meeting point.
            // Weak, so a torn-down host is never kept alive by the slot.
            let weak = Arc::downgrade(&host.inner);
            edge.viewport_activity.set_immediate_beat(move || {
                weak.upgrade()
                    .is_some_and(|inner| inner.beat_activity_now())
            });
        }
        tokio::spawn(workspace_task(Arc::downgrade(&host.inner), changed_rx));
        if host.inner.config.edge.is_some() {
            tokio::spawn(relay_probe_task(Arc::downgrade(&host.inner)));
        }
        Ok(host)
    }

    /// Edge room join — offline-tolerant: a failed join logs and stays local-first.
    fn join_room(&self) {
        let Some(edge) = &self.inner.config.edge else {
            return;
        };
        let org_id = self.inner.config.org_id.clone();
        // Per-dial URL provider: the bearer is re-read on every (re)connect.
        let url = edge.room_url(format!("/registry/{org_id}/ws"));
        self.spawn_join(url, edge.token_changes(), Some(edge.token.clone()));
    }

    /// Test seam: join a registry room at a fixed WebSocket URL without an
    /// `EdgeConfig` — integration tests wire hosts to an in-process mock
    /// server through this. Production always goes through [`Self::join_room`].
    #[doc(hidden)]
    pub fn connect_registry_url(&self, url: &str) {
        self.spawn_join(
            Arc::new(cypher_sync::StaticUrl(url.to_string())),
            None,
            None,
        );
    }

    /// Close the current registry membership before account-scoped state is
    /// drained. The auth signal prevents an in-flight join from replacing it.
    pub fn disconnect_edge(&self) {
        lock(&self.inner.room).take();
    }

    /// Wire the "peer is alive" signal (fresh presence heartbeat) to a callback —
    /// the engine points this at `LinkCache::reset_cooldown`.
    pub fn set_peer_alive_hook(&self, hook: PeerAliveHook) {
        *lock(&self.inner.peer_alive) = Some(hook);
    }

    pub fn set_notification_event_hook(&self, hook: NotificationEventHook) {
        *lock(&self.inner.notification_event) = Some(hook);
    }

    pub fn notify_session_event(&self, session: &Session) {
        if let Some(hook) = lock(&self.inner.notification_event).clone() {
            hook(session);
        }
    }

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
    }

    pub fn org_id(&self) -> &str {
        &self.inner.config.org_id
    }

    pub fn user_id(&self) -> &str {
        &self.inner.config.user_id
    }

    pub fn connected(&self) -> bool {
        lock(&self.inner.room)
            .as_ref()
            .is_some_and(|room| room.stats().connected)
    }

    /// Whether this boot has received an authoritative registry state. Local
    /// profile snapshots are considered synchronized immediately; online
    /// profiles latch this only after the first successful room state.
    pub fn registry_synced(&self) -> bool {
        self.inner.registry_synced.load(Ordering::Relaxed)
    }

    /// Probe the registry room's liveness NOW (window-focus sweep). Probes are
    /// deadline-checked in the client: an unanswered probe tears the session
    /// down for a fresh socket, so a deaf-receiving room heals within seconds of the user looking at the app.
    pub fn probe(&self) {
        // Foreground/manual retry bypasses negative-cache delays. The task
        // consumes this after any in-flight request, so a late false reply
        // cannot swallow the user's reset.
        if !lock(&self.inner.relay_probe_backoff).is_empty() {
            self.inner.relay_probe_wake.notify_one();
        }
        if let Some(room) = lock(&self.inner.room).as_ref() {
            room.probe();
        }
    }

    /// Registry room introspection for SyncStatus / `cypher sync`.
    /// `None` = no room yet (edge-less, or the initial join is still retrying).
    pub fn sync_status(&self) -> Option<cypher_sync::RoomStatsSnapshot> {
        lock(&self.inner.room).as_ref().map(|room| room.stats())
    }

    // ── registry access helpers ─────────────────────────────────────────────

    /// Run a mutation under the registry lock, then wake the publish/persist
    /// task and push the write to the room.
    fn mutate<R>(&self, f: impl FnOnce(&mut RegistryDoc) -> R) -> R {
        let result = f(&mut lock(&self.inner.reg));
        self.inner.bump_changed();
        if let Some(room) = lock(&self.inner.room).as_ref() {
            room.nudge();
        }
        result
    }

    fn read<R>(&self, f: impl FnOnce(&RegistryDoc) -> R) -> R {
        f(&lock(&self.inner.reg))
    }

    /// The chat row as currently known (overlay view).
    pub fn chat(&self, chat_id: &str) -> Result<Option<Chat>, EngineError> {
        Ok(self.read(|doc| doc.chat(chat_id))?)
    }

    /// The space row as currently known (overlay view).
    pub fn space(&self, space_id: &str) -> Result<Option<Space>, EngineError> {
        Ok(self.read(|doc| doc.space(space_id))?)
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, EngineError> {
        Ok(self.read(|doc| doc.read_chats())?)
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, EngineError> {
        Ok(self.read(|doc| doc.read_devices())?)
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, EngineError> {
        Ok(self.read(|doc| doc.read_sessions())?)
    }

    // ── watches (WatchChats / WatchDevices / merged WatchSessions) ──────────

    pub fn watch_chats(&self) -> watch::Receiver<Vec<Chat>> {
        self.inner.chats_tx.subscribe()
    }

    pub fn watch_devices(&self) -> watch::Receiver<Vec<Device>> {
        self.inner.devices_tx.subscribe()
    }

    /// Raw workspace session-status rows (all devices').
    pub fn watch_session_rows(&self) -> watch::Receiver<Vec<Session>> {
        self.inner.sessions_tx.subscribe()
    }

    pub fn watch_spaces(&self) -> watch::Receiver<Vec<Space>> {
        self.inner.spaces_tx.subscribe()
    }

    /// WatchSessions source: durable registry rows merged with this engine's
    /// live status watch (the local view is fresher for runs touched since boot).
    ///
    /// Local durable rows must remain in the stream when the live map has no
    /// entry yet. Otherwise every completed session — including child chats —
    /// disappears after an engine restart and the UI can only guess that the
    /// durable child relation is still "Starting".
    pub fn merged_sessions_watch(
        &self,
        local: watch::Receiver<Vec<Session>>,
    ) -> watch::Receiver<Vec<Session>> {
        let mut rows = self.watch_session_rows();
        let mut local = local;
        let device_id = self.inner.config.device_id.clone();
        let (tx, rx) = watch::channel(merge_sessions(&device_id, &rows.borrow(), &local.borrow()));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = rows.changed() => if changed.is_err() { break },
                    changed = local.changed() => if changed.is_err() { break },
                }
                let merged = merge_sessions(
                    &device_id,
                    &rows.borrow_and_update(),
                    &local.borrow_and_update(),
                );
                if tx.send(merged).is_err() {
                    break; // no receivers left
                }
            }
        });
        rx
    }

    // ── chat ownership ──────────────────────────────────────────────────────

    /// Writer discipline: the chat's host is its row's `deviceId`. Unknown chats
    /// are claimable — the first run command claims them via [`Self::claim_chat`].
    pub fn is_host(&self, chat_id: &str) -> bool {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(Some(chat)) => chat.device_id == self.inner.config.device_id,
            Ok(None) => true,
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                true
            }
        }
    }

    /// Claim-on-first-command: create the chat row under OUR device id when a run
    /// command arrives for a chat with no row yet. No-op when the row exists.
    ///
    /// The claim is a PARTIAL row write (identity/cwd/space only): the command
    /// plane is nudged and outruns the registry channel, so the client's
    /// `createChat` for the same chat routinely arrives AFTER the claim with
    /// older clocks — fields the claim never wrote (`config`, `title`) must
    /// still land then.
    ///
    /// Spaces invariant: every chat belongs to a space, so the claim resolves an
    /// own-device space matching `cwd` — or auto-creates one (gitDetected false;
    /// SpacesSync corrects on its next pass). A cwd-less claim (e.g. note_message
    /// racing ahead of the run command) leaves `spaceId` unset; the row is
    /// invisible to the UI until a spaced claim/create lands.
    pub fn claim_chat(&self, chat_id: &str, cwd: Option<&str>) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(());
        }
        let space_id = match cwd {
            Some(cwd) => Some(self.space_for_path(cwd)?),
            None => None,
        };
        self.mutate(|doc| doc.claim_chat(chat_id, cwd, space_id.as_deref(), Utc::now()));
        Ok(())
    }

    /// An own-device space whose path matches, else one at the path's parent
    /// checkout root, else a freshly created one at that root.
    ///
    /// A linked-worktree cwd resolves to the checkout root FIRST: claiming at
    /// the worktree path itself minted a phantom sidebar space named after the
    /// worktree folder ("clever-ember") next to the project's real space.
    fn space_for_path(&self, path: &str) -> Result<String, EngineError> {
        let device_id = &self.inner.config.device_id;
        let spaces = self.read(|doc| doc.read_spaces())?;
        if let Some(space) = spaces
            .iter()
            .find(|s| s.device_id == *device_id && s.path == path)
        {
            return Ok(space.id.clone());
        }
        let root = linked_worktree_root(std::path::Path::new(path));
        if let Some(root) = root.as_deref()
            && let Some(space) = spaces
                .iter()
                .find(|s| s.device_id == *device_id && s.path == root)
        {
            return Ok(space.id.clone());
        }
        let space = Space {
            icon: None,
            color: None,
            pinned: false,
            id: crate::util::new_id(),
            device_id: device_id.clone(),
            path: root.unwrap_or_else(|| path.to_string()),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        };
        self.mutate(|doc| doc.upsert_space(&space))?;
        Ok(space.id)
    }

    /// The chat's configured harness/model row, when present (RunRequest harness
    /// selection; callers fall back to the engine default).
    pub fn chat_config(&self, chat_id: &str) -> Option<ChatConfig> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => chat.and_then(|c| c.config),
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    // ── host-side row writes ────────────────────────────────────────────────

    /// Sidebar freshness on message persist: preview = first 120 chars of the last
    /// message's text. Claims the row first so a pre-workspace chat gains one.
    pub fn note_message(&self, chat_id: &str, text: &str) {
        let preview: String = text.chars().take(120).collect();
        let result = self.claim_chat(chat_id, None).and_then(|_| {
            self.mutate(|doc| doc.set_chat_last_message(chat_id, &preview, Utc::now()))
                .map_err(EngineError::from)
        });
        if let Err(err) = result {
            tracing::warn!(chat = %chat_id, error = %err, "registry last-message write failed");
        }
    }

    /// Activity bump without a preview (`RegistryDoc::touch_chat_activity`).
    pub fn touch_chat_activity(&self, chat_id: &str) {
        let result = self.claim_chat(chat_id, None).and_then(|_| {
            self.mutate(|doc| doc.touch_chat_activity(chat_id, Utc::now()))
                .map_err(EngineError::from)
        });
        if let Err(err) = result {
            tracing::warn!(chat = %chat_id, error = %err, "registry activity touch failed");
        }
    }

    /// Resume continuity: stamp the chat row with the harness-native session id
    /// of its latest run and the cwd it was created under. An empty `session_id`
    /// tombstones the row ("do not resume" after a rejected resume). Best-effort:
    /// a missing chat row (claim happens on first command) just returns.
    pub fn set_chat_harness_session(&self, chat_id: &str, session_id: &str, cwd: &str) {
        match self.mutate(|doc| doc.set_chat_harness_session(chat_id, session_id, cwd)) {
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry harness-session write failed");
            }
        }
    }

    /// The chat row's stored harness session `(session_id, cwd)`, if stamped.
    /// The empty-string tombstone passes through — callers must treat it as
    /// "explicitly no resume" (and must NOT fall back to older sources).
    pub fn chat_harness_session(&self, chat_id: &str) -> Option<(String, Option<String>)> {
        match self.read(|doc| doc.chat(chat_id)) {
            Ok(chat) => {
                let chat = chat?;
                let id = chat.harness_session_id?;
                Some((id, chat.harness_session_cwd))
            }
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "registry chat read failed");
                None
            }
        }
    }

    /// Session-status row upsert (sessions engine transitions land here too, in
    /// addition to the local watch channel).
    pub fn record_session(&self, session: &Session) {
        if let Err(err) = self.mutate(|doc| doc.upsert_session(session)) {
            tracing::warn!(chat = %session.chat_id, error = %err, "registry session write failed");
        }
    }

    /// True once this device's registry row was tombstoned and we accepted eviction.
    pub fn watch_evicted(&self) -> watch::Receiver<bool> {
        self.inner.evicted_tx.subscribe()
    }

    /// Re-check the server tombstone for THIS device. Synced runtimes call this
    /// after each authoritative apply; tests call it to simulate that.
    pub fn reconcile_own_device(&self) {
        self.inner.reconcile_own_device();
    }

    // ── persistence / teardown ──────────────────────────────────────────────

    /// Persist the snapshot now (shutdown path; bypasses the debounce).
    pub fn flush(&self) {
        self.inner.save_snapshot();
    }

    /// Shutdown: stamp our `lastSeenAt` (the only periodic-ish row write besides
    /// boot) and flush the snapshot.
    pub fn shutdown(&self) {
        let now = Utc::now();
        let device_id = self.inner.config.device_id.clone();
        if let Err(err) = self.mutate(|doc| doc.set_device_last_seen(&device_id, now)) {
            tracing::warn!(error = %err, "device lastSeenAt stamp failed");
        }
        self.inner.save_snapshot();
    }
}

/// Store a snapshot, waking watchers only when it actually differs.
///
/// Every inbound presence beat republishes everything, and with a beat per
/// device every 15s that was thousands of *identical* snapshots an hour. Each
/// one woke every watch stream, and a stream subscribed from another device
/// turns into a relay frame on that device's room — measured in production as
/// 1,050 inbound websocket messages/hour on a single DeviceRoom, all of them
/// re-sending data the peer already had.
///
/// This keeps the stored value current, which is why it is not a plain `send`:
/// `watch::Sender::send` drops the value when no receiver exists yet, so a
/// stream subscribed later would start from a stale snapshot (found the hard
/// way by the e2e smoke). `send_if_modified` still writes the value through —
/// it just wakes nobody when nothing changed.
fn publish_if_changed<T: PartialEq>(tx: &watch::Sender<Vec<T>>, next: Vec<T>) {
    tx.send_if_modified(|current| {
        if *current == next {
            return false;
        }
        *current = next;
        true
    });
}

impl WorkspaceHostInner {
    fn bump_changed(&self) {
        self.changed_tx.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn mark_evicted(&self) {
        if self.evicted.swap(true, Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            device = %self.config.device_id,
            "this device was unpaired from the account; dropping out of sync"
        );
        self.evicted_tx.send_replace(true);
    }

    /// After authoritative registry state: if we were unpaired, drop any
    /// pending revival and evict; otherwise announce this device once.
    fn reconcile_own_device(&self) {
        if self.evicted.load(Ordering::Relaxed) {
            return;
        }
        let device_id = self.config.device_id.clone();
        let (tombstoned, live) = {
            let doc = lock(&self.reg);
            (
                doc.device_is_tombstoned(&device_id),
                doc.read_devices()
                    .ok()
                    .is_some_and(|devices| devices.iter().any(|d| d.id == device_id)),
            )
        };
        let unpaired = tombstoned || (self.announced.load(Ordering::Relaxed) && !live);
        if unpaired && !self.config.allow_device_rejoin {
            lock(&self.reg).drop_pending_device_writes(&device_id);
            self.mark_evicted();
            self.bump_changed();
            return;
        }
        if self.announced.swap(true, Ordering::Relaxed) && !unpaired {
            return;
        }
        if let Err(err) = announce_device(&mut lock(&self.reg), &self.config) {
            tracing::warn!(error = %err, "device announce failed");
            self.announced.store(false, Ordering::Relaxed);
            return;
        }
        self.bump_changed();
    }

    fn publish(&self) {
        match lock(&self.reg).read_all() {
            Ok(mut state) => {
                self.overlay_presence(&mut state.devices);
                publish_if_changed(&self.chats_tx, state.chats);
                publish_if_changed(&self.devices_tx, state.devices);
                publish_if_changed(&self.sessions_tx, state.sessions);
                publish_if_changed(&self.spaces_tx, state.spaces);
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry read failed");
            }
        }
    }
}

/// The parent checkout root of a linked git worktree: `<path>/.git` is a FILE
/// containing `gitdir: <root>/.git/worktrees/<name>`. `None` for a primary
/// checkout (`.git` is a directory), a non-repo folder, or any other layout
/// (bare-repo worktrees have no `<root>` working copy to attribute to). Pure
/// fs reads — no git subprocess; this runs on the synchronous claim path.
fn linked_worktree_root(path: &std::path::Path) -> Option<String> {
    let gitfile = path.join(".git");
    if !std::fs::metadata(&gitfile).ok()?.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&gitfile).ok()?;
    let target = content
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let mut target = std::path::PathBuf::from(target);
    if target.is_relative() {
        // Rare (`worktree.useRelativePaths`); canonicalize resolves the
        // `../..` hops against the real filesystem.
        target = std::fs::canonicalize(path.join(target)).ok()?;
    }
    let worktrees = target.parent()?;
    let dot_git = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || dot_git.file_name()? != ".git" {
        return None;
    }
    Some(dot_git.parent()?.to_string_lossy().into_owned())
}

/// Durable registry rows survive engine restarts; local live statuses override
/// matching rows for this device once a run is touched. Sorted by chat id
/// (stable stream output).
fn merge_sessions(device_id: &str, rows: &[Session], local: &[Session]) -> Vec<Session> {
    let mut merged: std::collections::HashMap<String, Session> = rows
        .iter()
        .map(|s| (s.chat_id.clone(), s.clone()))
        .collect();
    for session in local {
        // Only this engine's own live projection may supersede its durable
        // row. A malformed/foreign live row must not overwrite another
        // device's registry truth.
        if session.device_id == device_id {
            merged.insert(session.chat_id.clone(), session.clone());
        }
    }
    let mut list: Vec<Session> = merged.into_values().collect();
    list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    list
}

fn announce_device(doc: &mut RegistryDoc, config: &WorkspaceHostConfig) -> Result<(), EngineError> {
    let now = Utc::now();
    let existing = doc
        .read_devices()?
        .into_iter()
        .find(|d| d.id == config.device_id);
    doc.upsert_device(&Device {
        id: config.device_id.clone(),
        name: device_name_on_boot(
            existing.as_ref().map(|device| device.name.as_str()),
            &config.device_name,
        ),
        platform: config.platform.clone(),
        last_seen_at: Some(now),
        created_at: existing.and_then(|d| d.created_at).or(Some(now)),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
    })?;
    Ok(())
}

fn device_name_on_boot(existing_name: Option<&str>, detected_name: &str) -> String {
    existing_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(detected_name)
        .to_string()
}

#[cfg(test)]
mod tests;

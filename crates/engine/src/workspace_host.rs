//! WorkspaceHost — owns the per-user workspace **registry** (docs/
//! registry-sync.md; replaces the Loro workspace doc after the 2026-07/08
//! wedge incidents): local snapshot persistence, edge room sync
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
//!
//! Migration: first boot after the update finds no `registry1` snapshot, reads
//! the legacy `workspace2` Loro snapshot, and seeds the registry from it as
//! pending upserts (historical HLCs — live writes always win). The overlay
//! serves the full sidebar before any server contact; the old `ws4` rooms are
//! simply never joined again. The legacy snapshot is kept for rollback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use cypher_doc::{DeletedDevice, DeletedSpace, REGISTRY_DOC_ID, RegistryDoc, WorkspaceDoc};
use cypher_proto::{
    Chat, ChatConfig, ChildAgentProfile, ChildChat, Device, HarnessId, SandboxLevel, Session,
    Space, SubagentRunMode,
};
use cypher_sync::{DocsStore, RegistryClient, RegistryTransport, RegistryTuning, SyncError};

use crate::doc_host::EdgeConfig;
use crate::{EngineError, now_ms};

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

/// Legacy Loro workspace snapshot row — now only read once, as the migration
/// source for the registry seed. Kept on disk for rollback.
pub const WORKSPACE_DOC_ID: &str = "workspace2";
/// Legacy (pre-spaces) snapshot row — best-effort deleted on open.
const LEGACY_WORKSPACE_DOC_ID: &str = "workspace";
/// Org used when none is configured (matches the edge's dev-mode `user@org` bearers).
pub const DEFAULT_ORG_ID: &str = "dev-org";
/// User used when none is configured (dev mode without a bearer).
pub const DEFAULT_USER_ID: &str = "dev-user";
/// Presence beat cadence.
const PRESENCE_INTERVAL_MS: u64 = 15_000;
/// A presence heartbeat younger than this marks the device alive (3 missed
/// beats = offline). Also the "peer is reachable" signal that clears the
/// peer-dial cooldown.
const PRESENCE_FRESH_MS: i64 = 45_000;
/// Relay-status probe cadence. Presence heartbeats ride the registry room, so
/// any registry pathology (or our own room connection being down) silently
/// starves them — and every device looks offline while its relay works fine.
/// Before believing "offline", ask the device's DeviceRoom
/// (`GET /device/{id}/status` → `hostConnected`), which tracks the host socket
/// authoritatively and shares no machinery with the registry room. Probes only
/// run for devices whose heartbeat is stale, so the steady state (healthy
/// room, fresh beats) sends no extra traffic.
const RELAY_PROBE_INTERVAL_MS: u64 = 30_000;
/// Per-request timeout for a relay-status probe.
const RELAY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Debounce window for local snapshot saves after a change.
const SNAPSHOT_DEBOUNCE_MS: u64 = 1_000;
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

async fn token_revoked(token: &Option<Arc<dyn cypher_rpc::TokenSource>>) -> bool {
    match token {
        Some(token) => token.token().await.is_none(),
        // Fixed test/dev URLs have no revocable credential source.
        None => false,
    }
}

/// Plain-HTTPS registry pull/push. This intentionally owns the EdgeConfig
/// instead of deriving requests from the WebSocket URL: HTTP credentials must
/// stay in `Authorization: Bearer`, never leak into query strings or logs.
struct EdgeRegistryTransport {
    http: reqwest::Client,
    edge: EdgeConfig,
    org_id: String,
}

impl EdgeRegistryTransport {
    fn endpoint(&self, leaf: &str) -> String {
        format!(
            "{}/registry/{}/{leaf}",
            self.edge.url.trim_end_matches('/'),
            self.org_id
        )
    }
}

impl RegistryTransport for EdgeRegistryTransport {
    fn fetch(&self, since: u64) -> futures::future::BoxFuture<'static, Result<String, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.endpoint("rows");
        let device = edge.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .get(url)
                .query(&[
                    ("since", since.to_string()),
                    ("device", device),
                    ("beat", "1".to_string()),
                ])
                .bearer_auth(bearer)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "registry pull HTTP {}",
                    response.status()
                )));
            }
            response
                .text()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }

    fn push(&self, body: String) -> futures::future::BoxFuture<'static, Result<String, SyncError>> {
        let http = self.http.clone();
        let edge = self.edge.clone();
        let url = self.endpoint("push");
        let device = edge.device_id.clone();
        Box::pin(async move {
            let bearer = edge
                .bearer()
                .await
                .ok_or_else(|| SyncError::Auth("signed out".into()))?;
            let response = http
                .post(url)
                .query(&[("device", device)])
                .header("content-type", "application/json")
                .bearer_auth(bearer)
                .body(body)
                .send()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SyncError::Protocol(format!(
                    "registry push HTTP {}",
                    response.status()
                )));
            }
            response
                .text()
                .await
                .map_err(|err| SyncError::WebSocket(err.to_string()))
        })
    }
}

/// Quiet-probe cadence for the registry room: fixed at 15 minutes. One room
/// per engine, so the fixed cadence costs ~100 DO wakes/day total, and the
/// probe is deadline-checked — a mute room is detected within
/// probe cadence + 10s instead of hours (2026-08-04 deaf-socket lesson).
const REGISTRY_PROBE_QUIET: std::time::Duration = std::time::Duration::from_secs(900);
/// Deaf-socket escalation: live peer presence dark this long after the
/// tripwire probe → redial on a fresh socket (see `check_presence_deafness`).
const PRESENCE_DEAF_REDIAL_MS: i64 = 60_000;

/// State for the presence deafness tripwire. Presence heartbeats ride the
/// SAME socket and the same DO broadcast fan-out as row updates, so "peers I
/// was seeing live via this room all went dark at once" is a *delivery*
/// signal, and it is bounded (~45-60s) at zero server cost — every device
/// already heartbeats each 15s. The monotonic seen-cache and the relay
/// status probe deliberately keep devices *fresh-looking* through other
/// paths; they must never feed this tripwire (they'd mask exactly the
/// failure it exists to catch — 2026-08-04 deaf-socket incident).
#[derive(Default)]
struct PresenceWatch {
    /// Armed once at least one OTHER device has been seen live via the
    /// room's presence map this session.
    armed: bool,
    /// Epoch ms when live peers first all went dark (0 = not dark).
    dark_since_ms: i64,
    /// The cheap first response (probe) already fired.
    probed: bool,
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
    /// Called with a device id whenever its presence heartbeat proves it alive —
    /// wired to `LinkCache::reset_cooldown` so a peer that comes back is dialed
    /// immediately instead of waiting out the failure backoff.
    peer_alive: Mutex<Option<PeerAliveHook>>,
    /// Deaf-socket tripwire state — see `check_presence_deafness`.
    presence_watch: Mutex<PresenceWatch>,
}

/// "This peer is alive" callback (device id) — see `WorkspaceHost::set_peer_alive_hook`.
pub type PeerAliveHook = Arc<dyn Fn(&str) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct WorkspaceHost {
    inner: Arc<WorkspaceHostInner>,
}

impl WorkspaceHost {
    /// Load (or migrate, or init) the registry, upsert this device's row, start
    /// the change-driven task, and join the edge registry room when configured.
    pub fn open(store: Arc<DocsStore>, config: WorkspaceHostConfig) -> Result<Self, EngineError> {
        let mut doc = match store.load_snapshot(REGISTRY_DOC_ID)? {
            Some(bytes) => RegistryDoc::from_bytes(&bytes, &config.device_id)
                .map_err(|e| EngineError::Other(format!("registry snapshot load failed: {e}")))?,
            None => {
                // MIGRATION (instant, one-time): seed from the legacy Loro
                // workspace snapshot when one exists. Seeds are pending upserts
                // with historical HLCs — the overlay serves the full sidebar
                // immediately, the room converges on first join, and any live
                // write beats a migrated value. The legacy snapshot stays on
                // disk for rollback.
                let mut doc = RegistryDoc::new(&config.device_id);
                match store.load_snapshot(WORKSPACE_DOC_ID) {
                    Ok(Some(bytes)) => {
                        let raw = loro::LoroDoc::new();
                        match raw.import(&bytes) {
                            Ok(_) => {
                                let legacy = WorkspaceDoc::from_doc(raw);
                                match legacy.read_all() {
                                    Ok(state) => match doc.seed_from_workspace(&state) {
                                        Ok(rows) => {
                                            tracing::info!(
                                                rows,
                                                "migrated legacy workspace doc into the registry"
                                            );
                                        }
                                        Err(err) => {
                                            tracing::warn!(error = %err, "workspace migration seed failed");
                                        }
                                    },
                                    Err(err) => {
                                        tracing::warn!(error = %err, "legacy workspace read failed; starting empty");
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, "legacy workspace import failed; starting empty");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "legacy workspace snapshot load failed; starting empty");
                    }
                }
                doc
            }
        };
        // Destructive-break hygiene: the pre-spaces row stays unreachable.
        store.delete_snapshot(LEGACY_WORKSPACE_DOC_ID).ok();

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
                peer_alive: Mutex::new(None),
                presence_watch: Mutex::new(PresenceWatch::default()),
            }),
        };
        // Persist immediately: after this boot the migration source is never
        // read again, so the registry snapshot must exist even if the process
        // dies before the first debounced save.
        host.inner.save_snapshot();
        host.join_room();
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

    fn spawn_join(
        &self,
        url: Arc<dyn cypher_sync::UrlProvider>,
        mut token_changes: Option<tokio::sync::watch::Receiver<u64>>,
        token: Option<Arc<dyn cypher_rpc::TokenSource>>,
    ) {
        let org_id = self.inner.config.org_id.clone();
        let reg = self.inner.reg.clone();
        let device_id = self.inner.config.device_id.clone();
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut wake = cypher_sync::wake::subscribe();
            // `RegistryClient` only self-reconnects AFTER a first successful
            // join; an INITIAL failure (a 500 from an overloaded DO, a token
            // racing a refresh, an edge deploy) must not end this task and
            // leave the device offline until an app restart. Retry the first
            // join on a capped, jittered backoff so a transient blip self-heals.
            let mut backoff = JOIN_RETRY_BASE;
            loop {
                if weak.upgrade().is_none() {
                    return; // host dropped
                }
                let tuning = RegistryTuning {
                    probe_quiet: REGISTRY_PROBE_QUIET,
                    ..RegistryTuning::default()
                };
                let client_result = if let Some(inner) = weak.upgrade() {
                    let transport = inner.config.edge.clone().map(|edge| {
                        Arc::new(EdgeRegistryTransport {
                            http: reqwest::Client::builder()
                                .connect_timeout(std::time::Duration::from_secs(10))
                                .timeout(std::time::Duration::from_secs(30))
                                .build()
                                .expect("registry HTTP client"),
                            edge,
                            org_id: org_id.clone(),
                        }) as Arc<dyn RegistryTransport>
                    });
                    match transport {
                        Some(transport) => {
                            RegistryClient::connect_via_transport_tuned(
                                url.clone(),
                                reg.clone(),
                                &device_id,
                                tuning,
                                transport,
                            )
                            .await
                        }
                        None => {
                            RegistryClient::connect_via_tuned(
                                url.clone(),
                                reg.clone(),
                                &device_id,
                                tuning,
                            )
                            .await
                        }
                    }
                } else {
                    return;
                };
                match client_result {
                    Ok(client) => {
                        let client = Arc::new(client);
                        client.set_presence(now_ms());
                        let mut events = client.events();
                        if token_revoked(&token).await {
                            return;
                        }
                        let Some(inner) = weak.upgrade() else { return };
                        *lock(&inner.room) = Some(client.clone());
                        // A transport-backed client is ready local-first,
                        // before either HTTP or WS has returned server truth.
                        // Do not open the orphan-sweep gate until the first
                        // state response has actually been applied.
                        if client.stats().server_known {
                            inner.registry_synced.store(true, Ordering::Relaxed);
                            inner.reconcile_own_device();
                        }
                        inner.bump_changed();
                        tracing::info!(org = %org_id, "registry room joined");
                        drop(inner);
                        // The slot is the sole owner. This lets engine-level
                        // revocation close the socket synchronously by taking it.
                        drop(client);
                        // The event pump lives for the client's whole life
                        // (across its self-reconnects); it ends only when the
                        // client is dropped at host teardown.
                        loop {
                            tokio::select! {
                                event = events.recv() => match event {
                                    Ok(cypher_sync::RegistryEvent::Applied)
                                    | Ok(cypher_sync::RegistryEvent::Connected) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        if lock(&inner.room)
                                            .as_ref()
                                            .is_some_and(|room| room.stats().server_known)
                                        {
                                            inner.registry_synced.store(true, Ordering::Relaxed);
                                            inner.reconcile_own_device();
                                        }
                                        inner.bump_changed();
                                    }
                                    Ok(cypher_sync::RegistryEvent::Presence) => {
                                        let Some(inner) = weak.upgrade() else { return };
                                        inner.publish();
                                    }
                                    Ok(cypher_sync::RegistryEvent::Disconnected) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                },
                                _ = token_changed(&mut token_changes) => {
                                    if token_revoked(&token).await {
                                        tracing::info!(org = %org_id,
                                            "registry credentials removed; leaving room");
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(inner) = weak.upgrade() {
                            *lock(&inner.room) = None;
                        }
                        return;
                    }
                    Err(err) => {
                        tracing::warn!(org = %org_id, error = %err, backoff_ms = backoff.as_millis() as u64,
                            "registry room join failed; retrying");
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff + join_retry_jitter()) => {
                        backoff = (backoff * 2).min(JOIN_RETRY_CAP);
                    }
                    _ = wake.recv() => {
                        backoff = JOIN_RETRY_BASE;
                    }
                    _ = token_changed(&mut token_changes) => {
                        if token_revoked(&token).await {
                            return;
                        }
                        backoff = JOIN_RETRY_BASE;
                    }
                }
            }
        });
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

    pub fn device_id(&self) -> &str {
        &self.inner.config.device_id
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
    /// down for a fresh socket, so a deaf-receiving room (2026-08-04 incident)
    /// heals within seconds of the user looking at the app.
    pub fn probe(&self) {
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

    /// WatchSessions source: remote devices' rows from the registry merged with
    /// this engine's live status watch (the local view is fresher for our own runs).
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
            id: crate::new_id(),
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

    // ── Mutate surface (LWW writes accepted from any device) ────────────────

    /// Create a chat, usually *in a project*: the project fixes the host device
    /// and base cwd (`cwd` override = an isolated-worktree path). With no
    /// `space_id` the chat is project-less: `device_id` picks the host and the
    /// cwd defaults to `~` (expanded host-side when the run spawns).
    pub fn create_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        device_id: Option<&str>,
        config: Option<ChatConfig>,
        cwd: Option<String>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(chat_id))?.is_some() {
            return Ok(()); // idempotent: optimistic client retries never duplicate
        }
        let space = match space_id {
            Some(space_id) => match self.read(|doc| doc.space(space_id))? {
                Some(space) => Some(space),
                None => return Err(EngineError::Other(format!("no such space: {space_id}"))),
            },
            None => None,
        };
        let host_device = match (&space, device_id) {
            (Some(space), _) => space.device_id.clone(),
            (None, Some(device_id)) => device_id.to_string(),
            (None, None) => {
                return Err(EngineError::Other(
                    "createChat needs a spaceId or a deviceId".into(),
                ));
            }
        };
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: chat_id.to_string(),
                device_id: host_device.clone(),
                title: None,
                archived: false,
                cwd: Some(cwd.unwrap_or_else(|| {
                    space
                        .as_ref()
                        .map(|s| s.path.clone())
                        .unwrap_or_else(|| "~".to_string())
                })),
                branch: None,
                checkout_id: None,
                config,
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                // Born on chat2: a brand-new chat has an empty doc — nothing
                // to seed, no migration race to lose. Only pre-existing chats
                // go through the seed+flip path (the host migration sweep).
                room_gen: Some(2),
                harness_session_cwd: None,
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                child: None,
            })
        })?;
        Ok(())
    }

    // ── spaces (Mutate surface + owner stamps) ──────────────────────────────

    /// Create a space (any device). Idempotent by id; a live duplicate of the
    /// same `(deviceId, path)` is a no-op backstop (the UI reuses via
    /// WatchSpaces). `git_detected` is seeded from the picker's FolderEntry;
    /// the owning device's SpacesSync re-verifies.
    pub fn create_space(
        &self,
        space_id: &str,
        device_id: &str,
        path: &str,
        name: Option<String>,
        git_detected: bool,
    ) -> Result<(), EngineError> {
        let spaces = self.read(|doc| doc.read_spaces())?;
        if spaces
            .iter()
            .any(|s| s.id == space_id || (s.device_id == device_id && s.path == path))
        {
            return Ok(());
        }
        self.mutate(|doc| {
            doc.upsert_space(&Space {
                id: space_id.to_string(),
                device_id: device_id.to_string(),
                path: path.to_string(),
                name,
                git_detected,
                git_checked_at: None,
                checkout_id: None,
                created_at: Utc::now(),
            })
        })?;
        Ok(())
    }

    pub fn rename_space(&self, space_id: &str, name: Option<&str>) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_space(space_id, name))?)
    }

    /// Hard-delete a space and its chats (registry cascade — one atomic batch).
    /// The caller (rpc layer) tears down live runs / doc-host handles for the
    /// returned chat ids.
    pub fn delete_space(&self, space_id: &str) -> Result<DeletedSpace, EngineError> {
        Ok(self.mutate(|doc| doc.delete_space(space_id))?)
    }

    /// Synced seen marker (any device; LWW + monotonic guard in the doc layer).
    pub fn mark_chat_seen(
        &self,
        chat_id: &str,
        at: chrono::DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_seen(chat_id, at))?)
    }

    /// Owner-only git stamp (SpacesSync). Refuses rows owned by another device.
    pub fn set_space_git(
        &self,
        space_id: &str,
        detected: bool,
        checkout_id: Option<&str>,
    ) -> Result<bool, EngineError> {
        match self.read(|doc| doc.space(space_id))? {
            Some(space) if space.device_id == self.inner.config.device_id => {
                Ok(self
                    .mutate(|doc| doc.set_space_git(space_id, detected, checkout_id, Utc::now()))?)
            }
            Some(space) => {
                tracing::warn!(
                    space = %space_id, owner = %space.device_id,
                    "refusing git stamp on space owned by another device"
                );
                Ok(false)
            }
            None => Ok(false),
        }
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, EngineError> {
        Ok(self.read(|doc| doc.read_spaces())?)
    }

    pub fn rename_chat(&self, chat_id: &str, title: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_chat(chat_id, title))?)
    }

    /// Backdate a chat's activity timestamps (epoch ms). Returns false when
    /// the chat doesn't exist.
    pub fn set_chat_activity(
        &self,
        chat_id: &str,
        last_message_at: Option<i64>,
        created_at: Option<i64>,
    ) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        if let Some(ms) = last_message_at {
            chat.last_message_at = chrono::DateTime::<Utc>::from_timestamp_millis(ms);
        }
        if let Some(ms) = created_at
            && let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        {
            chat.created_at = at;
        }
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Re-home a chat to another device (tooling/seeds; a future device
    /// migration flow will drive this). Returns false when the chat doesn't
    /// exist.
    pub fn set_chat_host(&self, chat_id: &str, device_id: &str) -> Result<bool, EngineError> {
        let Some(mut chat) = self.read(|doc| doc.chat(chat_id))? else {
            return Ok(false);
        };
        chat.device_id = device_id.to_string();
        self.mutate(|doc| doc.upsert_chat(&chat))?;
        Ok(true)
    }

    /// Upsert a chat row copied verbatim from another profile (local→synced
    /// import). Same write path as every live mutation, so the row persists
    /// and pushes like any other; the caller fixes `room_gen` beforehand.
    pub fn import_chat_row(&self, chat: &Chat) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_chat(chat))?)
    }

    /// Upsert a space row copied verbatim from another profile (local→synced
    /// import).
    pub fn import_space_row(&self, space: &Space) -> Result<(), EngineError> {
        Ok(self.mutate(|doc| doc.upsert_space(space))?)
    }

    /// Flip the chat's sync room generation (docs/chat2-sync.md M2) — the
    /// host calls this in the same breath as seeding the chat2 checkpoint.
    pub fn set_chat_room_gen(&self, chat_id: &str, room_gen: u32) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_room_gen(chat_id, room_gen))?)
    }

    pub fn set_chat_archived(&self, chat_id: &str, archived: bool) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_archived(chat_id, archived))?)
    }

    /// LWW full-config replace on the chat row (zeron `SetChatConfig` — the
    /// composer's mid-session model/reasoning/options changes). Returns false
    /// when the chat doesn't exist.
    pub fn set_chat_config(&self, chat_id: &str, config: &ChatConfig) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_config(chat_id, config))?)
    }

    /// Tombstone: removes the chats (and session-status) row; the per-chat session
    /// doc remains untouched.
    pub fn delete_chat(&self, chat_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.delete_chat(chat_id))?)
    }

    /// Sidebar freshness with an explicit timestamp: set the promoted Side
    /// Chat's preview + last-message activity from its transcript's newest
    /// message (round-21 audit — a promoted chat must not land blank in the
    /// sidebar). Best-effort: `false` when the row is missing.
    pub fn set_chat_last_message(
        &self,
        chat_id: &str,
        preview: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_last_message(chat_id, preview, at))?)
    }

    /// Promote a temporary Side Chat into a normal ROOT chat (round 21): a
    /// non-child Chat row with the SAME id, inheriting the parent's device /
    /// space / cwd / branch / config / checkout (deliberately NOT the parent's
    /// harness session — the promoted chat's own session continuity rides the
    /// in-memory harness-session backfill). The row's title is deterministic,
    /// derived from the selected quote (`title`). Born on chat2 (`room_gen: 2`).
    ///
    /// Idempotent: returns `Ok(false)` when a row already exists (a lost
    /// PromoteSideChat reply retried after the first promotion landed) — the
    /// caller treats that as already-promoted rather than double-writing.
    pub fn promote_side_chat(
        &self,
        side_chat_id: &str,
        parent: &Chat,
        title: &str,
    ) -> Result<bool, EngineError> {
        if self.read(|doc| doc.chat(side_chat_id))?.is_some() {
            return Ok(false);
        }
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: side_chat_id.to_string(),
                device_id: parent.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: parent.cwd.clone(),
                branch: parent.branch.clone(),
                checkout_id: parent.checkout_id.clone(),
                config: parent.config.clone(),
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                room_gen: Some(2),
                harness_session_cwd: None,
                space_id: parent.space_id.clone(),
                last_seen_at: None,
                child: None,
            })
        })?;
        Ok(true)
    }

    /// Session Fork (v1): create the NEW durable root chat row for a fork.
    /// Copies the source's host device / space / cwd / branch / checkout /
    /// config (same checkout, same root) verbatim; the fork's own identity is
    /// the `<source title> — Fork` title and — when the fork materialized a
    /// persisted pi session — the fresh harness session path + cwd. An
    /// EMPTY-CONTEXT fork before the first user carries NO session yet
    /// (`harness_session_id` / `harness_session_cwd` = `None`): its first
    /// send starts a fresh pi session from empty context (the source is
    /// Pi-configured, so normal dispatch works). Born on chat2
    /// (`room_gen: 2`) like every new chat. The sidebar TIMESTAMP is birth
    /// `now` (`last_message_at` = `last_seen_at` = now): a fork is NEW
    /// activity and must never be buried under the source's old timestamp —
    /// only the endpoint PREVIEW comes from the newest copied message.
    /// Idempotent by id — a lost-reply retry never mints a twin.
    pub fn create_fork_chat(
        &self,
        fork_chat_id: &str,
        source: &Chat,
        title: &str,
        harness_session_id: Option<&str>,
        harness_session_cwd: Option<&str>,
        last_message_preview: Option<String>,
    ) -> Result<(), EngineError> {
        if self.read(|doc| doc.chat(fork_chat_id))?.is_some() {
            return Ok(()); // idempotent: a retry never duplicates
        }
        let now = Utc::now();
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: fork_chat_id.to_string(),
                device_id: source.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: source.cwd.clone(),
                branch: source.branch.clone(),
                checkout_id: source.checkout_id.clone(),
                config: source.config.clone(),
                last_message_preview,
                // Fresh activity: a fork sorts as NEWLY created, never by the
                // source's old transcript timestamp.
                last_message_at: Some(now),
                created_at: now,
                harness_session_id: harness_session_id.map(str::to_string),
                room_gen: Some(2),
                harness_session_cwd: harness_session_cwd.map(str::to_string),
                space_id: source.space_id.clone(),
                // Seen on birth: the caller selects the fork immediately, so
                // it must never flash a "completed (unseen)" badge.
                last_seen_at: Some(now),
                child: None,
            })
        })?;
        Ok(())
    }

    /// Create a Cypher-hosted child subagent chat (`StartSubagent` bridge): a
    /// Pi-configured, titled chat row carrying the additive child metadata
    /// (parent chat id + parent run id + agent/task/mode + persisted profile).
    /// Inherits the parent's space/device/cwd/sandbox. Deterministic +
    /// idempotent by `(parent_chat_id, parent_run_id)` — a repeat start
    /// reports [`ChildChatOutcome::Existing`] with the existing child's id
    /// instead of minting a twin (the caller must then NOT queue a second
    /// run). The messaging channel is deliberately NOT persisted (host-local
    /// absolute path — see [`ChildChat`]); the caller registers it in a local
    /// runtime map for the initial run.
    #[allow(clippy::too_many_arguments)] // child-start seam, not a public API
    pub fn create_child_chat(
        &self,
        parent: &Chat,
        parent_run_id: &str,
        agent: &str,
        task: &str,
        mode: SubagentRunMode,
        tool_call_id: Option<String>,
        profile: ChildAgentProfile,
        title: &str,
    ) -> Result<ChildChatOutcome, EngineError> {
        for chat in self.read_chats()? {
            if let Some(child) = &chat.child
                && child.parent_chat_id == parent.id
                && child.parent_run_id == parent_run_id
            {
                return Ok(ChildChatOutcome::Existing(chat.id));
            }
        }
        let chat_id = crate::new_id();
        let sandbox = parent
            .config
            .as_ref()
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite);
        self.mutate(|doc| {
            doc.upsert_chat(&Chat {
                id: chat_id.clone(),
                device_id: parent.device_id.clone(),
                title: Some(title.to_string()),
                archived: false,
                cwd: parent.cwd.clone(),
                branch: None,
                checkout_id: None,
                config: Some(ChatConfig {
                    harness: HarnessId::Pi,
                    model: profile.model.clone(),
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox,
                }),
                last_message_preview: None,
                last_message_at: None,
                created_at: Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                space_id: parent.space_id.clone(),
                last_seen_at: None,
                room_gen: Some(2),
                child: Some(ChildChat {
                    parent_chat_id: parent.id.clone(),
                    parent_run_id: parent_run_id.to_string(),
                    agent: agent.to_string(),
                    task: task.to_string(),
                    mode,
                    tool_call_id,
                    profile,
                }),
            })
        })?;
        Ok(ChildChatOutcome::Created(chat_id))
    }

    /// Child chat rows whose parent is `chat_id` (cascade-delete targets).
    pub fn child_chats(&self, parent_chat_id: &str) -> Result<Vec<Chat>, EngineError> {
        Ok(self
            .read_chats()?
            .into_iter()
            .filter(|c| c.parent_chat_id() == Some(parent_chat_id))
            .collect())
    }

    pub fn rename_device(&self, device_id: &str, name: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.rename_device(device_id, name))?)
    }

    /// Unpair another device: tombstone its registry row so it drops out of
    /// sync. Refuses to delete THIS device — sign out is the way to leave.
    pub fn delete_device(&self, device_id: &str) -> Result<DeletedDevice, EngineError> {
        if device_id == self.inner.config.device_id {
            return Err(EngineError::Other("cannot delete this device".into()));
        }
        Ok(self.mutate(|doc| doc.delete_device(device_id))?)
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

    // ── git metadata (diff-sync host writes) ────────────────────────────────

    /// HEAD-watcher reconciliation: the branch checked out at the chat's cwd.
    pub fn set_chat_branch(&self, chat_id: &str, branch: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_branch(chat_id, branch))?)
    }

    /// Retarget a chat onto another folder (mid-session switch to an existing
    /// worktree). Resume is cwd-scoped — the next run there starts fresh.
    pub fn set_chat_cwd(&self, chat_id: &str, cwd: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_cwd(chat_id, cwd))?)
    }

    /// Canonical checkout identity for the chat's cwd (diff grouping key).
    pub fn set_chat_checkout(&self, chat_id: &str, checkout_id: &str) -> Result<bool, EngineError> {
        Ok(self.mutate(|doc| doc.set_chat_checkout(chat_id, checkout_id))?)
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
                // send_replace, NOT send: `watch::Sender::send` drops the value when
                // no receiver exists yet, so a stream subscribed later would start
                // from a stale snapshot (found the hard way by the e2e smoke).
                self.chats_tx.send_replace(state.chats);
                self.devices_tx.send_replace(state.devices);
                self.sessions_tx.send_replace(state.sessions);
                self.spaces_tx.send_replace(state.spaces);
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry read failed");
            }
        }
    }

    /// Fold the 15s presence heartbeats into the device rows' `lastSeenAt`
    /// before publishing. The row is written on boot/shutdown ONLY (server-
    /// state hygiene), so without this overlay every device looks offline
    /// ~70s after its boot — and a genuinely dead host is indistinguishable
    /// from slow sync. Fresh remote heartbeats also fire the peer-alive hook
    /// (dial-cooldown reset).
    fn overlay_presence(&self, devices: &mut [Device]) {
        let mut alive_peers: Vec<String> = Vec::new();
        {
            // No live room handle is NOT "everyone is offline": the cache (fed
            // by past heartbeats and the relay-status probe) still overlays —
            // a dead registry room must never fake an offline badge for
            // devices whose relay connection is fine.
            let room = lock(&self.room);
            let live_map = room
                .as_ref()
                .map(|room| room.presence())
                .unwrap_or_default();
            let mut seen = lock(&self.presence_seen);
            let now = now_ms();
            let mut live_fresh_peers = 0usize;
            for device in devices.iter_mut() {
                // RegistryClient intentionally exposes REMOTE presence only.
                // The local engine being able to publish this view is itself
                // authoritative proof that its own device is online.
                if device.id == self.config.device_id {
                    device.last_seen_at = chrono::DateTime::<Utc>::from_timestamp_millis(now);
                    continue;
                }
                // Freshest of the live presence entry and the cache: the room
                // map's 30s TTL (and its empty state right after a rejoin)
                // must not erase freshness this engine already witnessed — the
                // device is offline only once heartbeats genuinely stop
                // arriving for the UI's whole online window.
                let live = live_map.get(&device.id).copied();
                if live.is_some_and(|ms| now.saturating_sub(ms) < PRESENCE_FRESH_MS) {
                    live_fresh_peers += 1;
                }
                let cached = seen.get(&device.id).copied();
                let Some(ms) = live.into_iter().chain(cached).max() else {
                    continue;
                };
                seen.insert(device.id.clone(), ms);
                if let Some(at) = chrono::DateTime::<Utc>::from_timestamp_millis(ms)
                    && device.last_seen_at.is_none_or(|prev| prev < at)
                {
                    device.last_seen_at = Some(at);
                }
                if now.saturating_sub(ms) < PRESENCE_FRESH_MS {
                    alive_peers.push(device.id.clone());
                }
            }
            if let Some(room) = room.as_ref() {
                self.check_presence_deafness(room, live_fresh_peers, now);
            }
        }
        if alive_peers.is_empty() {
            return;
        }
        let hook = lock(&self.peer_alive).clone();
        if let Some(hook) = hook {
            for id in &alive_peers {
                hook(id);
            }
        }
    }

    /// The deaf-socket tripwire (see [`PresenceWatch`]). LIVE presence
    /// freshness only — never the seen-cache or relay probe. Escalation
    /// ladder: first all-dark observation → deadline-checked probe (free on a
    /// healthy room); still dark [`PRESENCE_DEAF_REDIAL_MS`] later → fresh-
    /// socket redial (the only cure when the server→client path drops even
    /// probe answers). Disarms after the redial and re-arms when a peer is
    /// next seen live, so a genuinely-offline fleet costs one probe + one
    /// redial, ever.
    fn check_presence_deafness(&self, room: &RegistryClient, live_fresh_peers: usize, now: i64) {
        let mut watch = lock(&self.presence_watch);
        if live_fresh_peers > 0 {
            watch.armed = true;
            watch.dark_since_ms = 0;
            watch.probed = false;
            return;
        }
        if !watch.armed {
            return;
        }
        if watch.dark_since_ms == 0 {
            watch.dark_since_ms = now;
        }
        if !watch.probed {
            tracing::info!(
                "all live peer presence went dark; probing registry room (deaf-socket tripwire)"
            );
            room.probe();
            watch.probed = true;
        } else if now.saturating_sub(watch.dark_since_ms) > PRESENCE_DEAF_REDIAL_MS {
            tracing::warn!(
                dark_ms = now.saturating_sub(watch.dark_since_ms),
                "peer presence still dark after probe; requesting registry room redial"
            );
            room.redial();
            watch.armed = false;
            watch.dark_since_ms = 0;
            watch.probed = false;
        }
    }

    fn save_snapshot(&self) {
        let bytes = lock(&self.reg).to_bytes();
        match bytes {
            Ok(bytes) => {
                if let Err(err) = self.store.save_snapshot(REGISTRY_DOC_ID, &bytes) {
                    tracing::warn!(error = %err, "registry snapshot save failed");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "registry snapshot export failed");
            }
        }
    }

    /// Presence heartbeat — a memory-only frame on the room, never a row write.
    fn presence_tick(&self) {
        if let Some(room) = lock(&self.room).as_ref() {
            room.set_presence(now_ms());
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

/// Local live statuses win for this device's chats; every other device's rows come
/// from the registry. Sorted by chat id (stable stream output).
fn merge_sessions(device_id: &str, rows: &[Session], local: &[Session]) -> Vec<Session> {
    let mut merged: std::collections::HashMap<String, Session> = rows
        .iter()
        .filter(|s| s.device_id != device_id)
        .map(|s| (s.chat_id.clone(), s.clone()))
        .collect();
    for session in local {
        merged.insert(session.chat_id.clone(), session.clone());
    }
    let mut list: Vec<Session> = merged.into_values().collect();
    list.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    list
}

/// Background task: relay-verified presence. Every [`RELAY_PROBE_INTERVAL_MS`],
/// for each known device whose merged heartbeat freshness has gone stale, ask
/// its DeviceRoom whether the host socket is live (`/device/{id}/status`); a
/// positive answer refreshes the presence cache so the overlay keeps the badge
/// online. The DeviceRoom shares no machinery with the registry room, so a
/// false "offline" now requires BOTH independent paths to be down — at which
/// point the device is, for every purpose the app has, genuinely offline.
/// Steady state (healthy room, fresh heartbeats) probes nothing.
async fn relay_probe_task(weak: Weak<WorkspaceHostInner>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(RELAY_PROBE_INTERVAL_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // consume the immediate first tick
    let client = reqwest::Client::new();
    loop {
        tick.tick().await;
        let Some(inner) = weak.upgrade() else { return };
        let Some(edge) = inner.config.edge.clone() else {
            return;
        };
        let self_id = inner.config.device_id.clone();
        let now = now_ms();
        let stale: Vec<String> = {
            let Ok(devices) = lock(&inner.reg).read_devices() else {
                continue;
            };
            let seen = lock(&inner.presence_seen);
            devices
                .into_iter()
                .filter(|d| d.id != self_id)
                .filter(|d| {
                    seen.get(&d.id)
                        .is_none_or(|ms| now.saturating_sub(*ms) >= PRESENCE_FRESH_MS)
                })
                .map(|d| d.id)
                .collect()
        };
        drop(inner);
        if stale.is_empty() {
            continue;
        }
        let Some(bearer) = edge.bearer().await else {
            continue; // signed out
        };
        let mut refreshed = false;
        for device_id in stale {
            let url = format!(
                "{}/device/{}/status",
                edge.url.trim_end_matches('/'),
                device_id
            );
            let response = client
                .get(&url)
                .bearer_auth(&bearer)
                .timeout(RELAY_PROBE_TIMEOUT)
                .send()
                .await;
            let Ok(response) = response else { continue };
            if !response.status().is_success() {
                continue;
            }
            let Ok(body) = response.json::<serde_json::Value>().await else {
                continue;
            };
            if body
                .get("hostConnected")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                let Some(inner) = weak.upgrade() else { return };
                lock(&inner.presence_seen).insert(device_id.clone(), now_ms());
                tracing::debug!(device = %device_id, "presence: relay-verified alive");
                refreshed = true;
            }
        }
        if refreshed && let Some(inner) = weak.upgrade() {
            inner.publish();
        }
    }
}

/// Background task: reacts to registry changes (local mutations and applied
/// server frames) by re-publishing the watch channels and debouncing snapshots,
/// and refreshes presence every [`PRESENCE_INTERVAL_MS`]. Holds only a weak
/// handle so a dropped host tears the task down.
async fn workspace_task(weak: Weak<WorkspaceHostInner>, mut changed_rx: watch::Receiver<u64>) {
    let mut presence =
        tokio::time::interval(std::time::Duration::from_millis(PRESENCE_INTERVAL_MS));
    presence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    presence.tick().await; // consume the immediate first tick
    let mut save_deadline: Option<tokio::time::Instant> = None;
    loop {
        let sleep_until = save_deadline.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            changed = changed_rx.changed() => {
                if changed.is_err() {
                    break; // host (and its change sender) is gone
                }
                let Some(inner) = weak.upgrade() else { break };
                inner.publish();
                if save_deadline.is_none() {
                    save_deadline = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(SNAPSHOT_DEBOUNCE_MS),
                    );
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if save_deadline.is_some() => {
                save_deadline = None;
                let Some(inner) = weak.upgrade() else { break };
                inner.save_snapshot();
            }
            _ = presence.tick() => {
                let Some(inner) = weak.upgrade() else { break };
                inner.presence_tick();
                // Re-publish on the same cadence: remote heartbeats decay when a
                // device goes silent, and watchers (the UI online dot, "host
                // offline" hints) need a tick to observe that staleness.
                inner.publish();
            }
        }
    }
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
        .filter(|name| {
            let name = name.trim();
            !name.is_empty() && name != crate::LEGACY_UNKNOWN_DEVICE_NAME
        })
        .unwrap_or(detected_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeDelta, Utc};
    use cypher_proto::Device;
    use cypher_sync::DocsStore;

    use super::{WorkspaceHost, WorkspaceHostConfig, device_name_on_boot, linked_worktree_root};

    #[test]
    fn boot_repairs_the_legacy_unknown_device_sentinel() {
        assert_eq!(
            device_name_on_boot(Some("unknown-device"), "MacBook Pro"),
            "MacBook Pro"
        );
    }

    #[test]
    fn boot_preserves_a_user_selected_device_name() {
        assert_eq!(
            device_name_on_boot(Some("Work laptop"), "MacBook Pro"),
            "Work laptop"
        );
    }

    #[tokio::test]
    async fn local_device_is_fresh_while_the_host_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "local-device".into(),
                device_name: "Local".into(),
                platform: "linux".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                allow_device_rejoin: false,
            },
        )
        .unwrap();
        let mut devices = vec![Device {
            id: "local-device".into(),
            name: "Local".into(),
            platform: "linux".into(),
            last_seen_at: Some(Utc::now() - TimeDelta::minutes(10)),
            created_at: None,
            version: None,
        }];

        host.inner.overlay_presence(&mut devices);

        let age = Utc::now()
            .signed_duration_since(devices[0].last_seen_at.unwrap())
            .num_seconds();
        assert!(age <= 1, "local presence should be fresh, age={age}s");
    }

    fn open_host(dir: &std::path::Path, device_id: &str, allow_rejoin: bool) -> WorkspaceHost {
        WorkspaceHost::open(
            Arc::new(DocsStore::open(dir).unwrap()),
            WorkspaceHostConfig {
                device_id: device_id.into(),
                device_name: "Local".into(),
                platform: "linux".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
                allow_device_rejoin: allow_rejoin,
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn delete_device_refuses_self_and_keeps_peer_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let host = open_host(dir.path(), "local-device", false);

        host.mutate(|doc| {
            doc.upsert_device(&Device {
                id: "dev-b".into(),
                name: "vps".into(),
                platform: "linux".into(),
                last_seen_at: None,
                created_at: Some(Utc::now()),
                version: None,
            })
        })
        .unwrap();
        host.create_space("sp-b", "dev-b", "/tmp/b", None, false)
            .unwrap();
        host.create_chat("chat-b", Some("sp-b"), None, None, None)
            .unwrap();

        let err = host.delete_device("local-device").unwrap_err();
        assert!(
            err.to_string().contains("cannot delete this device"),
            "{err}"
        );
        assert_eq!(host.read_devices().unwrap().len(), 2);

        let deleted = host.delete_device("dev-b").unwrap();
        assert!(deleted.existed);
        let devices = host.read_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "local-device");
        assert_eq!(host.read_spaces().unwrap().len(), 1);
        assert_eq!(host.read_chats().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unpaired_device_evicts_instead_of_reannouncing() {
        let dir = tempfile::tempdir().unwrap();
        let host = open_host(dir.path(), "local-device", false);
        assert!(!*host.watch_evicted().borrow());

        host.mutate(|doc| doc.delete_device("local-device"))
            .unwrap();
        host.reconcile_own_device();
        assert!(*host.watch_evicted().borrow());
        assert!(
            !host
                .read_devices()
                .unwrap()
                .iter()
                .any(|d| d.id == "local-device")
        );
    }

    #[tokio::test]
    async fn fresh_sign_in_may_revive_a_tombstoned_device() {
        let dir = tempfile::tempdir().unwrap();
        let host = open_host(dir.path(), "local-device", true);
        host.mutate(|doc| doc.delete_device("local-device"))
            .unwrap();
        host.reconcile_own_device();
        assert!(!*host.watch_evicted().borrow());
        assert!(
            host.read_devices()
                .unwrap()
                .iter()
                .any(|d| d.id == "local-device")
        );
    }

    #[test]
    fn linked_worktree_resolves_to_the_checkout_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let wt = dir.path().join("clever-ember");
        std::fs::create_dir_all(root.join(".git").join("worktrees").join("clever-ember")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join(".git/worktrees/clever-ember").display()
            ),
        )
        .unwrap();
        assert_eq!(
            linked_worktree_root(&wt).as_deref(),
            Some(root.to_str().unwrap())
        );
    }

    #[test]
    fn primary_checkouts_and_plain_folders_resolve_to_none() {
        let dir = tempfile::tempdir().unwrap();
        // Primary checkout: `.git` is a directory.
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(primary.join(".git")).unwrap();
        assert_eq!(linked_worktree_root(&primary), None);
        // Not a repo at all.
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(linked_worktree_root(&plain), None);
        // A `.git` file pointing somewhere that is not `<root>/.git/worktrees/<name>`.
        let odd = dir.path().join("odd");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert_eq!(linked_worktree_root(&odd), None);
    }
}

//! Account-bound Workspace v3 host. SQLite owns metadata and its offline
//! outbox; MetadataView is a disposable typed cache/mutation draft.
//! One WorkspaceHub connection carries metadata, demand and remote RPC.
//! Availability/session observations are replaceable leases, not row writes.
//! No old snapshot import, reseed, registry transport or peer HTTP probe exists
//! here. Domain mutations publish only after their native transaction commits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use cypher_proto::metadata::view::{DeletedDevice, DeletedSpace, MetadataError, MetadataView};
use cypher_proto::{
    Chat, ChatConfig, ChildAgentProfile, ChildChat, Device, HarnessId, SandboxLevel, Session,
    Space, SubagentRunMode,
};
use cypher_sync::DocsStore;

use crate::doc_host::EdgeConfig;
use crate::{EngineError, now_ms};
mod v3;

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

/// Retired snapshot key used by tests to verify fresh-v3 non-import.
pub const WORKSPACE_DOC_ID: &str = "workspace2";
/// Org used when none is configured (matches the edge's dev-mode `user@org` bearers).
pub const DEFAULT_ORG_ID: &str = "dev-org";
/// User used when none is configured (dev mode without a bearer).
pub const DEFAULT_USER_ID: &str = "dev-user";
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
    /// When present, join the account's WorkspaceHub. None uses native local
    /// SQLite authority; no cloud pending operations are created.
    pub edge: Option<EdgeConfig>,
    /// Fresh sign-in this process: revive a tombstoned device row instead of
    /// treating the tombstone as an eviction. Consumed once at first reconcile.
    pub allow_device_rejoin: bool,
}

struct WorkspaceHostInner {
    v3: Arc<v3::State>,
    config: WorkspaceHostConfig,
    reg: Arc<Mutex<MetadataView>>,
    chats_tx: watch::Sender<Vec<Chat>>,
    devices_tx: watch::Sender<Vec<Device>>,
    sessions_tx: watch::Sender<Vec<Session>>,
    spaces_tx: watch::Sender<Vec<Space>>,
    /// Bumped after a committed mutation or a replaceable local observation.
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
    /// Called with a device id whenever its presence heartbeat proves it alive —
    /// wired to `LinkCache::reset_cooldown` so a peer that comes back is dialed
    /// immediately instead of waiting out the failure backoff.
    peer_alive: Mutex<Option<PeerAliveHook>>,
    notification_event: Mutex<Option<NotificationEventHook>>,
}

/// "This peer is alive" callback (device id) — see `WorkspaceHost::set_peer_alive_hook`.
pub type PeerAliveHook = Arc<dyn Fn(&str) + Send + Sync>;
pub type NotificationEventHook = Arc<dyn Fn(&Session) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct WorkspaceHost {
    inner: Arc<WorkspaceHostInner>,
}

impl WorkspaceHost {
    pub(crate) fn read_local_profile(
        data_dir: &std::path::Path,
        actor: &str,
    ) -> Result<Option<MetadataView>, EngineError> {
        let profile = crate::EngineProfile::local(data_dir)?;
        v3::read_profile(
            profile.store_root(),
            cypher_sync::workspace3::wire::Scope {
                endpoint: "local".into(),
                org: profile.org_id().into(),
                user: profile.user_id().into(),
                actor: actor.into(),
            },
        )
    }
    /// Open only native SQLite, then join the captured account if configured.
    pub fn open(store: Arc<DocsStore>, config: WorkspaceHostConfig) -> Result<Self, EngineError> {
        v3::open(store, config)
    }

    /// Close the current registry membership before account-scoped state is
    /// drained. The auth signal prevents an in-flight join from replacing it.
    pub fn disconnect_edge(&self) {
        self.inner.v3.disconnect();
    }

    /// Wire the "peer is alive" signal (fresh presence heartbeat) to a callback —
    /// the engine points this at `LinkCache::reset_cooldown`.
    pub fn set_peer_alive_hook(&self, hook: PeerAliveHook) {
        *lock(&self.inner.peer_alive) = Some(hook);
    }
    pub(crate) fn set_demand_hook(&self, hook: Arc<dyn Fn(Vec<String>) + Send + Sync>) {
        self.inner.v3.set_demand(hook);
    }
    pub fn attach_rpc_host(&self, service: std::sync::Weak<dyn cypher_rpc::RpcService>) {
        self.inner.v3.attach_rpc(service);
    }
    pub fn rpc_caller(&self) -> Option<Arc<cypher_rpc::workspace3::Caller>> {
        self.inner.v3.rpc_caller()
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
        self.inner.v3.sync_status().is_some_and(|s| s.connected)
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
        self.inner.v3.probe();
    }

    /// Registry room introspection for SyncStatus / `cypher sync`.
    /// `None` = no room yet (edge-less, or the initial join is still retrying).
    pub fn sync_status(&self) -> Option<cypher_sync::RoomStatsSnapshot> {
        self.inner.v3.sync_status()
    }

    // ── registry access helpers ─────────────────────────────────────────────

    /// Run a mutation under the registry lock, then wake the publish/persist
    /// task and push the write to the room.
    fn mutate<R>(
        &self,
        f: impl FnOnce(&mut MetadataView) -> Result<R, MetadataError>,
    ) -> Result<R, MetadataError> {
        v3::mutate(&self.inner, f)
    }

    fn read<R>(
        &self,
        f: impl FnOnce(&MetadataView) -> Result<R, MetadataError>,
    ) -> Result<R, MetadataError> {
        if self.inner.v3.retired() {
            return Err(MetadataError("runtime_retired".into()));
        }
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
        if self.inner.v3.retired() {
            return Err(EngineError::Other("runtime_retired".into()));
        }
        Ok(self.inner.v3.sessions(&self.inner))
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
                false
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
        self.mutate(|doc| {
            doc.claim_chat(chat_id, cwd, space_id.as_deref(), Utc::now());
            Ok(())
        })?;
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

    /// Replaceable session observation. Never a durable metadata operation.
    pub fn record_session(&self, session: &Session) {
        if !self.inner.v3.retired() {
            self.inner.v3.record(session);
            self.inner.bump_changed();
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

    /// Compatibility with callers' flush boundary: native writes already commit
    /// synchronously. There is no delayed snapshot to flush.
    pub fn flush(&self) {
        // Each native mutation has already committed to durable SQLite.
    }

    /// Withdraw this connection. Presence loss does not transfer execution.
    pub fn shutdown(&self) {
        self.inner.v3.disconnect();
    }
    pub async fn shutdown_workers(&self) {
        self.inner.v3.shutdown().await;
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
            self.mark_evicted();
            self.bump_changed();
            return;
        }
        if self.announced.swap(true, Ordering::Relaxed) && !unpaired {
            return;
        }
        if let Err(err) = v3::mutate(self, |draft| {
            announce_device(draft, &self.config).map_err(|e| MetadataError(e.to_string()))
        }) {
            tracing::warn!(error = %err, "device announce failed");
            self.announced.store(false, Ordering::Relaxed);
            return;
        }
        self.bump_changed();
    }

    fn publish(&self) {
        let state = lock(&self.reg).read_all();
        match state {
            Ok(mut state) => {
                state.sessions = self.v3.sessions(self);
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
        self.v3.overlay_presence(self, devices);
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

/// One boot announcement in the same durable transaction as its HLC/outbox.
fn announce_device(
    doc: &mut MetadataView,
    config: &WorkspaceHostConfig,
) -> Result<(), EngineError> {
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
    use cypher_proto::{Device, Session, SessionStatus};
    use cypher_sync::DocsStore;

    use super::{
        WorkspaceHost, WorkspaceHostConfig, device_name_on_boot, linked_worktree_root, lock,
        merge_sessions,
    };

    fn session(chat_id: &str, device_id: &str, status: SessionStatus) -> Session {
        Session {
            chat_id: chat_id.into(),
            device_id: device_id.into(),
            status,
            started_at: None,
            updated_at: Utc::now(),
            subagents: Vec::new(),
        }
    }

    #[test]
    fn merged_sessions_restore_local_durable_rows_after_restart() {
        let durable = vec![
            session("finished-child", "local-device", SessionStatus::Idle),
            session("remote-chat", "remote-device", SessionStatus::Working),
        ];

        let merged = merge_sessions("local-device", &durable, &[]);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].chat_id, "finished-child");
        assert_eq!(merged[0].status, SessionStatus::Idle);
        assert_eq!(merged[1].chat_id, "remote-chat");
    }

    #[test]
    fn merged_sessions_prefer_own_live_status_over_durable_status() {
        let durable = vec![session("local-chat", "local-device", SessionStatus::Idle)];
        let live = vec![session(
            "local-chat",
            "local-device",
            SessionStatus::Working,
        )];

        let merged = merge_sessions("local-device", &durable, &live);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].status, SessionStatus::Working);
    }

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
        host.shutdown_workers().await;
        drop(host);
        let reopened = open_host(dir.path(), "local-device", false);
        assert!(
            reopened
                .read_devices()
                .unwrap()
                .iter()
                .any(|d| d.id == "local-device"),
            "authorized rejoin must be durable, not only an optimistic cache write"
        );
        reopened.shutdown_workers().await;
    }

    #[tokio::test]
    async fn malformed_chat_and_retired_runtime_never_grant_host_authority() {
        let dir = tempfile::tempdir().unwrap();
        let host = open_host(dir.path(), "local-device", false);
        lock(&host.inner.reg).replace_rows([cypher_proto::metadata::MetadataRow {
            kind: "chats".into(),
            id: "bad".into(),
            seq: 1,
            deleted: false,
            del_hlc: None,
            fields: std::collections::BTreeMap::from([
                ("id".into(), serde_json::json!("bad")),
                ("deviceId".into(), serde_json::json!(42)),
            ]),
            clocks: Default::default(),
        }]);
        assert!(!host.is_host("bad"));
        assert!(host.is_host("not-yet-indexed"));
        host.shutdown_workers().await;
        assert!(!host.is_host("not-yet-indexed"));
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

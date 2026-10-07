//! App state: the engine connection, entity lists, and the selected chat's
//! transcript — one gpui [`Entity`] the whole shell renders from.
//!
//! ## EngineHandle
//! The UI talks the same typed RPC whether the engine is in-process or a separate
//! daemon (ARCHITECTURE §1). [`EngineHandle::bootstrap`] probes the localhost IPC
//! port, mirroring zeron: if an engine is listening it connects over WebSocket
//! ([`RemoteEngine`]); otherwise it embeds one via [`EngineCore::assemble`] and an
//! in-memory RPC transport ([`InProcessEngine`]) — same envelopes, same dispatch.
//!
//! ## Async bridging
//! `bootstrap` runs on tokio via `gpui_tokio::Tokio::spawn`. Once an [`RpcClient`]
//! exists, its `call`/`subscribe` futures are runtime-agnostic (tokio channels),
//! so subscription pumps run on gpui's own executor via `cx.spawn` and fold each
//! frame into the entity with `this.update(...)` + `cx.notify()`.
//!
//! Pure logic (sort order, staleness, gate phase) lives in free functions with
//! unit tests; rendering reads them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gpui::{App, AppContext, Context, Entity, Subscription, Task, WeakEntity};
use gpui_tokio::Tokio;
use serde::de::DeserializeOwned;

use cypher_doc::{
    SessionCommandEntry, SessionCommandPayload, SessionCommandStatus, SessionMessageEntry,
    TranscriptDesync, TranscriptFrame,
};
use cypher_engine::{Engine, EngineConfig, EngineRuntime, InstanceLock, rpc::AuthRpc};
use cypher_proto::{
    AuthState, Chat, ChatIndicator, Device, EngineInfo, HarnessId, Session, SideChatStatus, Space,
    WorkspaceScope,
};
use cypher_rpc::{RpcClient, RpcError, RpcReply, RpcService, memory_client, methods};

use crate::settings::SidebarSort;

// ---------------------------------------------------------------------------
// Engine handle
// ---------------------------------------------------------------------------

/// Everything needed to reach (or start) an engine.
#[derive(Debug, Clone)]
pub struct EngineBootConfig {
    /// Data directory for the embedded engine (`~/.cypher`).
    pub data_dir: PathBuf,
    /// Private Unix IPC socket to probe / serve.
    pub ipc_socket: PathBuf,
    /// Edge base URL for the embedded engine.
    pub edge_url: String,
    /// Bearer for edge room joins; `None` runs offline.
    pub edge_token: Option<String>,
    /// Workspace org override for explicit dev-mode runs.
    pub org_id: Option<String>,
    /// WorkOS client id for production authentication.
    pub workos_client_id: Option<String>,
    /// Harness for doc-command runs until per-chat config lands (M4).
    pub default_harness: HarnessId,
}

/// How this UI reached its engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineMode {
    /// Engine embedded in this process (in-memory RPC transport).
    InProcess,
    /// Connected to a separate daemon over localhost WebSocket.
    Remote { url: String },
}

/// One of the two ways to own an engine connection. Both end at an [`RpcClient`]
/// speaking the identical protocol — the trait only differs in provenance and
/// teardown.
#[async_trait]
trait EngineBackend: Send + Sync {
    fn client(&self) -> &RpcClient;
    fn mode(&self) -> EngineMode;
    /// Graceful teardown (drains runs / flushes docs for the in-process engine).
    async fn shutdown(&self);
}

/// Embedded engine: owns the [`EngineCore`] and an in-memory RPC loop.
struct InProcessEngine {
    runtime: Arc<tokio::sync::Mutex<Option<EngineRuntime>>>,
    boot_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    refresh_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Listener task, taken and joined during shutdown before releasing ownership.
    ipc_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    client: RpcClient,
}

#[async_trait]
impl EngineBackend for InProcessEngine {
    fn client(&self) -> &RpcClient {
        &self.client
    }
    fn mode(&self) -> EngineMode {
        EngineMode::InProcess
    }
    async fn shutdown(&self) {
        async fn stop(slot: &tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>) {
            if let Some(task) = slot.lock().await.take() {
                task.abort();
                let _ = task.await;
            }
        }
        stop(&self.boot_task).await;
        // Stop accepting first: a viewport must not connect midway through the
        // drain and queue work against stores that are closing.
        stop(&self.ipc_task).await;
        stop(&self.refresh_task).await;
        if let Some(runtime) = self.runtime.lock().await.take() {
            runtime.shutdown().await;
        }
    }
}

#[derive(Clone)]
enum DeferredEngineState {
    Waiting,
    Ready,
    Failed(String),
}

/// Serves engine identity and AuthRpc immediately, then holds data calls only
/// while a captured synced profile still needs organization onboarding.
/// Existing subscriptions attach to the assembled service without reconnecting.
struct DeferredEngineRpc {
    auth: AuthRpc,
    engine_info: EngineInfo,
    state: tokio::sync::watch::Receiver<DeferredEngineState>,
    service: Arc<tokio::sync::OnceCell<Arc<dyn RpcService>>>,
}

#[async_trait]
impl RpcService for DeferredEngineRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method == methods::ENGINE_INFO {
            return RpcReply::value(&self.engine_info);
        }
        if method == methods::ENGINE_READY {
            let mut state = self.state.clone();
            return match wait_for_deferred_engine(&mut state).await {
                Ok(()) => RpcReply::value(&serde_json::json!({ "ready": true })),
                Err(message) => Err(RpcError::Failed(message)),
            };
        }
        if AuthRpc::handles(method) {
            return self.auth.handle(method, params).await;
        }

        let mut state = self.state.clone();
        loop {
            let current = { state.borrow().clone() };
            match current {
                DeferredEngineState::Waiting => {}
                DeferredEngineState::Ready => {
                    let service = self.service.get().ok_or_else(|| {
                        RpcError::Failed(
                            "embedded engine became ready without an RPC service".into(),
                        )
                    })?;
                    return service.handle(method, params).await;
                }
                DeferredEngineState::Failed(message) => return Err(RpcError::Failed(message)),
            }
            state.changed().await.map_err(|_| RpcError::Closed)?;
        }
    }
}

async fn wait_for_deferred_engine(
    state: &mut tokio::sync::watch::Receiver<DeferredEngineState>,
) -> Result<(), String> {
    loop {
        let current = { state.borrow().clone() };
        match current {
            DeferredEngineState::Waiting => {}
            DeferredEngineState::Ready => return Ok(()),
            DeferredEngineState::Failed(message) => return Err(message),
        }
        state
            .changed()
            .await
            .map_err(|_| "embedded engine assembly ended without a result".to_string())?;
    }
}

/// External same-user daemon over a private Unix socket.
struct RemoteEngine {
    client: Arc<RpcClient>,
    url: String,
    lifecycle_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[async_trait]
impl EngineBackend for RemoteEngine {
    fn client(&self) -> &RpcClient {
        &self.client
    }
    fn mode(&self) -> EngineMode {
        EngineMode::Remote {
            url: self.url.clone(),
        }
    }
    async fn shutdown(&self) {
        // The daemon outlives this viewport; only stop our readiness probe.
        if let Some(task) = self.lifecycle_task.lock().await.take() {
            task.abort();
        }
    }
}

/// Cheaply clonable handle to whichever backend won the probe.
#[derive(Clone)]
pub struct EngineHandle {
    inner: Arc<dyn EngineBackend>,
    engine_info: EngineInfo,
    deferred_state: Option<tokio::sync::watch::Receiver<DeferredEngineState>>,
}

impl EngineHandle {
    /// Probe the IPC socket and connect (daemon listening) or embed (nothing there).
    /// Must run on the tokio runtime (`Tokio::spawn`): both transports spawn
    /// tokio tasks.
    pub async fn bootstrap(config: EngineBootConfig) -> anyhow::Result<EngineHandle> {
        anyhow::ensure!(
            config.ipc_socket == cypher_env::ipc_socket(&config.data_dir)?,
            "Engine IPC socket does not match the selected data directory"
        );
        // Invariant: at most one bootstrap in this process runs probe+embed at
        // a time. The winner binds the deferred IPC listener before releasing
        // the gate, so a concurrent viewport's probe finds it and attaches as
        // Remote instead of racing it for the data dir.
        static BOOTSTRAP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _gate = BOOTSTRAP_GATE.lock().await;

        if let Some(handle) = Self::attach_to_daemon(&config.ipc_socket, &config.data_dir).await? {
            return Ok(handle);
        }

        tracing::info!(data_dir = %config.data_dir.display(), "no daemon on Unix socket; embedding engine");
        let engine_config = EngineConfig {
            data_dir: config.data_dir,
            edge_url: config.edge_url,
            edge_token: config.edge_token,
            ipc_socket: config.ipc_socket,
            default_harness: config.default_harness,
            org_id: config.org_id,
            workos_client_id: config.workos_client_id,
        };

        // Own the data dir before opening anything under it or binding IPC —
        // the lock, not the socket bind, is the ownership decision. A failed
        // acquire means an out-of-process engine holds the dir but was not
        // serving IPC at probe time (a daemon mid-start): wait for its
        // listener, re-trying the lock in case it dies instead.
        std::fs::create_dir_all(&engine_config.data_dir)?;
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let lock = loop {
            match InstanceLock::acquire(&engine_config.data_dir) {
                Ok(lock) => break lock,
                Err(err) => {
                    if std::time::Instant::now() >= lock_deadline {
                        return Err(err.into());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    if let Some(handle) =
                        Self::attach_to_daemon(&engine_config.ipc_socket, &engine_config.data_dir)
                            .await?
                    {
                        return Ok(handle);
                    }
                }
            }
        };

        let auth = Engine::build_auth(&engine_config).await;
        let workspace_scope = Engine::initial_workspace_scope(&auth);
        let initial_profile = Engine::resolve_profile(&engine_config, &auth, workspace_scope)?;
        let profile_is_resolved = initial_profile.is_some();
        let engine_info = Engine::engine_info(&engine_config, workspace_scope)?;
        let refresh_task = auth.spawn_refresh_loop();
        let (state_tx, mut state_rx) = tokio::sync::watch::channel(DeferredEngineState::Waiting);
        let assembled_service = Arc::new(tokio::sync::OnceCell::new());
        let service: Arc<dyn RpcService> = Arc::new(DeferredEngineRpc {
            auth: AuthRpc::new(auth.clone()),
            engine_info: engine_info.clone(),
            state: state_rx.clone(),
            service: assembled_service.clone(),
        });
        let client = memory_client(service.clone());

        // Serve the same service on the IPC socket so a terminal viewport can
        // attach to this window's engine with no setup. Deliberately the
        // *deferred* service, not the assembled one: a viewport that connects
        // during cloud onboarding gets EngineInfo and AuthRpc immediately, and
        // its data subscriptions wait exactly as this window's do.
        //
        // Fail closed: an embedded engine must own its private IPC endpoint.
        let ipc_task = Some(cypher_engine::serve_ipc(&engine_config.ipc_socket, service).await?);
        let runtime = Arc::new(tokio::sync::Mutex::new(None));
        let runtime_for_boot = runtime.clone();
        let service_for_boot = assembled_service.clone();
        // The instance lock rides into the boot task and is consumed by
        // assembly — held through sign-in onboarding too, because this process
        // owns the data dir from the moment it decided to embed.
        let boot_task = tokio::spawn(async move {
            let profile = match initial_profile {
                Some(profile) => profile,
                None => {
                    let mut auth_state = auth.watch_state();
                    while !auth_state.borrow().is_signed_in() {
                        if auth_state.changed().await.is_err() {
                            state_tx.send_replace(DeferredEngineState::Failed(
                                "authentication state closed before workspace onboarding".into(),
                            ));
                            return;
                        }
                    }
                    match Engine::resolve_profile(&engine_config, &auth, workspace_scope) {
                        Ok(Some(profile)) => profile,
                        Ok(None) => {
                            state_tx.send_replace(DeferredEngineState::Failed(
                                "workspace onboarding completed without an organization".into(),
                            ));
                            return;
                        }
                        Err(err) => {
                            state_tx.send_replace(DeferredEngineState::Failed(err.to_string()));
                            return;
                        }
                    }
                }
            };

            match Engine::assemble_runtime_with_lock(&engine_config, auth, profile, lock).await {
                Ok(engine_runtime) => {
                    let service: Arc<dyn RpcService> = engine_runtime.core().rpc_service();
                    *runtime_for_boot.lock().await = Some(engine_runtime);
                    if service_for_boot.set(service).is_err() {
                        state_tx.send_replace(DeferredEngineState::Failed(
                            "embedded engine RPC service was assembled more than once".into(),
                        ));
                        return;
                    }
                    state_tx.send_replace(DeferredEngineState::Ready);
                }
                Err(err) => {
                    tracing::error!(error = %err, "embedded engine assembly failed");
                    state_tx.send_replace(DeferredEngineState::Failed(format!("{err:#}")));
                }
            }
        });
        let handle = EngineHandle {
            inner: Arc::new(InProcessEngine {
                runtime,
                boot_task: tokio::sync::Mutex::new(Some(boot_task)),
                refresh_task: tokio::sync::Mutex::new(Some(refresh_task)),
                ipc_task: tokio::sync::Mutex::new(ipc_task),
                client,
            }),
            engine_info,
            deferred_state: Some(state_rx.clone()),
        };
        // Local, development, and already-resolved synced profiles need no
        // authentication UI while assembling. Keep the viewport Connecting
        // until their stores and journals are actually open, and surface a
        // boot failure through the existing bootstrap error path.
        if profile_is_resolved && let Err(message) = wait_for_deferred_engine(&mut state_rx).await {
            handle.shutdown().await;
            return Err(anyhow::anyhow!(message));
        }
        Ok(handle)
    }

    /// Probe the IPC socket and, if a live engine answers, attach as a remote
    /// viewport. `None` means embed: nothing listening, a non-engine listener,
    /// or a listener without an identity.
    async fn attach_to_daemon(
        ipc_socket: &std::path::Path,
        data_dir: &std::path::Path,
    ) -> anyhow::Result<Option<EngineHandle>> {
        let url = format!("unix:{}", ipc_socket.display());
        if !cypher_rpc::probe_local(ipc_socket).await? {
            return Ok(None);
        }
        tracing::info!(%url, "engine daemon detected; connecting");
        match cypher_rpc::connect_local(ipc_socket).await {
            Ok(client) => match query_engine_info(&client).await {
                Ok(engine_info) => {
                    let expected = std::fs::read_to_string(data_dir.join("device-id"))?;
                    anyhow::ensure!(
                        !expected.trim().is_empty() && expected.trim() == engine_info.device_id,
                        "IPC engine identity does not match the selected data directory"
                    );
                    let client = Arc::new(client);
                    let (state_tx, state_rx) =
                        tokio::sync::watch::channel(DeferredEngineState::Waiting);
                    let lifecycle_client = client.clone();
                    let lifecycle_task = tokio::spawn(async move {
                        let state = match lifecycle_client
                            .call(methods::ENGINE_READY, serde_json::json!({}))
                            .await
                        {
                            Ok(_) => DeferredEngineState::Ready,
                            Err(err) => DeferredEngineState::Failed(err.to_string()),
                        };
                        state_tx.send_replace(state);
                    });
                    Ok(Some(EngineHandle {
                        inner: Arc::new(RemoteEngine {
                            client,
                            url,
                            lifecycle_task: tokio::sync::Mutex::new(Some(lifecycle_task)),
                        }),
                        engine_info,
                        deferred_state: Some(state_rx),
                    }))
                }
                Err(err) => {
                    tracing::warn!(
                        %url,
                        error = %err,
                        "listener did not provide engine identity; refusing to attach"
                    );
                    Err(anyhow::anyhow!("IPC engine identity unavailable: {err}"))
                }
            },
            // An unresponsive endpoint is not an empty slot. Do not embed.
            Err(err) => {
                tracing::warn!(%url, error = %err, "IPC handshake failed; refusing to attach");
                Err(anyhow::anyhow!("IPC handshake failed: {err}"))
            }
        }
    }

    pub fn client(&self) -> &RpcClient {
        self.inner.client()
    }

    pub fn mode(&self) -> EngineMode {
        self.inner.mode()
    }

    pub fn engine_info(&self) -> &EngineInfo {
        &self.engine_info
    }

    fn deferred_state(&self) -> Option<tokio::sync::watch::Receiver<DeferredEngineState>> {
        self.deferred_state.clone()
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

/// Query the current protocol first, with a conservative fallback for daemons
/// from before `EngineInfo` existed. Old daemons are always treated as synced.
async fn query_engine_info(client: &RpcClient) -> Result<EngineInfo, RpcError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.call_as(methods::ENGINE_INFO, serde_json::json!({})),
    )
    .await
    .map_err(|_| RpcError::Transport("Engine identity check timed out".into()))?
}

// ---------------------------------------------------------------------------
// Pure state + reducers
// ---------------------------------------------------------------------------

// The frontend-agnostic derivations (sort orders, staleness gating, sidebar
// grouping, the boot gate, relative times) live in `cypher_proto::view`, pure
// and with their own test suite. Re-exported here because every call site in
// this crate reads them as `state::…`.
pub use cypher_proto::view::{
    ConnectionStatus, GatePhase, Indicator, chat_location, display_status, effective_indicator,
    format_time_ago, gate_phase, parse_auth_state, sort_active, sort_chats, sort_spaces, sort_tabs,
};

// ---------------------------------------------------------------------------
// Org gate (pure)
// ---------------------------------------------------------------------------

/// One org membership row (tolerant local mirror of the engine's ListOrgs
/// reply — `{orgs: [{id, organizationId, name}]}`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRow {
    pub organization_id: String,
    pub name: String,
}

/// Parse a ListOrgs reply tolerantly (accepts a bare array too).
pub fn parse_orgs(value: &serde_json::Value) -> Vec<OrgRow> {
    let list = value.get("orgs").unwrap_or(value);
    serde_json::from_value(list.clone()).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrgSetup {
    AutoCreate,
    AutoSelect(String),
    Pick(Vec<OrgRow>),
}

/// Decide organization setup after normalizing the membership list.
pub fn org_setup(rows: Vec<OrgRow>) -> OrgSetup {
    let rows = sort_memberships(rows);
    match rows.as_slice() {
        [] => OrgSetup::AutoCreate,
        [only] => OrgSetup::AutoSelect(only.organization_id.clone()),
        _ => OrgSetup::Pick(rows),
    }
}

/// Memberships sorted by name (case-insensitive), deduped by organization id.
pub fn sort_memberships(mut orgs: Vec<OrgRow>) -> Vec<OrgRow> {
    orgs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    orgs.dedup_by(|a, b| a.organization_id == b.organization_id);
    orgs
}

// ---------------------------------------------------------------------------
// AppState entity
// ---------------------------------------------------------------------------

/// A composer send whose doc command is queued but not yet executed by the
/// chat's host device — cleared when the host writes the user message back
/// into the transcript (same client-minted id as the [`AppState::echoes`]
/// dedup), or after [`PENDING_SEND_TTL_MS`].
#[derive(Debug, Clone)]
struct PendingSend {
    message_id: String,
    started: DateTime<Utc>,
}

struct UploadProgress {
    /// The chat whose send owns this upload. The trailer is scoped to it so a
    /// background upload never narrates itself under someone else's
    /// conversation.
    chat_id: String,
    total_bytes: u64,
    completed_bytes: Arc<std::sync::atomic::AtomicU64>,
}

/// How long the send-in-flight overlay may hold before the synced status
/// shows through again. Covers the queue → nudge → drain → sync round-trip
/// to a remote host; when the host is offline the dot falls back to the
/// truth after this.
pub const PENDING_SEND_TTL_MS: i64 = 30_000;

/// Projected status of one queued message command, mapped from the durable
/// ledger by `message_id` (Run/Steer commands only). This is the source of
/// truth over the local optimistic overlay: the composer's send-in-flight
/// state guesses, the doc ledger knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSendStatus {
    /// A live pending attempt exists and no earlier attempt failed.
    Queued,
    /// A live pending attempt exists AFTER an earlier failure — a retry is
    /// in flight.
    Retrying,
    /// The latest attempt failed (Rejected/Expired) and awaits a retry.
    Failed,
}

/// The logical message id a Run/Steer command carries (the same client-minted
/// id as the optimistic echo — the dedup key across attempts).
pub fn command_message_id(payload: &SessionCommandPayload) -> Option<&str> {
    match payload {
        SessionCommandPayload::Run { message_id, .. } => Some(message_id),
        SessionCommandPayload::Steer {
            message_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    }
}

/// Map a message id to its projected send status from the durable ledger.
/// `None` = no Run/Steer command (or the message already resolved).
pub fn command_send_status(
    commands: &[SessionCommandEntry],
    message_id: &str,
) -> Option<CommandSendStatus> {
    let attempts: Vec<&SessionCommandEntry> = commands
        .iter()
        .filter(|c| command_message_id(&c.payload) == Some(message_id))
        .collect();
    if attempts.is_empty() {
        return None;
    }
    let has_live = attempts
        .iter()
        .any(|c| c.status == SessionCommandStatus::Pending);
    let has_failed = attempts.iter().any(|c| {
        matches!(
            c.status,
            SessionCommandStatus::Rejected | SessionCommandStatus::Expired
        )
    });
    if has_live {
        if has_failed {
            Some(CommandSendStatus::Retrying)
        } else {
            Some(CommandSendStatus::Queued)
        }
    } else if has_failed {
        Some(CommandSendStatus::Failed)
    } else {
        None
    }
}

/// A failed (Rejected/Expired) message command the user can retry — the
/// composer's failed row. One per message id, always the LATEST attempt.
#[derive(Debug, Clone)]
pub struct FailedCommand {
    pub command_id: String,
    pub prompt: String,
    pub resolution: Option<String>,
}

/// The retry-able failures in the ledger, in doc order, skipping messages
/// with a live pending attempt (their retry is already in flight).
pub fn failed_commands(commands: &[SessionCommandEntry]) -> Vec<FailedCommand> {
    let mut out: Vec<FailedCommand> = Vec::new();
    for command in commands {
        let Some(message_id) = command_message_id(&command.payload) else {
            continue;
        };
        // A live attempt supersedes the failed row: Retrying is in flight.
        if command_send_status(commands, message_id) != Some(CommandSendStatus::Failed) {
            continue;
        }
        // Latest failed attempt only: a retry that failed again keeps the
        // newest command id as the retry target.
        let is_latest = commands.iter().any(|other| {
            other.id != command.id
                && command_message_id(&other.payload) == Some(message_id)
                && other.issued_at > command.issued_at
        });
        if is_latest {
            continue;
        }
        let prompt = match &command.payload {
            SessionCommandPayload::Run { request, .. } => request.prompt.clone(),
            SessionCommandPayload::Steer { prompt, .. } => prompt.clone(),
            _ => continue,
        };
        out.push(FailedCommand {
            command_id: command.id.clone(),
            prompt,
            resolution: command.resolution.clone(),
        });
    }
    out
}

/// Root application state. Reducer methods (`apply_*`, [`Self::session_for`], …)
/// are plain `&mut self` functions so tests construct the struct directly; gpui
/// glue ([`Self::bootstrap`], [`Self::select_chat`]) layers subscriptions on top.
pub struct AppState {
    pub connection: ConnectionStatus,
    /// Fixed data boundary of the attached engine. Authentication may change
    /// in place, but changing this scope requires assembling a new runtime.
    pub workspace_scope: Option<WorkspaceScope>,
    /// Auth stream value; `None` until the engine reports one (M4).
    pub auth: Option<AuthState>,
    pub devices: Vec<Device>,
    /// Sorted (see [`sort_spaces`]).
    pub spaces: Vec<Space>,
    /// Sorted (see [`sort_chats`]); includes archived rows — views filter.
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
    /// The project the new-session canvas mints into. Healed by
    /// [`Self::apply_spaces`] when the row vanishes; selecting a chat implies
    /// its project.
    pub selected_space: Option<String>,
    /// Deliberate "Don't work in a project" pick: while set, the canvas mints
    /// project-less sessions (cwd `~` on the picked device) and
    /// [`Self::selected_space_row`] reads as `None` — healing must NOT
    /// re-select a project underneath it.
    pub no_project: bool,
    /// The canvas is a QUICK CHAT: the next send asks the picked device for a
    /// throwaway scratch folder and runs there (implies `no_project`).
    /// Cleared by any project pick, chat selection, or the ordinary new
    /// session.
    pub scratch_pending: bool,
    /// The composer's device pick — where project-less sessions run, and the
    /// device whose projects the project picker lists. `None` falls back to
    /// the local device.
    pub selected_device: Option<String>,
    pub selected_chat: Option<String>,
    /// Boot auto-select happened (or a manual selection superseded it).
    pub auto_selected: bool,
    /// First chats / spaces watch frame has landed — device-local state that
    /// prunes against the doc (open tabs) must not judge by the empty
    /// pre-sync lists.
    pub chats_synced: bool,
    pub spaces_synced: bool,
    /// Bumped per applied chats frame. A session context mirrors it and
    /// heals a vanished selection only when it advances — like
    /// [`Self::apply_chats`], never on an unrelated notify (a canvas tile
    /// selects its minted chat before the row's frame lands).
    chats_generation: u64,
    /// Joined transcript of the selected chat (continuations folded engine-side).
    pub transcript: Vec<SessionMessageEntry>,
    /// Durable command ledger of the selected chat (WatchDocCommands): the
    /// source of truth the UI projects Queued/Failed/Retrying from.
    commands: Vec<SessionCommandEntry>,
    /// Optimistic user echoes per chat id, shown until the doc frame carrying
    /// the same message id arrives (client-minted ids make dedup exact).
    echoes: HashMap<String, Vec<SessionMessageEntry>>,
    /// Message ids this device sent as a Steer. Labels the optimistic echo
    /// before the ledger frame carrying its Steer command arrives.
    local_steers: HashSet<String>,
    /// Send-in-flight overlay per chat id: a queued doc command the host
    /// hasn't executed yet (see [`Self::begin_pending_send`]). Shared between
    /// a main state and its session contexts ([`Self::new_session_context`])
    /// so a send from any tile drives the main sidebar's dot and chime gate;
    /// a project window keeps its own copy.
    pending_sends: Rc<RefCell<HashMap<String, PendingSend>>>,
    upload_progress: Option<UploadProgress>,
    /// This engine's device id (best-effort `LocalDevice` probe; `None` until
    /// the engine serves it — views degrade gracefully).
    pub local_device_id: Option<String>,
    /// Latest `UpdateStatus` frame — drives the sidebar update strip.
    pub update: Option<cypher_update::UpdateStatus>,
    /// Latest Pi CLI + extension update facts — drives the one-click package
    /// update notification beside the Cypher release strip.
    pub pi_update: Option<cypher_engine::pi_packages::PiUpdateStatus>,
    /// Data directory (`ui-settings.json`, `composer-defaults.json`); set at
    /// bootstrap so child views can persist small preference files.
    pub data_dir: Option<PathBuf>,
    engine: Option<EngineHandle>,
    watch_tasks: Vec<Task<()>>,
    transcript_task: Option<Task<()>>,
    commands_task: Option<Task<()>>,
    /// Which projects this state's window lists (see [`ProjectScope`]).
    scope: ProjectScope,
    /// Whether `select_chat` subscribes the selected chat's transcript and
    /// command ledger. Off on a main state whose session tiles own the
    /// transcripts ("lists-only": the selection only drives the sidebar).
    transcript_watches: bool,
    /// Session context: the main state this one mirrors lists from and
    /// forwards seen / config writes to ([`Self::new_session_context`]).
    parent: Option<WeakEntity<AppState>>,
    /// Session context: the observation copying `parent`'s lists on each
    /// notify. Dropped with the context.
    mirror: Option<Subscription>,
    /// Bumped whenever what a transcript view renders may have changed
    /// (transcript, echoes, command ledger, steers, selection) — views gate
    /// their row rebuild on it ([`Self::transcript_revision`]).
    transcript_rev: u64,
}

/// Which projects a window lists. The main window lists every project except
/// the ones open in their own window; a project window lists only its
/// project (no project-less sessions). Scoping is a view concern: the lists
/// ([`AppState::visible_chats`], [`AppState::spaces_sorted`], the sidebar
/// groups) narrow, the synced rows underneath stay complete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectScope {
    /// The project a project window is dedicated to.
    pub only: Option<String>,
    /// Main window: projects currently open in their own windows.
    pub hidden: HashSet<String>,
}

impl ProjectScope {
    pub fn space_visible(&self, space_id: &str) -> bool {
        match self.only.as_deref() {
            Some(only) => only == space_id,
            None => !self.hidden.contains(space_id),
        }
    }

    pub fn chat_visible(&self, chat: &Chat) -> bool {
        match chat.space_id.as_deref() {
            Some(space_id) => self.space_visible(space_id),
            None => self.only.is_none(),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// The sidebar view menu's state: device filter + card sort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarView {
    pub device: Option<String>,
    pub sort: SidebarSort,
    /// Flip the sort's natural direction (see
    /// [`SidebarSort::natural_descending`]).
    pub reversed: bool,
}

impl SidebarView {
    /// Whether the view currently reads newest/Z first.
    pub fn descending(&self) -> bool {
        self.sort.natural_descending() != self.reversed
    }
}

/// What kind of sidebar card a group is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarGroupKind {
    /// A live `Space` — has a project context menu; empty spaces included.
    Space,
    /// Project-less (`space_id = None`) chats of one device.
    NoProject,
    /// Quick chats (project-less, scratch-folder cwd) of one device.
    Scratch,
    /// Chats whose `space_id` names a missing space.
    Unavailable,
}

/// One card of the project-grouped sidebar, produced by
/// [`AppState::sidebar_groups`]. Synthetic cards (No project / Unavailable
/// project) carry no `space_id` and therefore no project context menu.
#[derive(Debug)]
pub struct SidebarGroup<'a> {
    /// Stable key: `s:<space id>` live space, `np:<device id>` no-project,
    /// `sc` quick chats (one card across every device), `u:<missing space
    /// id>` unavailable. Status changes never re-key a group, so cards keep
    /// their identity across renders.
    pub key: String,
    pub kind: SidebarGroupKind,
    /// Card title: the project's display name, "No project", or
    /// "Unavailable project".
    pub title: String,
    /// Folder path for live spaces (muted truncated in the header); `None`
    /// for synthetic cards.
    pub path: Option<String>,
    /// Host device name. Empty on a Quick chats card merged from several
    /// devices (each session names its own host).
    pub device: String,
    /// Host device id (the sidebar device filter's key).
    pub device_id: String,
    /// Host offline (live spaces only; synthetic cards read as online).
    pub offline: bool,
    /// Creation instant for the Date sort: the space's `created_at`, or the
    /// newest chat's for synthetic cards.
    pub created_at: DateTime<Utc>,
    /// The space id for live-space cards (the project context menu target).
    pub space_id: Option<&'a str>,
    /// User pin on the project (live spaces only): pinned cards lead the list.
    pub pinned: bool,
    /// Sidebar glyph/colour keys (live spaces only; see `space_style`).
    pub icon: Option<String>,
    pub color: Option<String>,
    /// The card's chats in overview recency order, pinned sessions first
    /// (empty for quiet spaces).
    pub chats: Vec<(ChatIndicator, &'a Chat)>,
}

/// Fold every per-device Quick chats card into one `sc` card at the first
/// (newest) one's position: quick chats are throwaway, so one card per host
/// only repeated the same header. The merged sessions return to overview
/// order; a card drawn from several hosts drops its single device name.
fn merge_scratch_groups(groups: &mut Vec<SidebarGroup<'_>>) {
    let Some(first) = groups
        .iter()
        .position(|g| g.kind == SidebarGroupKind::Scratch)
    else {
        return;
    };
    let mut chats = Vec::new();
    let mut devices = HashSet::new();
    let mut ix = first + 1;
    while ix < groups.len() {
        if groups[ix].kind == SidebarGroupKind::Scratch {
            let group = groups.remove(ix);
            devices.insert(group.device_id);
            chats.extend(group.chats);
        } else {
            ix += 1;
        }
    }
    let card = &mut groups[first];
    card.key = "sc".into();
    if chats.is_empty() {
        return;
    }
    devices.remove(&card.device_id);
    if !devices.is_empty() {
        card.device.clear();
    }
    card.chats.extend(chats);
    sort_active(&mut card.chats);
    card.created_at = card
        .chats
        .iter()
        .map(|(_, c)| c.created_at)
        .max()
        .unwrap_or(card.created_at);
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connection: ConnectionStatus::Connecting,
            workspace_scope: None,
            auth: None,
            devices: Vec::new(),
            spaces: Vec::new(),
            chats: Vec::new(),
            sessions: Vec::new(),
            selected_space: None,
            no_project: false,
            scratch_pending: false,
            selected_device: None,
            selected_chat: None,
            transcript: Vec::new(),
            commands: Vec::new(),
            echoes: HashMap::new(),
            local_steers: HashSet::new(),
            pending_sends: Rc::default(),
            upload_progress: None,
            local_device_id: None,
            update: None,
            pi_update: None,
            data_dir: None,
            engine: None,
            watch_tasks: Vec::new(),
            transcript_task: None,
            commands_task: None,
            auto_selected: false,
            chats_synced: false,
            spaces_synced: false,
            chats_generation: 0,
            scope: ProjectScope::default(),
            transcript_watches: true,
            parent: None,
            mirror: None,
            transcript_rev: 0,
        }
    }

    /// The state behind a project window: a secondary [`AppState`] sharing
    /// `main`'s [`EngineHandle`], scoped to `space_id`, with its own
    /// selection and transcript watches. Seeded from `main`'s synced lists so
    /// the window's first frame is already populated; its own watches take
    /// over from there. Lands on `main`'s selected chat when that belongs to
    /// the project (the chat moves windows with it), else on the project's
    /// most recent session. `None` while `main` has no engine attached.
    pub fn new_project_window(
        main: &Entity<AppState>,
        space_id: &str,
        cx: &mut App,
    ) -> Option<Entity<AppState>> {
        let m = main.read(cx);
        let engine = m.engine.clone()?;
        let space = m.space_row(space_id)?.clone();
        let in_project = |chat_id: &String| {
            m.chats
                .iter()
                .any(|c| &c.id == chat_id && c.space_id.as_deref() == Some(space_id))
        };
        let landing = m.selected_chat.clone().filter(|id| in_project(id));
        let mut seed = AppState::new();
        seed.scope.only = Some(space_id.to_string());
        seed.auth = m.auth.clone();
        seed.devices = m.devices.clone();
        seed.spaces = m.spaces.clone();
        seed.chats = m.chats.clone();
        seed.sessions = m.sessions.clone();
        seed.chats_synced = m.chats_synced;
        seed.spaces_synced = m.spaces_synced;
        seed.update = m.update.clone();
        seed.pi_update = m.pi_update.clone();
        seed.data_dir = m.data_dir.clone();
        // In-flight sends ride along so the moved sessions keep their
        // optimistic echoes and Working dots until the host acks.
        seed.echoes = m
            .echoes
            .iter()
            .filter(|(id, _)| in_project(id))
            .map(|(id, echoes)| (id.clone(), echoes.clone()))
            .collect();
        // A copy, not the shared map: the window runs its own watches.
        seed.pending_sends = Rc::new(RefCell::new(
            m.pending_sends
                .borrow()
                .iter()
                .filter(|(id, _)| in_project(id))
                .map(|(id, send)| (id.clone(), send.clone()))
                .collect(),
        ));
        seed.local_steers = m.local_steers.clone();
        seed.selected_space = Some(space.id.clone());
        seed.selected_device = Some(space.device_id.clone());
        let state = cx.new(|_| seed);
        state.update(cx, |s, cx| {
            s.attach_engine(engine, false, cx);
            let landing = landing.or_else(|| {
                s.overview_chats(Utc::now())
                    .first()
                    .map(|(_, c)| c.id.clone())
            });
            if landing.is_some() {
                s.select_chat(landing, cx);
            }
        });
        Some(state)
    }

    /// A session context: a secondary [`AppState`] pinned to one session so
    /// several sessions render side by side, each through the unchanged
    /// `Transcript` / `Composer` / `Changes` / `FilesPanel` /
    /// `TerminalPanel` (which read `selected_chat`, `transcript`, …).
    /// `chat_id: None` is a new-session canvas tile: it starts from `main`'s
    /// project / device picks, and its first send selects the new chat here.
    ///
    /// It runs no list watches of its own: it MIRRORS `main` (engine,
    /// connection, auth, devices, spaces, chats, sessions, …) on every
    /// notify, and only subscribes its own selected chat's transcript and
    /// ledger. The send-in-flight overlay is shared with `main`; seen marks
    /// and optimistic config writes are forwarded to it
    /// ([`Self::mark_chat_seen`], [`Self::set_chat_config_optimistic`]).
    pub fn new_session_context(
        main: &Entity<AppState>,
        chat_id: Option<String>,
        cx: &mut App,
    ) -> Entity<AppState> {
        let m = main.read(cx);
        let mut seed = AppState::new();
        seed.parent = Some(main.downgrade());
        seed.pending_sends = m.pending_sends.clone();
        seed.engine = m.engine.clone();
        seed.mirror_lists(m);
        seed.selected_space = m.selected_space.clone();
        seed.selected_device = m.selected_device.clone();
        seed.no_project = m.no_project;
        seed.scratch_pending = chat_id.is_none() && m.scratch_pending;
        seed.auto_selected = true;
        if let Some(id) = chat_id.as_deref()
            && let Some(echoes) = m.echoes.get(id)
        {
            seed.echoes.insert(id.to_string(), echoes.clone());
        }
        seed.local_steers = m.local_steers.clone();
        let state = cx.new(|cx| {
            seed.mirror = Some(cx.observe(main, |this: &mut AppState, main, cx| {
                this.mirror_from_parent(&main, cx);
            }));
            seed
        });
        if chat_id.is_some() {
            state.update(cx, |s, cx| s.select_chat(chat_id, cx));
        }
        state
    }

    /// The main state a session context mirrors (`None` elsewhere, or once
    /// the parent is gone).
    pub fn parent(&self) -> Option<Entity<AppState>> {
        self.parent.as_ref()?.upgrade()
    }

    /// Copy the synced lists from `main` (session contexts), then heal a
    /// selection that vanished the same way the watches' reducers do.
    /// Returns whether anything changed (unchanged lists aren't re-cloned).
    fn mirror_lists(&mut self, main: &AppState) -> bool {
        let mut changed = false;
        macro_rules! mirror {
            ($($field:ident),* $(,)?) => {$(
                if self.$field != main.$field {
                    self.$field = main.$field.clone();
                    changed = true;
                }
            )*};
        }
        mirror!(
            connection,
            workspace_scope,
            auth,
            devices,
            spaces,
            chats,
            sessions,
            local_device_id,
            data_dir,
            update,
            pi_update,
            chats_synced,
            spaces_synced,
            scope,
        );
        // Only a chats frame judges the selection (see `chats_generation`);
        // pre-sync (or mid runtime replacement) lists are empty, not
        // authoritative.
        let chats_frame = self.chats_generation != main.chats_generation;
        self.chats_generation = main.chats_generation;
        if self.chats_synced && chats_frame && self.drop_vanished_chat() {
            changed = true;
        }
        if self.spaces_synced {
            let before = (
                self.selected_space.clone(),
                self.selected_device.clone(),
                self.no_project,
            );
            self.heal_space_selection();
            changed |= before
                != (
                    self.selected_space.clone(),
                    self.selected_device.clone(),
                    self.no_project,
                );
        }
        changed
    }

    /// The mirror observation: lists, plus the engine — a runtime
    /// replacement on `main` re-aims (or drops) this context's watches.
    /// Notifies only when something was mirrored: every main notify reaches
    /// every tile's context, and a no-op notify re-rendered them all.
    fn mirror_from_parent(&mut self, main: &Entity<AppState>, cx: &mut Context<Self>) {
        let (mut changed, engine) = {
            let main = main.read(cx);
            (self.mirror_lists(main), main.engine.clone())
        };
        let same_engine = match (&self.engine, &engine) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a.inner, &b.inner),
            (None, None) => true,
            _ => false,
        };
        if !same_engine {
            self.engine = engine;
            self.transcript.clear();
            self.commands.clear();
            self.transcript_task = None;
            self.commands_task = None;
            self.bump_transcript();
            self.spawn_transcript_watches(cx);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// A closed tab's context kept alive only by a parked terminal panel
    /// (its PTYs outlive the tab): stop mirroring `main` and drop the
    /// transcript watches — nothing renders from it until the panel is
    /// re-bound to a new tile's context.
    pub fn park_session_context(&mut self) {
        self.mirror = None;
        self.transcript_task = None;
        self.commands_task = None;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
    }

    /// See [`Self::chats_generation`]: advances once per applied chats
    /// frame (never on an optimistic insert).
    pub fn chats_generation(&self) -> u64 {
        self.chats_generation
    }

    /// See [`Self::transcript_rev`].
    pub fn transcript_revision(&self) -> u64 {
        self.transcript_rev
    }

    fn bump_transcript(&mut self) {
        self.transcript_rev = self.transcript_rev.wrapping_add(1);
    }

    /// Replace the selected chat's transcript wholesale (a promoted side
    /// chat's handoff seeds the new tile before its doc watch lands).
    pub fn set_transcript(&mut self, entries: Vec<SessionMessageEntry>) {
        self.transcript = entries;
        self.bump_transcript();
    }

    pub fn project_scope(&self) -> &ProjectScope {
        &self.scope
    }

    /// The project a project window is dedicated to (`None` in the main
    /// window).
    pub fn window_project(&self) -> Option<&str> {
        self.scope.only.as_deref()
    }

    /// Main window: hide the projects that are open in their own windows.
    /// A selection that just left the scope moves on — the chat to the most
    /// recent listed session (else the canvas), the canvas project to the
    /// first listed one.
    pub fn set_hidden_projects(&mut self, hidden: HashSet<String>, cx: &mut Context<Self>) {
        if self.scope.hidden == hidden {
            return;
        }
        self.scope.hidden = hidden;
        if self
            .selected_chat_row()
            .is_some_and(|chat| !self.scope.chat_visible(chat))
        {
            let next = self
                .overview_chats(Utc::now())
                .first()
                .map(|(_, c)| c.id.clone());
            self.select_chat(next, cx);
        }
        if self
            .selected_space
            .as_deref()
            .is_some_and(|id| !self.scope.space_visible(id))
        {
            self.selected_space = self.first_space_on_picked_device();
        }
        cx.notify();
    }

    // ---- reducers (pure) ----

    pub fn apply_chats(&mut self, mut chats: Vec<Chat>) {
        sort_chats(&mut chats);
        self.chats = chats;
        self.chats_synced = true;
        self.chats_generation = self.chats_generation.wrapping_add(1);
        self.drop_vanished_chat();
    }

    /// Selected chat vanished (deleted elsewhere): drop selection +
    /// transcript. A chat with a send in flight is kept: a canvas's first
    /// send selects the client-minted id before the row's chats frame lands,
    /// and an unrelated frame in that gap must not drop it (the tile would
    /// close mid-send). The overlay is TTL-bounded, so a send that never
    /// creates the row still lets the selection go. Returns whether it
    /// dropped.
    fn drop_vanished_chat(&mut self) -> bool {
        if let Some(selected) = &self.selected_chat
            && !self.chats.iter().any(|c| &c.id == selected)
            && !self.send_pending(selected, Utc::now())
        {
            self.selected_chat = None;
            self.transcript.clear();
            self.commands.clear();
            self.transcript_task = None;
            self.commands_task = None;
            self.bump_transcript();
            return true;
        }
        false
    }

    pub fn apply_sessions(&mut self, sessions: Vec<Session>) {
        self.sessions = sessions;
    }

    /// Project one `WatchSideChatStatus` frame into `sessions` (upsert by
    /// chat id). Temporary side chats never appear in the public
    /// `WatchSessions` stream; this is the ONLY status channel for a fork, and
    /// projecting it into `sessions` makes the reused Transcript/Composer
    /// status logic (`session_for`, `indicator_for`, `run_live`) work
    /// unchanged. `device_id` is the side chat's authoritative host device.
    pub fn apply_side_chat_status(&mut self, status: SideChatStatus, target_device_id: &str) {
        let session = Session {
            chat_id: status.side_chat_id,
            device_id: target_device_id.to_string(),
            status: status.status,
            started_at: status.started_at,
            updated_at: status.updated_at,
            subagents: Vec::new(),
            context_usage: None,
            throughput: None,
        };
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|s| s.chat_id == session.chat_id)
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    /// Build a forked/secondary [`AppState`] for one temporary Side Chat: a
    /// synthetic selected `Chat` row inheriting the parent's
    /// device/space/cwd/branch/checkout/config, plus the targeted
    /// `WatchDocMessages` (transcript) and private `WatchSideChatStatus`
    /// watches — and nothing else. The main state's selection is untouched;
    /// the shared [`EngineHandle`] is cloned, never restarted. The EXISTING
    /// `Transcript` / `Composer` components (which read `selected_chat`,
    /// `transcript`, `pending_echoes` and `sessions`) work unchanged on it.
    ///
    /// No normal `WatchChats`/`WatchSessions`/`WatchSpaces`/`WatchDevices`
    /// watches run in the fork — they would replace the synthetic row/list
    /// state. The remote `targetDeviceId` stays authoritative on the two
    /// watches and on every side-chat RPC.
    ///
    /// The parent chat is read from `main`; when the parent row is missing
    /// (should not happen — the shell's StartSideChat race guard disposes
    /// late starts) the fork still exists but carries no synthetic row, so
    /// the panel renders a degraded empty transcript.
    pub fn new_side_chat_fork(
        main: &Entity<AppState>,
        parent_chat_id: &str,
        side_chat_id: &str,
        target_device_id: &str,
        cx: &mut App,
    ) -> Entity<AppState> {
        let (engine, local, workspace_scope, parent, parent_space, devices) = {
            let m = main.read(cx);
            let parent = m.chats.iter().find(|c| c.id == parent_chat_id).cloned();
            let parent_space = parent
                .as_ref()
                .and_then(|p| p.space_id.as_deref())
                .and_then(|space_id| m.spaces.iter().find(|s| s.id == space_id).cloned());
            (
                m.engine.clone(),
                m.local_device_id.clone(),
                m.workspace_scope,
                parent,
                parent_space,
                m.devices.clone(),
            )
        };
        let fork = cx.new(|_cx| {
            let mut s = AppState::new();
            s.engine = engine.clone();
            s.connection = ConnectionStatus::Ready;
            s.workspace_scope = workspace_scope;
            s.local_device_id = local;
            s.devices = devices;
            s.spaces = parent_space.into_iter().collect();
            if let Some(parent) = parent {
                let synthetic = side_chat_synthetic_row(&parent, side_chat_id, target_device_id);
                s.chats = vec![synthetic];
                s.selected_chat = Some(side_chat_id.to_string());
                s.selected_space = parent.space_id.clone();
                s.selected_device = Some(target_device_id.to_string());
                s.no_project = parent.space_id.is_none();
            }
            s
        });
        // The fork's standing (and only) watches: the targeted transcript
        // watch and the private status watch. No WatchChats/WatchSessions —
        // they would erase the synthetic state.
        fork.update(cx, |s, cx| {
            if let Some(engine) = engine {
                s.transcript_task = Some(spawn_fork_transcript_watch(
                    cx,
                    engine.clone(),
                    side_chat_id.to_string(),
                    target_device_id.to_string(),
                ));
                s.watch_tasks.push(spawn_side_chat_status_watch(
                    cx,
                    engine,
                    side_chat_id.to_string(),
                    target_device_id.to_string(),
                ));
            }
        });
        fork
    }

    /// Optimistic insert for a promoted Side Chat: the engine has
    /// already created the row (PromoteSideChat is synchronous engine-side),
    /// so this local copy makes the promotion seamless — the sidebar renders
    /// and the chat is selectable immediately, before the next chats frame
    /// replaces it with the authoritative row. Idempotent: a row that already
    /// arrived is left untouched.
    pub fn insert_chat_optimistic(&mut self, chat: Chat) {
        if self.chats.iter().any(|c| c.id == chat.id) {
            return;
        }
        self.chats.push(chat);
        sort_chats(&mut self.chats);
    }

    pub fn apply_spaces(&mut self, mut spaces: Vec<Space>) {
        sort_spaces(&mut spaces);
        self.spaces = spaces;
        self.spaces_synced = true;
        self.heal_space_selection();
    }

    fn heal_space_selection(&mut self) {
        // Heal a vanished selection (project deleted elsewhere): fall back to
        // the first project; its chats died with it, so a matching chat
        // selection is healed by the accompanying chats frame (`apply_chats`).
        // The picker lists projects per-device, so healing prefers one on the
        // picked device — a global fallback would silently re-aim the canvas
        // at another machine.
        if let Some(selected) = &self.selected_space
            && !self.spaces.iter().any(|s| &s.id == selected)
        {
            self.selected_space = self.first_space_on_picked_device();
        }
        // First frame with no selection yet: pick the first project so the
        // canvas never boots project-less by accident — unless the user
        // deliberately opted out.
        if self.selected_space.is_none() && !self.no_project {
            self.selected_space = self.first_space_on_picked_device();
        }
    }

    /// Optimistic local echo of a `setChatConfig` mutate: stamp the row now so
    /// the chips update on click; the next chats watch frame carries the same
    /// value once the engine applies the LWW write.
    pub fn apply_chat_config(&mut self, chat_id: &str, config: cypher_proto::ChatConfig) {
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.config = Some(config);
        }
    }

    /// [`Self::apply_chat_config`] from a view: a session context stamps its
    /// parent too, or the next mirror copy would revert the chips until the
    /// engine's chats frame lands. Must not run inside the parent's update.
    pub fn set_chat_config_optimistic(
        &mut self,
        chat_id: &str,
        config: cypher_proto::ChatConfig,
        cx: &mut Context<Self>,
    ) {
        if let Some(parent) = self.parent() {
            parent.update(cx, |parent, cx| {
                parent.set_chat_config_optimistic(chat_id, config.clone(), cx);
            });
        }
        self.apply_chat_config(chat_id, config);
        cx.notify();
    }

    pub fn apply_devices(&mut self, mut devices: Vec<Device>) {
        // A local-only workspace has no remote device identity to distinguish.
        // Keep the engine's legacy sentinel out of the UI while preserving real
        // hostnames and user-assigned device names.
        if self.workspace_scope == Some(WorkspaceScope::Local)
            && let Some(local_id) = self.local_device_id.as_deref()
            && let Some(device) = devices.iter_mut().find(|device| device.id == local_id)
            && device.name == "unknown-device"
        {
            device.name = "Local".to_string();
        }
        self.devices = devices;
    }

    /// First project on the composer's picked device (falling back through
    /// the local device, then any project at all — better a cross-device
    /// project than a surprise project-less canvas). Display order.
    ///
    /// Public: the deterministic live-space fallback for the new-session
    /// canvas (and the healing of a vanished selection).
    pub fn first_space_on_picked_device(&self) -> Option<String> {
        let device = self
            .selected_device
            .as_deref()
            .or(self.local_device_id.as_deref());
        let sorted = self.spaces_sorted();
        device
            .and_then(|d| sorted.iter().find(|s| s.device_id == d).copied())
            .or_else(|| sorted.first().copied())
            .map(|s| s.id.clone())
    }

    pub fn apply_update(&mut self, status: cypher_update::UpdateStatus) {
        self.update = Some(status);
    }

    pub fn apply_pi_update(&mut self, status: cypher_engine::pi_packages::PiUpdateStatus) {
        self.pi_update = Some(status);
    }

    pub fn apply_auth(&mut self, auth: AuthState) {
        self.auth = Some(auth);
    }

    /// Tolerant AuthStatus frame reducer (see [`parse_auth_state`]).
    pub fn apply_auth_value(&mut self, value: serde_json::Value) {
        match parse_auth_state(&value) {
            Some(auth) => self.apply_auth(auth),
            None => tracing::warn!("dropping unrecognized AuthStatus frame"),
        }
    }

    /// The signed-in user, if the engine reports one.
    pub fn auth_user(&self) -> Option<&cypher_proto::UserProfile> {
        match self.auth.as_ref()? {
            AuthState::SignedIn { user, .. } | AuthState::NeedsOrganization { user } => Some(user),
            AuthState::SignedOut => None,
        }
    }

    #[cfg(test)]
    pub fn apply_transcript(&mut self, entries: Vec<SessionMessageEntry>) {
        // Doc frames supersede optimistic echoes carrying the same id.
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(echoes) = self.echoes.get_mut(chat_id)
        {
            echoes.retain(|echo| !entries.iter().any(|e| e.id == echo.id));
        }
        self.transcript = entries;
        self.bump_transcript();
        self.ack_pending_send_from_transcript();
    }

    /// Apply a `WatchDocMessages` delta frame in place. `Err` = this copy has
    /// diverged; the watch task resubscribes for a fresh reset.
    pub fn apply_transcript_frame(
        &mut self,
        frame: TranscriptFrame,
    ) -> Result<(), TranscriptDesync> {
        // Bumped even on a desync: the partially applied copy renders.
        self.bump_transcript();
        cypher_doc::apply_transcript_frame(&mut self.transcript, frame)?;
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(echoes) = self.echoes.get_mut(chat_id)
        {
            let transcript = &self.transcript;
            echoes.retain(|echo| !transcript.iter().any(|e| e.id == echo.id));
        }
        self.ack_pending_send_from_transcript();
        Ok(())
    }

    /// Add an optimistic user echo (composer send path).
    pub fn push_echo(&mut self, chat_id: &str, entry: SessionMessageEntry) {
        let echoes = self.echoes.entry(chat_id.to_string()).or_default();
        if !echoes.iter().any(|e| e.id == entry.id) {
            echoes.push(entry);
        }
        self.bump_transcript();
    }

    /// Mark a message id as sent via Steer (see [`Self::steer_message_ids`]).
    pub fn mark_steer(&mut self, message_id: &str) {
        self.local_steers.insert(message_id.to_string());
        self.bump_transcript();
    }

    /// User messages of the selected chat that were steers: the explicit
    /// message ids on the ledger's Steer commands (the iOS join), plus this
    /// device's own not-yet-synced steers. Old messages without a matching
    /// id stay plain prompts.
    pub fn steer_message_ids(&self) -> HashSet<String> {
        self.commands
            .iter()
            .filter_map(|c| match &c.payload {
                SessionCommandPayload::Steer {
                    message_id: Some(id),
                    ..
                } if !id.is_empty() => Some(id.clone()),
                _ => None,
            })
            .chain(self.local_steers.iter().cloned())
            .collect()
    }

    /// Drop an echo (send failed — the prompt returns to the draft).
    pub fn remove_echo(&mut self, chat_id: &str, message_id: &str) {
        if let Some(echoes) = self.echoes.get_mut(chat_id) {
            echoes.retain(|e| e.id != message_id);
        }
        self.bump_transcript();
    }

    /// Composer send fired: overlay the chat as Working until the host writes
    /// the user message back into the transcript (or the TTL lapses). A remote
    /// send has no live session row until the host drains the queued command —
    /// that gap read as "no live run" and flashed the Completed dot, and any
    /// phantom Working→Idle edge in it rang the done-chime on send (user
    /// report 2026-08-05).
    pub fn begin_pending_send(&mut self, chat_id: &str, message_id: &str, now: DateTime<Utc>) {
        self.pending_sends.borrow_mut().insert(
            chat_id.to_string(),
            PendingSend {
                message_id: message_id.to_string(),
                started: now,
            },
        );
    }

    /// Send failed — drop the overlay so the dot tells the truth again. Only
    /// removes the overlay this message started: a quick resend must not lose
    /// its own overlay to the first send's failure cleanup.
    pub fn end_pending_send(&mut self, chat_id: &str, message_id: &str) {
        let mut pending = self.pending_sends.borrow_mut();
        if pending
            .get(chat_id)
            .is_some_and(|p| p.message_id == message_id)
        {
            pending.remove(chat_id);
        }
    }

    /// Is a send still in flight for this chat (unacked, inside the TTL)?
    pub fn send_pending(&self, chat_id: &str, now: DateTime<Utc>) -> bool {
        self.pending_sends.borrow().get(chat_id).is_some_and(|p| {
            now.signed_duration_since(p.started).num_milliseconds() <= PENDING_SEND_TTL_MS
        })
    }

    /// When the in-flight send (if any, inside the TTL) was fired — the
    /// elapsed-timer base while the overlay reads as Working. The session
    /// row's `started_at` still belongs to the PREVIOUS turn during this
    /// window, and showing it made a fresh send open at the old turn's
    /// half-hour mark.
    pub fn pending_send_started(&self, chat_id: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.pending_sends
            .borrow()
            .get(chat_id)
            .filter(|p| {
                now.signed_duration_since(p.started).num_milliseconds() <= PENDING_SEND_TTL_MS
            })
            .map(|p| p.started)
    }

    /// The host executed the queued command iff the sent message's id showed
    /// up in the transcript (it writes the message before — causally with —
    /// the Working status; sessions.rs dispatch paths).
    fn ack_pending_send_from_transcript(&mut self) {
        let Some(chat_id) = self.selected_chat.as_deref() else {
            return;
        };
        let mut pending = self.pending_sends.borrow_mut();
        if pending
            .get(chat_id)
            .is_some_and(|p| self.transcript.iter().any(|e| e.id == p.message_id))
        {
            pending.remove(chat_id);
        }
    }

    /// Unconfirmed echoes for the selected chat, in send order.
    pub fn pending_echoes(&self) -> &[SessionMessageEntry] {
        self.selected_chat
            .as_deref()
            .and_then(|id| self.echoes.get(id))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Fold one `WatchDocCommands` frame (the current ledger) into state.
    /// The ledger is the durable truth: a Rejected/Expired command ends the
    /// optimistic send-in-flight overlay so the sidebar dot stops reading
    /// "Working" for a message the host refused, and the composer's failed
    /// row + Retry take over.
    pub fn apply_commands(&mut self, commands: Vec<SessionCommandEntry>) {
        self.commands = commands;
        self.bump_transcript();
        let Some(chat_id) = self.selected_chat.as_deref() else {
            return;
        };
        let mut pending = self.pending_sends.borrow_mut();
        if pending.get(chat_id).is_some_and(|p| {
            command_send_status(&self.commands, &p.message_id) == Some(CommandSendStatus::Failed)
        }) {
            pending.remove(chat_id);
        }
    }

    /// Projected status of the selected chat's message command, or `None`
    /// when no Run/Steer command exists for it.
    pub fn command_status_for(&self, message_id: &str) -> Option<CommandSendStatus> {
        command_send_status(&self.commands, message_id)
    }

    /// The retry-able failures of the selected chat's ledger (composer row).
    pub fn failed_commands(&self) -> Vec<FailedCommand> {
        failed_commands(&self.commands)
    }

    /// Whether an optimistic echo is still genuinely in flight. A message
    /// whose latest attempt FAILED renders at full opacity (the failed row
    /// explains it) instead of the 0.65 sending veil forever.
    pub fn echo_pending(&self, message_id: &str) -> bool {
        self.command_status_for(message_id) != Some(CommandSendStatus::Failed)
    }

    pub fn begin_upload_progress(
        &mut self,
        chat_id: &str,
        total_bytes: u64,
        completed_bytes: Arc<std::sync::atomic::AtomicU64>,
    ) {
        self.upload_progress = Some(UploadProgress {
            chat_id: chat_id.to_string(),
            total_bytes: total_bytes.max(1),
            completed_bytes,
        });
    }

    /// Retire the upload trailer when `chat_id`'s send leaves the streaming
    /// stage — success or failure. Scoped by chat so a finishing send never
    /// cancels a LATER upload that has already claimed the slot.
    pub fn end_upload_progress(&mut self, chat_id: &str) {
        if self
            .upload_progress
            .as_ref()
            .is_some_and(|p| p.chat_id == chat_id)
        {
            self.upload_progress = None;
        }
    }

    /// Percent uploaded for `chat_id`'s in-flight send, or `None` when this
    /// chat has no upload streaming right now.
    pub fn upload_progress_percent(&self, chat_id: &str) -> Option<u8> {
        let progress = self.upload_progress.as_ref()?;
        if progress.chat_id != chat_id {
            return None;
        }
        let completed = progress
            .completed_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(progress.total_bytes);
        Some(((completed.saturating_mul(100)) / progress.total_bytes) as u8)
    }

    // ---- queries ----

    /// Non-archived, NON-CHILD chats in sidebar order. Cypher child subagent
    /// chats (engine-owned `child: Some(..)` rows) are hidden from the root
    /// sidebar/session overview — they are reached only through the parent's
    /// Subagents inspector. [`Self::selected_chat_row`] and transcript
    /// subscriptions still work for a selected child (navigation selects it
    /// directly).
    ///
    /// Scoped to this window's projects ([`ProjectScope`]).
    pub fn visible_chats(&self) -> impl Iterator<Item = &Chat> {
        self.chats
            .iter()
            .filter(|c| !c.archived && !c.is_child() && self.scope.chat_visible(c))
    }

    pub fn selected_space_row(&self) -> Option<&Space> {
        if self.no_project {
            return None;
        }
        let id = self.selected_space.as_deref()?;
        self.spaces.iter().find(|s| s.id == id)
    }

    /// The device the new-session canvas targets: the picked project's host
    /// when one is selected, else the explicit device pick, else this device.
    pub fn effective_device_id(&self) -> Option<String> {
        if let Some(space) = self.selected_space_row() {
            return Some(space.device_id.clone());
        }
        self.selected_device
            .clone()
            .or_else(|| self.local_device_id.clone())
    }

    /// Pick the composer's target device. Keeps the project pick consistent:
    /// a project on another device can't survive the switch — fall back to
    /// the first project on the new device, else "no project".
    pub fn select_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        let project_moves = self
            .selected_space_row()
            .is_some_and(|s| s.device_id != device_id);
        if project_moves {
            let first = self
                .spaces_sorted()
                .iter()
                .find(|s| s.device_id == device_id)
                .map(|s| s.id.clone());
            self.no_project = first.is_none();
            if first.is_some() {
                self.selected_space = first;
            }
        }
        self.selected_device = Some(device_id);
        cx.notify();
    }

    /// Aim the canvas at a quick chat on `device_id`: project-less, with the
    /// next send minting a scratch folder there. The caller opens the canvas.
    pub fn begin_quick_chat(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.selected_device = Some(device_id);
        self.no_project = true;
        self.scratch_pending = true;
        cx.notify();
    }

    pub fn space_row(&self, space_id: &str) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == space_id)
    }

    /// The selected space id, but only while it still resolves to a LIVE
    /// Space — a dangling id (project deleted elsewhere) is `None`. Drives
    /// `last_space_id` persistence and the new-session fallback: a dead
    /// selection must never be remembered or re-aimed at.
    pub fn selected_space_if_live(&self) -> Option<String> {
        self.selected_space
            .as_deref()
            .filter(|id| self.space_row(id).is_some())
            .map(str::to_string)
    }

    /// Spaces in display order — case-insensitive alphabetical, the order
    /// the space selectors (the canvas project picker, composer) list rows in.
    /// Ties break on id so the order is stable across renders.
    /// Scoped to this window's projects ([`ProjectScope`]).
    pub fn spaces_sorted(&self) -> Vec<&Space> {
        let mut spaces: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|s| self.scope.space_visible(&s.id))
            .collect();
        spaces.sort_by_key(|s| (s.display_name().to_lowercase(), s.id.clone()));
        spaces
    }

    /// Non-archived chats of a space in tab (creation) order. Chats with a
    /// dangling/missing `space_id` are invisible by construction.
    pub fn chats_in_space(&self, space_id: &str) -> Vec<&Chat> {
        let mut chats: Vec<&Chat> = self
            .visible_chats()
            .filter(|c| c.space_id.as_deref() == Some(space_id))
            .collect();
        sort_tabs(&mut chats);
        chats
    }

    pub fn device_name(&self, device_id: &str) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.id == device_id)
            .map(|d| d.name.as_str())
    }

    /// Host-presence check: is this device's 15s presence heartbeat fresh?
    /// Distinguishes "host offline" (its queued work syncs when it returns)
    /// from slow sync. The local device is trivially online; unknown devices
    /// get the benefit of the doubt (no evidence — don't cry wolf).
    pub fn device_online(&self, device_id: &str, now: DateTime<Utc>) -> bool {
        if self.local_device_id.as_deref() == Some(device_id) {
            return true;
        }
        match self.devices.iter().find(|d| d.id == device_id) {
            Some(d) => crate::settings::devices::device_online(d.last_seen_at, now),
            None => true,
        }
    }

    /// Does the selected space's folder have git? Drives the branch picker and
    /// the diff sidebar (owner-stamped, synced — no RPC).
    pub fn selected_space_git(&self) -> bool {
        self.selected_space_row().is_some_and(|s| s.git_detected)
    }

    /// Full display status for a chat (tab dots, Active list). A send in
    /// flight ([`Self::begin_pending_send`]) reads as Working — the queued
    /// command is as good as running.
    pub fn display_status_for(&self, chat: &Chat, now: DateTime<Utc>) -> ChatIndicator {
        if self.send_pending(&chat.id, now) {
            return ChatIndicator::Working;
        }
        display_status(chat, self.session_for(&chat.id), now)
    }

    /// The sidebar's Sessions list: every non-archived chat of a LIVE space,
    /// on any device — idle included — in pure recency order (status drives
    /// the dot, never the position; see [`sort_active`]).
    pub fn overview_chats(&self, now: DateTime<Utc>) -> Vec<(ChatIndicator, &Chat)> {
        let mut rows: Vec<(ChatIndicator, &Chat)> = self
            .visible_chats()
            .filter(|c| match c.space_id.as_deref() {
                // Project-less sessions are first-class rows.
                None => true,
                Some(id) => self.space_row(id).is_some(),
            })
            .map(|c| (self.display_status_for(c, now), c))
            .collect();
        sort_active(&mut rows);
        rows
    }

    /// The Dock badge: sessions whose sidebar corner asks for you — waiting
    /// on an answer, errored, or finished — with activity newer than the
    /// synced seen marker. Opening a session on ANY device moves that marker,
    /// so a read on the phone takes it off this badge too (and the Worker
    /// clears the phones' badges off the same marker). The host bumps
    /// `lastMessageAt` when a run starts asking or fails, so a question
    /// counts until it's been looked at, not until it's answered.
    ///
    /// App-wide: the Dock icon is shared by every window, so this counts
    /// the sessions of projects open in their own windows too (the
    /// [`ProjectScope`] is ignored).
    pub fn attention_count(&self, now: DateTime<Utc>) -> usize {
        self.chats
            .iter()
            .filter(|c| !c.archived && !c.is_child())
            .filter(|c| match c.space_id.as_deref() {
                None => true,
                Some(id) => self.space_row(id).is_some(),
            })
            .filter(|chat| {
                matches!(
                    self.display_status_for(chat, now),
                    ChatIndicator::AwaitingInput
                        | ChatIndicator::Errored
                        | ChatIndicator::Completed
                ) && chat.unseen()
            })
            .count()
    }

    /// The project-grouped sidebar: one card per live `Space` (empty spaces
    /// included, so project management stays reachable), plus synthetic
    /// cards for project-less chats ("No project", per device) and chats
    /// whose `space_id` names a missing space ("Unavailable project", keyed
    /// by the missing id). Groups with chats are ordered by their newest chat
    /// (the overview recency order, preserved inside each group); empty
    /// spaces are appended deterministically by display name / device / path
    /// / id. Status changes never reorder. Archived and child chats stay
    /// excluded. Pure — see the tests in [`mod tests`] for the exact rules.
    #[cfg(test)]
    pub fn sidebar_groups(&self, now: DateTime<Utc>) -> Vec<SidebarGroup<'_>> {
        self.sidebar_groups_with(now, &SidebarView::default())
    }

    /// [`Self::sidebar_groups`] under the sidebar view menu's filter and
    /// sort. The filter drops cards hosted elsewhere; the sort reorders
    /// cards (and their sessions) with stable sorts so ties keep the
    /// activity order, and pins always lead.
    pub fn sidebar_groups_with(
        &self,
        now: DateTime<Utc>,
        view: &SidebarView,
    ) -> Vec<SidebarGroup<'_>> {
        let mut groups = self.sidebar_groups_unsorted(now);
        if let Some(device) = view.device.as_deref() {
            groups.retain(|g| g.device_id == device);
        }
        merge_scratch_groups(&mut groups);
        match view.sort {
            SidebarSort::Activity => {}
            SidebarSort::Name => {
                groups.sort_by_cached_key(|g| (g.title.to_lowercase(), g.device.to_lowercase()));
            }
            SidebarSort::Device => {
                groups.sort_by_cached_key(|g| g.device.to_lowercase());
            }
            SidebarSort::Date => {
                groups.sort_by_key(|g| std::cmp::Reverse(g.created_at));
            }
        }
        for group in &mut groups {
            match view.sort {
                SidebarSort::Name => group
                    .chats
                    .sort_by_cached_key(|(_, c)| c.title.as_deref().unwrap_or("").to_lowercase()),
                SidebarSort::Date => group
                    .chats
                    .sort_by_key(|(_, c)| std::cmp::Reverse(c.created_at)),
                SidebarSort::Activity | SidebarSort::Device => {}
            }
        }
        // Reversed direction flips cards, and sessions for the sorts that
        // ordered them (Device leaves sessions in activity order).
        if view.reversed {
            groups.reverse();
            if view.sort != SidebarSort::Device {
                for group in &mut groups {
                    group.chats.reverse();
                }
            }
        }
        // Pins: a pinned project leads the list and a pinned session leads
        // its project, each keeping the sort order among themselves.
        groups.sort_by_key(|g| !g.pinned);
        for group in &mut groups {
            group.chats.sort_by_key(|(_, chat)| !chat.pinned);
        }
        groups
    }

    fn sidebar_groups_unsorted(&self, now: DateTime<Utc>) -> Vec<SidebarGroup<'_>> {
        let mut all: Vec<(ChatIndicator, &Chat)> = self
            .visible_chats()
            .map(|c| (self.display_status_for(c, now), c))
            .collect();
        sort_active(&mut all);

        // Fold chats into groups in overview order. A group is keyed by its
        // live space (`s:<id>`), a missing space id (`u:<id>`), or a device
        // (`np:<device id>` for project-less chats). First appearance orders
        // the groups by their newest chat; within a group the overview order
        // is preserved. Status changes leave the keys and order untouched.
        let mut groups: Vec<SidebarGroup> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (status, chat) in all {
            // Quick chats are keyed per device here so the device filter can
            // drop other hosts; `merge_scratch_groups` then folds whatever
            // survives into the single `sc` card.
            let (key, kind) = match chat.space_id.as_deref() {
                None if chat.is_scratch() => {
                    (format!("sc:{}", chat.device_id), SidebarGroupKind::Scratch)
                }
                None => (
                    format!("np:{}", chat.device_id),
                    SidebarGroupKind::NoProject,
                ),
                Some(id) if self.space_row(id).is_some() => {
                    (format!("s:{id}"), SidebarGroupKind::Space)
                }
                Some(id) => (format!("u:{id}"), SidebarGroupKind::Unavailable),
            };
            if let Some(&ix) = index.get(&key) {
                groups[ix].chats.push((status, chat));
                continue;
            }
            index.insert(key.clone(), groups.len());
            let (space, title, path) = match kind {
                SidebarGroupKind::Space => {
                    let space = self
                        .space_row(chat.space_id.as_deref().expect("space kind has an id"))
                        .expect("space kind resolves");
                    (
                        Some(space),
                        space.display_name().to_string(),
                        Some(space.path.clone()),
                    )
                }
                SidebarGroupKind::NoProject => (None, "No project".into(), None),
                SidebarGroupKind::Scratch => (None, "Quick chats".into(), None),
                SidebarGroupKind::Unavailable => (None, "Unavailable project".into(), None),
            };
            let (device, offline) = match space {
                Some(space) => (
                    self.device_name(&space.device_id)
                        .unwrap_or("Unknown device")
                        .to_string(),
                    !self.device_online(&space.device_id, now),
                ),
                None => (
                    self.device_name(&chat.device_id)
                        .unwrap_or("Unknown device")
                        .to_string(),
                    false,
                ),
            };
            let device_id = space
                .map(|s| s.device_id.clone())
                .unwrap_or_else(|| chat.device_id.clone());
            let created_at = space.map(|s| s.created_at).unwrap_or(chat.created_at);
            groups.push(SidebarGroup {
                key,
                kind,
                title,
                path,
                device,
                device_id,
                offline,
                created_at,
                space_id: space.map(|s| s.id.as_str()),
                pinned: space.is_some_and(|s| s.pinned),
                icon: space.and_then(|s| s.icon.clone()),
                color: space.and_then(|s| s.color.clone()),
                chats: vec![(status, chat)],
            });
        }

        // Append live spaces with no visible chats: project management must
        // stay reachable even when a space is quiet. Deterministic order
        // (display name / device / path / id) so an empty space never moves
        // between renders.
        let live: HashSet<&str> = groups
            .iter()
            .filter(|g| g.kind == SidebarGroupKind::Space)
            .filter_map(|g| g.space_id)
            .collect();
        let mut empty: Vec<&Space> = self
            .spaces
            .iter()
            .filter(|s| !live.contains(s.id.as_str()) && self.scope.space_visible(&s.id))
            .collect();
        empty.sort_by(|a, b| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
                .then_with(|| {
                    self.device_name(&a.device_id)
                        .unwrap_or("")
                        .cmp(self.device_name(&b.device_id).unwrap_or(""))
                })
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.id.cmp(&b.id))
        });
        groups.extend(empty.into_iter().map(|space| {
            SidebarGroup {
                key: format!("s:{}", space.id),
                kind: SidebarGroupKind::Space,
                title: space.display_name().to_string(),
                path: Some(space.path.clone()),
                device: self
                    .device_name(&space.device_id)
                    .unwrap_or("Unknown device")
                    .to_string(),
                device_id: space.device_id.clone(),
                offline: !self.device_online(&space.device_id, now),
                created_at: space.created_at,
                space_id: Some(space.id.as_str()),
                pinned: space.pinned,
                icon: space.icon.clone(),
                color: space.color.clone(),
                chats: Vec::new(),
            }
        }));
        groups
    }

    pub fn session_for(&self, chat_id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.chat_id == chat_id)
    }

    /// Staleness-checked status dot for a chat row. A send in flight reads as
    /// Working (see [`Self::display_status_for`]).
    pub fn indicator_for(&self, chat_id: &str, now: DateTime<Utc>) -> Indicator {
        if self.send_pending(chat_id, now) {
            return Indicator::Working;
        }
        effective_indicator(self.session_for(chat_id), now)
    }

    pub fn selected_chat_row(&self) -> Option<&Chat> {
        let id = self.selected_chat.as_deref()?;
        self.chats.iter().find(|c| c.id == id)
    }

    pub fn gate(&self) -> GatePhase {
        gate_phase(&self.connection, self.workspace_scope, self.auth.as_ref())
    }

    pub fn engine(&self) -> Option<&EngineHandle> {
        self.engine.as_ref()
    }

    /// Drop every account-scoped view and subscription after its runtime has
    /// stopped. The next bootstrap must never render rows from the previous
    /// account while the local profile is opening.
    pub fn prepare_runtime_replacement(&mut self, cx: &mut Context<Self>) {
        self.engine = None;
        self.watch_tasks.clear();
        self.transcript_task = None;
        self.commands_task = None;
        self.connection = ConnectionStatus::Connecting;
        self.workspace_scope = None;
        self.auth = None;
        self.devices.clear();
        self.spaces.clear();
        self.chats.clear();
        self.sessions.clear();
        self.selected_space = None;
        self.no_project = false;
        self.scratch_pending = false;
        self.selected_device = None;
        self.selected_chat = None;
        self.auto_selected = false;
        self.chats_synced = false;
        self.spaces_synced = false;
        self.transcript.clear();
        self.commands.clear();
        self.echoes.clear();
        self.bump_transcript();
        self.pending_sends.borrow_mut().clear();
        self.upload_progress = None;
        self.local_device_id = None;
        self.update = None;
        self.pi_update = None;
        cx.notify();
    }

    // ---- gpui glue ----

    /// Kick off (or retry) the engine bootstrap: probe → connect-or-embed on
    /// tokio, then attach subscriptions. Safe to call again after `Failed`.
    pub fn bootstrap(
        state: Entity<AppState>,
        data_dir: PathBuf,
        config: EngineBootConfig,
        cx: &mut App,
    ) {
        state.update(cx, |s, cx| {
            s.connection = ConnectionStatus::Connecting;
            s.workspace_scope = None;
            s.auth = None;
            s.data_dir = Some(data_dir);
            cx.notify();
        });
        let boot = Tokio::spawn(cx, EngineHandle::bootstrap(config));
        cx.spawn(async move |cx| {
            let outcome = match boot.await {
                Ok(Ok(handle)) => Ok(handle),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(join_err) => Err(join_err.to_string()),
            };
            // NB: at the pinned rev `Entity::update(&mut AsyncApp)` returns the
            // closure's value directly (no Result) — AsyncApp implements
            // AppContext like App does.
            state.update(cx, |s, cx| match outcome {
                Ok(handle) => s.attach_engine(handle, true, cx),
                Err(message) => {
                    tracing::error!(%message, "engine bootstrap failed");
                    s.connection = ConnectionStatus::Failed(message);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Wire the connected engine: mark Ready and start the standing watches.
    /// Methods the engine doesn't serve yet (chats/devices/auth land with the
    /// workspace doc in M4) fail their subscribe and are skipped gracefully.
    /// `owner`: this state bootstrapped the handle, so it also watches the
    /// deferred engine assembly (and shuts the handle down on failure). A
    /// project window's state only borrows the main window's handle.
    fn attach_engine(&mut self, handle: EngineHandle, owner: bool, cx: &mut Context<Self>) {
        let engine_info = handle.engine_info();
        self.workspace_scope = Some(engine_info.workspace_scope);
        self.local_device_id = Some(engine_info.device_id.clone());
        self.engine = Some(handle.clone());
        let mut watch_tasks = Vec::with_capacity(8);
        if owner && let Some(task) = spawn_deferred_engine_watch(cx, handle.clone()) {
            watch_tasks.push(task);
        }
        watch_tasks.extend([
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SESSIONS,
                AppState::apply_sessions,
            ),
            spawn_chats_watch(cx, handle.clone()),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_DEVICES,
                AppState::apply_devices,
            ),
            spawn_watch(
                cx,
                handle.clone(),
                methods::WATCH_SPACES,
                AppState::apply_spaces,
            ),
            // Auth frames parse tolerantly — engine and proto tags differ today.
            spawn_watch(
                cx,
                handle.clone(),
                methods::AUTH_STATUS,
                AppState::apply_auth_value,
            ),
            spawn_update_watch(cx, handle.clone()),
            spawn_pi_update_watch(cx, handle.clone()),
            spawn_local_device_probe(cx, handle.clone()),
        ]);
        self.watch_tasks = watch_tasks;
        // EngineInfo is part of the attachment boundary: views must know which
        // data profile they reached before they are allowed to render Ready.
        self.connection = ConnectionStatus::Ready;
        // Re-subscribe the transcript if a chat was already selected (reconnect path).
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    /// Subscribe the selected chat's transcript + ledger (when this state
    /// owns transcripts and has an engine). Callers drop the old tasks.
    fn spawn_transcript_watches(&mut self, cx: &mut Context<Self>) {
        if !self.transcript_watches {
            return;
        }
        if let (Some(chat_id), Some(handle)) = (self.selected_chat.clone(), self.engine.clone()) {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            self.commands_task = Some(spawn_commands_watch(cx, handle, chat_id));
        }
    }

    /// Lists-only mode (`false`): the selection keeps driving the sidebar,
    /// space and seen marks, but this state subscribes no transcript — the
    /// session tiles' contexts own them. Switching drops or re-subscribes
    /// the current selection's watches.
    pub fn set_transcript_watches(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.transcript_watches == on {
            return;
        }
        self.transcript_watches = on;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
        self.transcript_task = None;
        self.commands_task = None;
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub fn transcript_watches(&self) -> bool {
        self.transcript_watches
    }

    /// Select a chat (or clear). Swaps the per-chat doc-transcript subscription:
    /// dropping the old task drops its stream receiver, which cancels the doc
    /// watch server-side. Selecting a chat also lands in its space and marks it
    /// seen (a global-list click must switch the tab strip too).
    pub fn select_chat(&mut self, chat_id: Option<String>, cx: &mut Context<Self>) {
        if self.selected_chat == chat_id {
            // Re-selecting still clears a fresh "completed" badge.
            if let Some(id) = chat_id {
                self.mark_chat_seen(&id, cx);
            }
            return;
        }
        self.selected_chat = chat_id.clone();
        self.auto_selected = true;
        self.transcript.clear();
        self.commands.clear();
        self.bump_transcript();
        self.transcript_task = None;
        self.commands_task = None;
        if let Some(id) = chat_id.as_deref() {
            // A chat implies its project (or the lack of one); `select_chat(None)`
            // (the new-session canvas) keeps the current project pick.
            self.scratch_pending = false;
            if let Some(chat) = self.chats.iter().find(|c| c.id == id) {
                match chat.space_id.clone() {
                    Some(space_id) => {
                        self.selected_space = Some(space_id);
                        self.no_project = false;
                    }
                    None => {
                        self.no_project = true;
                        self.selected_device = Some(chat.device_id.clone());
                    }
                }
            }
            self.mark_chat_seen(id, cx);
        }
        self.spawn_transcript_watches(cx);
        cx.notify();
    }

    /// Select a project; the caller (shell) decides which chat to land on.
    /// `Some` clears a "Don't work in a project" opt-out and re-aims the
    /// device pick at the project's host; `None` IS that opt-out.
    pub fn select_space(&mut self, space_id: Option<String>, cx: &mut Context<Self>) {
        match &space_id {
            Some(id) => {
                self.no_project = false;
                self.scratch_pending = false;
                if let Some(device) = self.space_row(id).map(|s| s.device_id.clone()) {
                    self.selected_device = Some(device);
                }
            }
            None => self.no_project = true,
        }
        if self.selected_space == space_id && space_id.is_some() {
            cx.notify();
            return;
        }
        if space_id.is_some() {
            self.selected_space = space_id;
        }
        cx.notify();
    }

    /// Window-focus liveness sweep: ask the engine to probe every open room
    /// (workspace + chat docs). Fire-and-forget; each room ignores the hint
    /// unless it has been broadcast-quiet ≥30s, so spamming is harmless.
    pub fn probe_sync(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({});
            if let Err(err) = handle.client().call(methods::PROBE_SYNC, params).await {
                tracing::debug!(error = %err, "probe sync failed");
            }
        })
        .detach();
    }

    pub fn report_notification_activity(
        &self,
        mut activity: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let Some(AuthState::SignedIn {
            user,
            org_id: Some(org),
        }) = &self.auth
        else {
            return;
        };
        activity["expectedUserId"] = serde_json::json!(user.id);
        activity["expectedOrgId"] = serde_json::json!(org);
        cx.spawn(async move |_, _| {
            // Old engines/disabled notification services must not affect the UI.
            let _ = handle
                .client()
                .call(methods::NOTIFICATION_ACTIVITY, activity)
                .await;
        })
        .detach();
    }

    /// Synced seen marker: only fires when the chat is currently unseen
    /// (idempotence — no mutate spam), stamps the local row optimistically so
    /// the LWW round-trip is invisible, and fire-and-forgets the mutate.
    ///
    /// A session context stamps its own row and forwards to its parent,
    /// which owns the mutate — one RPC, and the sidebar badge clears at
    /// once instead of after the next mirror copy. Must not run inside the
    /// parent's update.
    pub fn mark_chat_seen(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let stamped = match self.chats.iter_mut().find(|c| c.id == chat_id) {
            Some(chat) if chat.unseen() => {
                chat.last_seen_at = Some(Utc::now());
                cx.notify();
                true
            }
            _ => false,
        };
        if let Some(parent) = self.parent() {
            parent.update(cx, |parent, cx| parent.mark_chat_seen(chat_id, cx));
            return;
        }
        if !stamped {
            return;
        }
        let Some(handle) = self.engine.clone() else {
            return;
        };
        let chat_id = chat_id.to_string();
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "op": "markChatSeen", "chatId": chat_id });
            if let Err(err) = handle.client().call(methods::MUTATE, params).await {
                tracing::warn!(chat = %chat_id, error = %err, "markChatSeen failed");
            }
        })
        .detach();
    }
}

/// Observe assembly after an early attach (cloud onboarding or another viewport
/// reaching the embedded engine over IPC). Data subscriptions wait on the same
/// result, but their individual errors are not authoritative: older engines may
/// legitimately omit a watch method. Only the assembly result may fail the
/// whole connection.
fn spawn_deferred_engine_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
) -> Option<Task<()>> {
    let mut deferred = handle.deferred_state()?;
    Some(cx.spawn(async move |this, cx| {
        let Err(failure) = wait_for_deferred_engine(&mut deferred).await else {
            return;
        };
        tracing::error!(error = %failure, "engine assembly failed after attachment");
        // Embedded handles release their IPC listener before exposing Retry;
        // remote handles stop their completed readiness probe.
        handle.shutdown().await;
        this.update(cx, |state, cx| {
            state.connection = ConnectionStatus::Failed(failure);
            cx.notify();
        })
        .ok();
    }))
}

/// Chats watch. Boot selection is the shell's job (it lands on the first
/// restored open tab, device-local state this entity can't see); this task
/// only pumps frames.
fn spawn_chats_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop (same contract as the transcript watch): a daemon
        // restart or RPC drop ends the stream, and a bare return here froze
        // the sidebar until app restart — new chats, renames and archives
        // from every device silently stopped arriving.
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_CHATS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "chats watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                let parsed: Vec<Chat> = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed chats frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    state.apply_chats(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!("chats stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

fn spawn_watch<T: DeserializeOwned + 'static>(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    method: &'static str,
    apply: fn(&mut AppState, T),
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Resubscribe loop: these are the standing Sessions/Devices/Spaces
        // watches — a daemon restart ended the stream and a bare return froze
        // them for the rest of the app's life (remote Working dots staled out
        // to nothing after 45s, and Idle/Completed transitions from other
        // devices never arrived again — "the session never completes").
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut rx = match handle
                .client()
                .subscribe(method, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(method, error = %err, "watch unavailable; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                let parsed: T = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(method, error = %err, "dropping malformed watch frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    apply(state, parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            tracing::debug!(method, "watch stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// Capped exponential backoff for the UpdateStatus watch: 2, 4, 8, 16, then 30s
/// forever. The other standing watches retry at a flat 2s; the update strip is
/// advisory and must not churn the IPC + log every 2s while the stream is
/// unavailable or closes prematurely (the 0.1.0 local-only regression). A
/// stream that delivered a valid frame resets the step, so a healthy engine
/// restart is picked up quickly.
const UPDATE_BACKOFF_SECS: [u64; 5] = [2, 4, 8, 16, 30];

/// Delay for backoff `step` (0-based), capped at the final entry.
fn update_backoff_delay(step: usize) -> std::time::Duration {
    std::time::Duration::from_secs(UPDATE_BACKOFF_SECS[step.min(UPDATE_BACKOFF_SECS.len() - 1)])
}

/// UpdateStatus watch. Unlike the other standing watches, the update strip is
/// advisory: a missing or prematurely closed stream must never surface a
/// user-facing error or churn the IPC every 2s forever. On 0.1.0 local-only
/// runtimes had no updater, so the generic watch's flat-2s resubscribe loop
/// spun forever; this one backs off (capped exponential) and keeps the last
/// valid frame on screen while it is unavailable.
fn spawn_update_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut backoff_step = 0usize;
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::UPDATE_STATUS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "update status unavailable; retrying");
                    let delay = update_backoff_delay(backoff_step);
                    if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                        backoff_step += 1;
                    }
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(delay).await;
                    continue;
                }
            };
            let mut frames = 0usize;
            while let Some(value) = rx.recv().await {
                let parsed: cypher_update::UpdateStatus = match serde_json::from_value(value) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        tracing::warn!(error = %err, "dropping malformed update frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    state.apply_update(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                frames += 1;
            }
            // Stream ended (engine restart, RPC drop). A stream that delivered a
            // valid frame resets the backoff; one that closed prematurely keeps
            // backing off so a broken runtime cannot churn every 2s.
            tracing::debug!("update status stream ended; retrying");
            if frames > 0 {
                backoff_step = 0;
            }
            let delay = update_backoff_delay(backoff_step);
            if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                backoff_step += 1;
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(delay).await;
        }
    })
}

/// Pi/package update watch: same advisory backoff discipline as the Cypher
/// release stream. The last valid frame remains visible across engine
/// reconnects so an available update never flickers away.
fn spawn_pi_update_watch(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut backoff_step = 0usize;
        loop {
            let mut rx = match handle
                .client()
                .subscribe(methods::PI_UPDATE_STATUS, serde_json::json!({}))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::debug!(error = %err, "Pi update status unavailable; retrying");
                    let delay = update_backoff_delay(backoff_step);
                    if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                        backoff_step += 1;
                    }
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(delay).await;
                    continue;
                }
            };
            let mut frames = 0usize;
            while let Some(value) = rx.recv().await {
                let parsed: cypher_engine::pi_packages::PiUpdateStatus =
                    match serde_json::from_value(value) {
                        Ok(parsed) => parsed,
                        Err(err) => {
                            tracing::warn!(error = %err, "dropping malformed Pi update frame");
                            continue;
                        }
                    };
                let alive = this.update(cx, |state, cx| {
                    state.apply_pi_update(parsed);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                frames += 1;
            }
            tracing::debug!("Pi update status stream ended; retrying");
            if frames > 0 {
                backoff_step = 0;
            }
            let delay = update_backoff_delay(backoff_step);
            if backoff_step < UPDATE_BACKOFF_SECS.len() - 1 {
                backoff_step += 1;
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(delay).await;
        }
    })
}

/// Best-effort `LocalDevice` probe: fills `local_device_id` for the "This
/// device" badge. Engines that don't serve the method leave it `None`.
fn spawn_local_device_probe(cx: &mut Context<AppState>, handle: EngineHandle) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let Ok(value) = handle
            .client()
            .call("LocalDevice", serde_json::json!({}))
            .await
        else {
            tracing::debug!("LocalDevice unavailable; skipping this-device badge");
            return;
        };
        let id = value
            .get("id")
            .or_else(|| value.get("deviceId"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(id) = id {
            this.update(cx, |state, cx| {
                state.local_device_id = Some(id);
                cx.notify();
            })
            .ok();
        }
    })
}

fn spawn_transcript_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        // Outer loop: a delta desync (missed frame) resubscribes immediately
        // and the fresh stream's opening reset heals the copy; a subscribe
        // failure, malformed frame, or stream end retries on a delay. Every
        // path re-enters the loop — a return here freezes the transcript
        // with no banner and no heal short of an app restart (this watch and
        // its engine-side room are the ONLY transcript delivery path). The
        // task itself is dropped by select_chat/apply_chats when the chat is
        // deselected or deleted, so retrying can't outlive relevance.
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_MESSAGES, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "transcript watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let frame: TranscriptFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(err) => {
                        // Schema skew (a newer peer's entry shape arriving
                        // through sync): a skipped frame is a silently stale
                        // copy, so resubscribe for a fresh reset — delayed,
                        // in case the reset itself is what can't parse.
                        tracing::warn!(error = %err, "malformed transcript frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let mut desync = false;
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str()) {
                        if let Err(err) = state.apply_transcript_frame(frame) {
                            tracing::warn!(%chat_id, error = %err, "resubscribing transcript");
                            desync = true;
                        }
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    continue 'resubscribe;
                }
            }
            // Stream ended: engine restart, RPC drop, or chat purge. Retry;
            // the purge case is cleaned up by apply_chats dropping this task.
            tracing::debug!(%chat_id, "transcript stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchDocCommands`: the selected chat's durable command ledger — current
/// value first, then re-sent on every doc change. The UI projects Queued /
/// Retrying / Failed from this, so a Rejected or Expired command is visible
/// even when no session row ever reflects it (the host writes nothing to the
/// transcript for a refused message). Same resubscribe discipline as
/// [`spawn_transcript_watch`]; dropped by `select_chat`/`apply_chats` with
/// the chat.
fn spawn_commands_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let params = serde_json::json!({ "chatId": chat_id });
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_COMMANDS, params)
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "commands watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let commands: Vec<SessionCommandEntry> = match serde_json::from_value(value) {
                    Ok(commands) => commands,
                    Err(err) => {
                        // Schema skew — resubscribe for a fresh frame.
                        tracing::warn!(error = %err, "malformed commands frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let alive = this.update(cx, |state, cx| {
                    // Guard against a stale pump racing a newer selection.
                    if state.selected_chat.as_deref() == Some(chat_id.as_str()) {
                        state.apply_commands(commands);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
            // Stream ended: engine restart, RPC drop, or chat purge. Retry;
            // the purge case is cleaned up by apply_chats dropping this task.
            tracing::debug!(%chat_id, "commands stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchDocMessages` for a Side Chat fork: identical to [`spawn_transcript_watch`]
/// but carries `targetDeviceId` (the side chat is owned by the parent's host
/// device, which may differ from the connected engine's), and the fork's
/// selection never changes so the guard is trivially true. The task dies with
/// the fork (panel close drops the entity).
fn spawn_fork_transcript_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    chat_id: String,
    target_device_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        'resubscribe: loop {
            let mut params = serde_json::Map::new();
            params.insert("chatId".into(), serde_json::Value::String(chat_id.clone()));
            if let Some(local) = this.update(cx, |s, _| s.local_device_id.clone()).ok().flatten()
                && target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target_device_id.clone()),
                );
            }
            let mut rx = match handle
                .client()
                .subscribe(methods::WATCH_DOC_MESSAGES, serde_json::Value::Object(params))
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%chat_id, error = %err, "side chat transcript watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue 'resubscribe;
                }
            };
            while let Some(value) = rx.recv().await {
                let frame: TranscriptFrame = match serde_json::from_value(value) {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed side chat transcript frame; resubscribing");
                        cx.background_executor().timer(RETRY_DELAY).await;
                        continue 'resubscribe;
                    }
                };
                let mut desync = false;
                let alive = this.update(cx, |s, cx| {
                    if let Err(err) = s.apply_transcript_frame(frame) {
                        tracing::warn!(%chat_id, error = %err, "side chat transcript desync");
                        desync = true;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                if desync {
                    continue 'resubscribe;
                }
            }
            tracing::debug!(%chat_id, "side chat transcript stream ended; resubscribing");
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// `WatchSideChatStatus`: the private per-chat status stream, projected into
/// the fork's `sessions` via [`AppState::apply_side_chat_status`] (`null`
/// until the first transition or after dispose). The stream ends at
/// promotion/dispose; retrying after a clean end would hang, so a closed
/// stream simply stops the fork's status updates (the panel is usually gone
/// by then anyway).
fn spawn_side_chat_status_watch(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    side_chat_id: String,
    target_device_id: String,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        loop {
            let mut params = serde_json::Map::new();
            params.insert(
                "sideChatId".into(),
                serde_json::Value::String(side_chat_id.clone()),
            );
            if let Some(local) = this.update(cx, |s, _| s.local_device_id.clone()).ok().flatten()
                && target_device_id != local
            {
                params.insert(
                    "targetDeviceId".into(),
                    serde_json::Value::String(target_device_id.clone()),
                );
            }
            let mut rx = match handle
                .client()
                .subscribe(
                    methods::WATCH_SIDE_CHAT_STATUS,
                    serde_json::Value::Object(params),
                )
                .await
            {
                Ok(rx) => rx,
                Err(err) => {
                    tracing::warn!(%side_chat_id, error = %err, "side chat status watch failed; retrying");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    cx.background_executor().timer(RETRY_DELAY).await;
                    continue;
                }
            };
            while let Some(value) = rx.recv().await {
                if value.is_null() {
                    // First frame / after dispose: no session yet.
                    if this
                        .update(cx, |s, cx| {
                            s.sessions.retain(|s| s.chat_id != side_chat_id);
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let status: SideChatStatus = match serde_json::from_value(value) {
                    Ok(status) => status,
                    Err(err) => {
                        tracing::warn!(error = %err, "malformed side chat status frame");
                        continue;
                    }
                };
                let alive = this.update(cx, |s, cx| {
                    s.apply_side_chat_status(status.clone(), &target_device_id);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            // Stream ended: after dispose or promotion the panel is normally
            // already gone; if it somehow outlived the chat, retry.
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(RETRY_DELAY).await;
        }
    })
}

/// The synthetic `Chat` row a Side Chat fork selects (pure — testable without
/// a panel): the side chat inherits the parent's working context
/// (device/space/cwd/branch/checkout/config) so the reused Transcript/Composer
/// read the right values, but is its OWN row (the engine holds the real temp
/// in memory; there is no workspace row until promotion).
fn side_chat_synthetic_row(parent: &Chat, side_chat_id: &str, target_device_id: &str) -> Chat {
    Chat {
        pinned: false,
        id: side_chat_id.to_string(),
        device_id: target_device_id.to_string(),
        title: None,
        archived: false,
        cwd: parent.cwd.clone(),
        branch: parent.branch.clone(),
        checkout_id: parent.checkout_id.clone(),
        config: parent.config.clone(),
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::Utc::now(),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: parent.space_id.clone(),
        last_seen_at: None,
        room_gen: Some(2),
        child: None,
    }
}

#[cfg(test)]
mod tests;

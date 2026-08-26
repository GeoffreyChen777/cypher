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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gpui::{App, AppContext, Context, Entity, Task};
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
use cypher_rpc::{RpcClient, RpcError, RpcReply, RpcService, connect_ws, memory_client, methods};

// ---------------------------------------------------------------------------
// Engine handle
// ---------------------------------------------------------------------------

/// Everything needed to reach (or start) an engine.
#[derive(Debug, Clone)]
pub struct EngineBootConfig {
    /// Data directory for the embedded engine (`~/.cypher`).
    pub data_dir: PathBuf,
    /// Localhost IPC port to probe / serve.
    pub ipc_port: u16,
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
    boot_task: tokio::task::JoinHandle<()>,
    refresh_task: tokio::task::JoinHandle<()>,
    /// Serves this engine to other viewports over the IPC port. `None` when the
    /// port was already taken — the window still works over its own transport.
    ipc_task: Option<tokio::task::JoinHandle<()>>,
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
        self.boot_task.abort();
        // Stop accepting first: a viewport must not connect midway through the
        // drain and queue work against stores that are closing.
        if let Some(ipc) = &self.ipc_task {
            ipc.abort();
        }
        if let Some(runtime) = self.runtime.lock().await.take() {
            runtime.shutdown().await;
        }
        self.refresh_task.abort();
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

/// External daemon over `ws://127.0.0.1:{port}`.
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
    /// Probe the IPC port and connect (daemon listening) or embed (nothing there).
    /// Must run on the tokio runtime (`Tokio::spawn`): both transports spawn
    /// tokio tasks.
    pub async fn bootstrap(config: EngineBootConfig) -> anyhow::Result<EngineHandle> {
        // Invariant: at most one bootstrap in this process runs probe+embed at
        // a time. The winner binds the deferred IPC listener before releasing
        // the gate, so a concurrent viewport's probe finds it and attaches as
        // Remote instead of racing it for the data dir.
        static BOOTSTRAP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _gate = BOOTSTRAP_GATE.lock().await;

        if let Some(handle) = Self::attach_to_daemon(config.ipc_port).await {
            return Ok(handle);
        }

        tracing::info!(data_dir = %config.data_dir.display(), "no daemon on port; embedding engine");
        let engine_config = EngineConfig {
            data_dir: config.data_dir,
            edge_url: config.edge_url,
            edge_token: config.edge_token,
            ipc_port: config.ipc_port,
            default_harness: config.default_harness,
            org_id: config.org_id,
            workos_client_id: config.workos_client_id,
        };

        // Own the data dir before opening anything under it or binding IPC —
        // the lock, not the port bind, is the ownership decision. A failed
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
                    if let Some(handle) = Self::attach_to_daemon(engine_config.ipc_port).await {
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

        // Serve the same service on the IPC port so a terminal viewport can
        // attach to this window's engine with no setup. Deliberately the
        // *deferred* service, not the assembled one: a viewport that connects
        // during cloud onboarding gets EngineInfo and AuthRpc immediately, and
        // its data subscriptions wait exactly as this window's do.
        //
        // Best-effort — losing the bind race with another engine costs other
        // viewports, not this one.
        let ipc_task = match cypher_engine::serve_ipc(engine_config.ipc_port, service).await {
            Ok(task) => Some(task),
            Err(err) => {
                tracing::warn!(
                    port = engine_config.ipc_port,
                    error = %err,
                    "IPC port unavailable; other viewports cannot attach to this window"
                );
                None
            }
        };
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
                boot_task,
                refresh_task,
                ipc_task,
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

    /// Probe the IPC port and, if a live engine answers, attach as a remote
    /// viewport. `None` means embed: nothing listening, a non-engine listener,
    /// or a listener without an identity.
    async fn attach_to_daemon(ipc_port: u16) -> Option<EngineHandle> {
        let url = format!("ws://127.0.0.1:{ipc_port}");
        let probe = tokio::time::timeout(
            std::time::Duration::from_millis(750),
            tokio::net::TcpStream::connect(("127.0.0.1", ipc_port)),
        )
        .await;
        if !matches!(probe, Ok(Ok(_))) {
            return None;
        }
        tracing::info!(%url, "engine daemon detected; connecting");
        match connect_ws(&url).await {
            Ok(client) => match query_engine_info(&client).await {
                Ok(engine_info) => {
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
                            // EngineReady was added after EngineInfo. An older daemon
                            // that does not expose the barrier is already assembled.
                            Err(RpcError::Failed(message))
                                if message
                                    == format!("unknown method: {}", methods::ENGINE_READY) =>
                            {
                                DeferredEngineState::Ready
                            }
                            Err(err) => DeferredEngineState::Failed(err.to_string()),
                        };
                        state_tx.send_replace(state);
                    });
                    Some(EngineHandle {
                        inner: Arc::new(RemoteEngine {
                            client,
                            url,
                            lifecycle_task: tokio::sync::Mutex::new(Some(lifecycle_task)),
                        }),
                        engine_info,
                        deferred_state: Some(state_rx),
                    })
                }
                Err(err) => {
                    tracing::warn!(
                        %url,
                        error = %err,
                        "listener did not provide engine identity; embedding instead"
                    );
                    None
                }
            },
            // Something is on the port but it is not an engine (or it is
            // wedged). Fall through and embed: a stranger holding 27654
            // should cost other viewports, not this window.
            Err(err) => {
                tracing::warn!(%url, error = %err, "not an engine; embedding instead");
                None
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
    match client
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
    {
        Ok(info) => Ok(info),
        Err(RpcError::Failed(message))
            if message == format!("unknown method: {}", methods::ENGINE_INFO) =>
        {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct LocalDevice {
                device_id: String,
            }
            let legacy: LocalDevice = client
                .call_as(methods::LOCAL_DEVICE, serde_json::json!({}))
                .await?;
            Ok(EngineInfo {
                device_id: legacy.device_id,
                workspace_scope: WorkspaceScope::Synced,
            })
        }
        Err(err) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// Pure state + reducers
// ---------------------------------------------------------------------------

// The frontend-agnostic derivations (sort orders, staleness gating, sidebar
// grouping, the boot gate, relative times) live in `cypher_proto::view`, pure
// and with their own test suite. Re-exported here because every call site in
// this crate reads them as `state::…`.
pub use cypher_proto::view::{
    ChatGroup, ConnectionStatus, GatePhase, Indicator, SESSION_STALE_MS, attention_rank,
    chat_location, display_status, effective_indicator, format_time_ago, gate_phase, group_chats,
    parse_auth_state, project_label, sort_active, sort_chats, sort_spaces, sort_tabs,
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

/// Workspace names must be non-empty (trimmed) and reasonably short.
pub fn org_name_valid(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= 64
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
    pub message_id: String,
    pub prompt: String,
    pub resolution: Option<String>,
    pub sent_at: Option<i64>,
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
            message_id: message_id.to_string(),
            prompt,
            resolution: command.resolution.clone(),
            sent_at: command.sent_at,
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
    /// Joined transcript of the selected chat (continuations folded engine-side).
    pub transcript: Vec<SessionMessageEntry>,
    /// Durable command ledger of the selected chat (WatchDocCommands): the
    /// source of truth the UI projects Queued/Failed/Retrying from.
    commands: Vec<SessionCommandEntry>,
    /// Optimistic user echoes per chat id, shown until the doc frame carrying
    /// the same message id arrives (client-minted ids make dedup exact).
    echoes: HashMap<String, Vec<SessionMessageEntry>>,
    /// Send-in-flight overlay per chat id: a queued doc command the host
    /// hasn't executed yet (see [`Self::begin_pending_send`]).
    pending_sends: HashMap<String, PendingSend>,
    /// This engine's device id (best-effort `LocalDevice` probe; `None` until
    /// the engine serves it — views degrade gracefully).
    pub local_device_id: Option<String>,
    /// Latest `UpdateStatus` frame — drives the sidebar update strip.
    pub update: Option<cypher_update::UpdateStatus>,
    /// Data directory (`ui-settings.json`, `composer-defaults.json`); set at
    /// bootstrap so child views can persist small preference files.
    pub data_dir: Option<PathBuf>,
    engine: Option<EngineHandle>,
    watch_tasks: Vec<Task<()>>,
    transcript_task: Option<Task<()>>,
    commands_task: Option<Task<()>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// What kind of sidebar card a group is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarGroupKind {
    /// A live `Space` — has a project context menu; empty spaces included.
    Space,
    /// Project-less (`space_id = None`) chats of one device.
    NoProject,
    /// Chats whose `space_id` names a missing space.
    Unavailable,
}

/// One card of the project-grouped sidebar, produced by
/// [`AppState::sidebar_groups`]. Synthetic cards (No project / Unavailable
/// project) carry no `space_id` and therefore no project context menu.
#[derive(Debug)]
pub struct SidebarGroup<'a> {
    /// Stable key: `s:<space id>` live space, `np:<device id>` no-project,
    /// `u:<missing space id>` unavailable. Status changes never re-key a
    /// group, so cards keep their identity across renders.
    pub key: String,
    pub kind: SidebarGroupKind,
    /// Card title: the project's display name, "No project", or
    /// "Unavailable project".
    pub title: String,
    /// Folder path for live spaces (muted truncated in the header); `None`
    /// for synthetic cards.
    pub path: Option<String>,
    /// Host device name.
    pub device: String,
    /// Host offline (live spaces only; synthetic cards read as online).
    pub offline: bool,
    /// The space id for live-space cards (the project context menu target).
    pub space_id: Option<&'a str>,
    /// The card's chats in overview recency order (empty for quiet spaces).
    pub chats: Vec<(ChatIndicator, &'a Chat)>,
}

/// A temporary Side Chat's forked state (round 21 refactor): a SECONDARY
/// [`AppState`] entity per panel that shares the main state's [`EngineHandle`]
/// but never mutates the main selection. It owns a synthetic selected `Chat`
/// row inheriting the parent's device/space/cwd/branch/checkout/config, a
/// targeted `WatchDocMessages` transcript watch, and the private
/// `WatchSideChatStatus` projected into its `sessions` — so the EXISTING
/// `Transcript` / `Composer` components (which read `selected_chat`,
/// `transcript`, `pending_echoes` and `sessions`) work unchanged.
///
/// No normal `WatchChats`/`WatchSessions`/`WatchSpaces`/`WatchDevices` watches
/// run in the fork — they would replace the synthetic row/list state. The
/// remote `targetDeviceId` stays authoritative on the two watches and on every
/// side-chat RPC.
pub struct SideChatContext {
    pub state: Entity<AppState>,
    /// The chat the side chat was opened from — its host device owns the
    /// side chat and the promoted row inherits its working context.
    pub parent_chat_id: String,
    /// The engine-hosted temporary chat id (== the promoted row's id).
    pub side_chat_id: String,
    /// The device hosting the side chat — every side-chat RPC carries this
    /// as `targetDeviceId` when it differs from the connected engine's.
    pub target_device_id: String,
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
            selected_device: None,
            selected_chat: None,
            transcript: Vec::new(),
            commands: Vec::new(),
            echoes: HashMap::new(),
            pending_sends: HashMap::new(),
            local_device_id: None,
            update: None,
            data_dir: None,
            engine: None,
            watch_tasks: Vec::new(),
            transcript_task: None,
            commands_task: None,
            auto_selected: false,
            chats_synced: false,
            spaces_synced: false,
        }
    }

    // ---- reducers (pure) ----

    pub fn apply_chats(&mut self, mut chats: Vec<Chat>) {
        sort_chats(&mut chats);
        self.chats = chats;
        self.chats_synced = true;
        if let Some(selected) = &self.selected_chat
            && !self.chats.iter().any(|c| &c.id == selected)
        {
            // Selected chat vanished (deleted elsewhere): drop selection + transcript.
            self.selected_chat = None;
            self.transcript.clear();
            self.commands.clear();
            self.transcript_task = None;
            self.commands_task = None;
        }
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
    /// the shared [`EngineHandle`] is cloned, never restarted.
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

    /// Optimistic insert for a promoted Side Chat (round 21): the engine has
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

    pub fn apply_transcript(&mut self, entries: Vec<SessionMessageEntry>) {
        // Doc frames supersede optimistic echoes carrying the same id.
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(echoes) = self.echoes.get_mut(chat_id)
        {
            echoes.retain(|echo| !entries.iter().any(|e| e.id == echo.id));
        }
        self.transcript = entries;
        self.ack_pending_send_from_transcript();
    }

    /// Apply a `WatchDocMessages` delta frame in place. `Err` = this copy has
    /// diverged; the watch task resubscribes for a fresh reset.
    pub fn apply_transcript_frame(
        &mut self,
        frame: TranscriptFrame,
    ) -> Result<(), TranscriptDesync> {
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
    }

    /// Drop an echo (send failed — the prompt returns to the draft).
    pub fn remove_echo(&mut self, chat_id: &str, message_id: &str) {
        if let Some(echoes) = self.echoes.get_mut(chat_id) {
            echoes.retain(|e| e.id != message_id);
        }
    }

    /// Composer send fired: overlay the chat as Working until the host writes
    /// the user message back into the transcript (or the TTL lapses). A remote
    /// send has no live session row until the host drains the queued command —
    /// that gap read as "no live run" and flashed the Completed dot, and any
    /// phantom Working→Idle edge in it rang the done-chime on send (user
    /// report 2026-08-05).
    pub fn begin_pending_send(&mut self, chat_id: &str, message_id: &str, now: DateTime<Utc>) {
        self.pending_sends.insert(
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
        if self
            .pending_sends
            .get(chat_id)
            .is_some_and(|p| p.message_id == message_id)
        {
            self.pending_sends.remove(chat_id);
        }
    }

    /// Is a send still in flight for this chat (unacked, inside the TTL)?
    pub fn send_pending(&self, chat_id: &str, now: DateTime<Utc>) -> bool {
        self.pending_sends.get(chat_id).is_some_and(|p| {
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
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(pending) = self.pending_sends.get(chat_id)
            && self.transcript.iter().any(|e| e.id == pending.message_id)
        {
            self.pending_sends.remove(chat_id);
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
        if let Some(chat_id) = self.selected_chat.as_deref()
            && let Some(pending) = self.pending_sends.get(chat_id)
            && command_send_status(&self.commands, &pending.message_id)
                == Some(CommandSendStatus::Failed)
        {
            self.pending_sends.remove(chat_id);
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

    // ---- queries ----

    /// Non-archived, NON-CHILD chats in sidebar order. Cypher child subagent
    /// chats (engine-owned `child: Some(..)` rows) are hidden from the root
    /// sidebar/session overview — they are reached only through the parent's
    /// Subagents inspector. [`Self::selected_chat_row`] and transcript
    /// subscriptions still work for a selected child (navigation selects it
    /// directly).
    pub fn visible_chats(&self) -> impl Iterator<Item = &Chat> {
        self.chats.iter().filter(|c| !c.archived && !c.is_child())
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
    pub fn spaces_sorted(&self) -> Vec<&Space> {
        let mut spaces: Vec<&Space> = self.spaces.iter().collect();
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

    /// The project-grouped sidebar: one card per live `Space` (empty spaces
    /// included, so project management stays reachable), plus synthetic
    /// cards for project-less chats ("No project", per device) and chats
    /// whose `space_id` names a missing space ("Unavailable project", keyed
    /// by the missing id). Groups with chats are ordered by their newest chat
    /// (the overview recency order, preserved inside each group); empty
    /// spaces are appended deterministically by display name / device / path
    /// / id. Status changes never reorder. Archived and child chats stay
    /// excluded. Pure — see the tests in [`mod tests`] for the exact rules.
    pub fn sidebar_groups(&self, now: DateTime<Utc>) -> Vec<SidebarGroup<'_>> {
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
            let (key, kind) = match chat.space_id.as_deref() {
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
            groups.push(SidebarGroup {
                key,
                kind,
                title,
                path,
                device,
                offline,
                space_id: space.map(|s| s.id.as_str()),
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
            .filter(|s| !live.contains(s.id.as_str()))
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
                offline: !self.device_online(&space.device_id, now),
                space_id: Some(space.id.as_str()),
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
        self.selected_device = None;
        self.selected_chat = None;
        self.auto_selected = false;
        self.chats_synced = false;
        self.spaces_synced = false;
        self.transcript.clear();
        self.commands.clear();
        self.echoes.clear();
        self.pending_sends.clear();
        self.local_device_id = None;
        self.update = None;
        cx.notify();
    }

    // ---- gpui glue ----

    /// Kick off (or retry) the engine bootstrap: probe → connect-or-embed on
    /// tokio, then attach subscriptions. Safe to call again after `Failed`.
    pub fn bootstrap(state: Entity<AppState>, config: EngineBootConfig, cx: &mut App) {
        let data_dir = config.data_dir.clone();
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
                Ok(handle) => s.attach_engine(handle, cx),
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
    fn attach_engine(&mut self, handle: EngineHandle, cx: &mut Context<Self>) {
        let engine_info = handle.engine_info();
        self.workspace_scope = Some(engine_info.workspace_scope);
        self.local_device_id = Some(engine_info.device_id.clone());
        self.engine = Some(handle.clone());
        let mut watch_tasks = Vec::with_capacity(8);
        if let Some(task) = spawn_deferred_engine_watch(cx, handle.clone()) {
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
            spawn_local_device_probe(cx, handle.clone()),
        ]);
        self.watch_tasks = watch_tasks;
        // EngineInfo is part of the attachment boundary: views must know which
        // data profile they reached before they are allowed to render Ready.
        self.connection = ConnectionStatus::Ready;
        // Re-subscribe the transcript if a chat was already selected (reconnect path).
        if let Some(chat_id) = self.selected_chat.clone() {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            self.commands_task = Some(spawn_commands_watch(cx, handle, chat_id));
        }
        cx.notify();
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
        self.transcript_task = None;
        self.commands_task = None;
        if let Some(id) = chat_id.as_deref() {
            // A chat implies its project (or the lack of one); `select_chat(None)`
            // (the new-session canvas) keeps the current project pick.
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
        if let (Some(chat_id), Some(handle)) = (chat_id, self.engine.clone()) {
            self.transcript_task =
                Some(spawn_transcript_watch(cx, handle.clone(), chat_id.clone()));
            self.commands_task = Some(spawn_commands_watch(cx, handle, chat_id));
        }
        cx.notify();
    }

    /// Select a project; the caller (shell) decides which chat to land on.
    /// `Some` clears a "Don't work in a project" opt-out and re-aims the
    /// device pick at the project's host; `None` IS that opt-out.
    pub fn select_space(&mut self, space_id: Option<String>, cx: &mut Context<Self>) {
        match &space_id {
            Some(id) => {
                self.no_project = false;
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

    /// Synced seen marker: only fires when the chat is currently unseen
    /// (idempotence — no mutate spam), stamps the local row optimistically so
    /// the LWW round-trip is invisible, and fire-and-forgets the mutate.
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

    pub fn mark_chat_seen(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) else {
            return;
        };
        if !chat.unseen() {
            return;
        }
        chat.last_seen_at = Some(Utc::now());
        cx.notify();
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
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use cypher_engine::{EngineCore, default_registry};
    // `SessionStatus` is only needed to build the fixtures below — the module
    // itself derives everything through `cypher_proto::view`.
    use cypher_proto::{SessionStatus, UserProfile};

    /// A localhost port that was just free (bind :0, read, drop).
    async fn free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    struct LegacyIdentityRpc;

    #[async_trait]
    impl RpcService for LegacyIdentityRpc {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::LOCAL_DEVICE => {
                    RpcReply::value(&serde_json::json!({ "deviceId": "legacy-device" }))
                }
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    struct DeferredIdentityRpc {
        engine_info: EngineInfo,
        state: tokio::sync::watch::Receiver<DeferredEngineState>,
    }

    #[async_trait]
    impl RpcService for DeferredIdentityRpc {
        async fn handle(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                methods::ENGINE_INFO => RpcReply::value(&self.engine_info),
                methods::ENGINE_READY => {
                    let mut state = self.state.clone();
                    wait_for_deferred_engine(&mut state)
                        .await
                        .map_err(RpcError::Failed)?;
                    RpcReply::value(&serde_json::json!({ "ready": true }))
                }
                other => Err(RpcError::UnknownMethod(other.into())),
            }
        }
    }

    #[tokio::test]
    async fn legacy_daemon_identity_falls_back_to_synced_scope() {
        let client = memory_client(Arc::new(LegacyIdentityRpc));

        let info = query_engine_info(&client).await.unwrap();

        assert_eq!(info.device_id, "legacy-device");
        assert_eq!(info.workspace_scope, WorkspaceScope::Synced);
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(info.workspace_scope),
                Some(&AuthState::SignedOut),
            ),
            GatePhase::SignIn
        );
    }

    #[tokio::test]
    async fn remote_viewport_treats_legacy_daemon_as_ready() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(cypher_rpc::serve_ws_listener(
            listener,
            Arc::new(LegacyIdentityRpc),
        ));
        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .expect("legacy daemon remains attachable");

        let mut deferred = handle
            .deferred_state()
            .expect("remote viewport tracks readiness");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_deferred_engine(&mut deferred),
        )
        .await
        .expect("legacy readiness fallback completes")
        .expect("unknown EngineReady means the old daemon is assembled");

        handle.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn bootstrap_embeds_engine_when_port_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: free_port().await,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None, // offline
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();
        assert_eq!(handle.mode(), EngineMode::InProcess);
        assert!(matches!(
            handle
                .deferred_state()
                .expect("embedded lifecycle")
                .borrow()
                .clone(),
            DeferredEngineState::Ready
        ));
        // Same protocol over the in-memory transport: a real engine answers.
        let harnesses = handle
            .client()
            .call(methods::LIST_HARNESSES, serde_json::json!({}))
            .await
            .unwrap();
        assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn local_only_runtime_serves_update_status() {
        // Regression (0.1.0): the release checker used to be attached only for
        // edge-enabled runtimes, so a fresh local-only profile had no Updater —
        // `UpdateStatus` errored, the UI's subscription closed instantly, and the
        // generic watch re-subscribed every 2s forever. The updater must exist
        // for every profile (release endpoints are public, updates are
        // device-local); only the token-change wake is edge-gated.
        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: free_port().await,
            // Unreachable — the 20s initial check is never reached inside this
            // test, so no real network happens.
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None, // offline
            org_id: None,
            workos_client_id: Some("client_test".into()), // signed out → Local
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();
        assert_eq!(handle.engine_info().workspace_scope, WorkspaceScope::Local);

        // The RPC is served by the real assembled engine (in-memory transport).
        let mut rx = handle
            .client()
            .subscribe(methods::UPDATE_STATUS, serde_json::json!({}))
            .await
            .expect("a local-only runtime must serve UpdateStatus");

        // Immediate initial frame: current version, no update yet.
        let initial = rx
            .recv()
            .await
            .expect("initial UpdateStatus frame must arrive immediately");
        let status: cypher_update::UpdateStatus = serde_json::from_value(initial).unwrap();
        assert_eq!(status.current_version, cypher_update::current_version());
        assert!(!status.update_available);

        // The stream stays open (no frame, no close) before the 20s check.
        let still_open =
            tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await;
        assert!(
            still_open.is_err(),
            "UpdateStatus stream must remain open, got: {still_open:?}"
        );

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn bootstrap_reports_local_assembly_failure_before_returning_a_handle() {
        let dir = tempfile::tempdir().unwrap();
        cypher_engine::EngineProfile::local(dir.path()).unwrap();
        std::fs::create_dir(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/local"), b"not a directory").unwrap();
        let port = free_port().await;

        let error = match EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: Some("client_test".into()),
            default_harness: HarnessId::Mock,
        })
        .await
        {
            Ok(handle) => {
                handle.shutdown().await;
                panic!("a corrupt local store must fail bootstrap")
            }
            Err(error) => error,
        };

        assert!(!format!("{error:#}").is_empty());
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "failed bootstrap must release the IPC listener"
        );
    }

    #[tokio::test]
    async fn deferred_engine_failure_remains_observable_after_early_attach() {
        let (state_tx, mut state_rx) = tokio::sync::watch::channel(DeferredEngineState::Waiting);
        state_tx.send_replace(DeferredEngineState::Failed("store failed".into()));

        assert_eq!(
            wait_for_deferred_engine(&mut state_rx).await,
            Err("store failed".into())
        );
    }

    #[tokio::test]
    async fn remote_viewport_observes_deferred_engine_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (state_tx, state_rx) = tokio::sync::watch::channel(DeferredEngineState::Waiting);
        let server = tokio::spawn(cypher_rpc::serve_ws_listener(
            listener,
            Arc::new(DeferredIdentityRpc {
                engine_info: EngineInfo {
                    device_id: "owner-device".into(),
                    workspace_scope: WorkspaceScope::Local,
                },
                state: state_rx,
            }),
        ));

        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .expect("second viewport attaches over IPC");
        assert!(matches!(handle.mode(), EngineMode::Remote { .. }));

        let mut deferred = handle
            .deferred_state()
            .expect("remote viewport tracks engine readiness");
        state_tx.send_replace(DeferredEngineState::Failed("store failed".into()));
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                wait_for_deferred_engine(&mut deferred),
            )
            .await
            .expect("remote readiness probe completes"),
            Err("store failed".into())
        );

        handle.shutdown().await;
        server.abort();
    }

    #[tokio::test]
    async fn an_embedded_engine_serves_the_ipc_port_for_other_viewports() {
        // The whole point of embedding-and-serving: a second viewport (the
        // terminal app) can attach to this window's engine with no setup, no
        // separate daemon, and no launch ordering.
        let dir = tempfile::tempdir().unwrap();
        let port = free_port().await;
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None, // offline
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();
        assert_eq!(handle.mode(), EngineMode::InProcess);

        // Attach the way an external viewport would, and speak the same protocol.
        let attached = connect_ws(&format!("ws://127.0.0.1:{port}"))
            .await
            .expect("a second viewport must be able to attach");
        let harnesses = attached
            .call(methods::LIST_HARNESSES, serde_json::json!({}))
            .await
            .unwrap();
        assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));

        // Shutting the window down stops accepting, so the next viewport
        // starts its own engine rather than talking to closing stores.
        handle.shutdown().await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err(),
            "the port must be released on shutdown"
        );
    }

    #[tokio::test]
    async fn concurrent_bootstraps_elect_one_embedded_engine() {
        // Two viewports of one app booting at once (the Local-switch restart
        // path): both used to probe a closed port, both embedded, and one lost
        // the data-dir lock. The bootstrap gate must elect exactly one owner
        // and turn the other into a plain remote attach.
        let dir = tempfile::tempdir().unwrap();
        let port = free_port().await;
        let config = EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None, // offline
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        };
        let (a, b) = tokio::join!(
            EngineHandle::bootstrap(config.clone()),
            EngineHandle::bootstrap(config.clone()),
        );
        let a = a.expect("first viewport boots");
        let b = b.expect("second viewport boots");

        let modes = [a.mode(), b.mode()];
        assert_eq!(
            modes
                .iter()
                .filter(|mode| **mode == EngineMode::InProcess)
                .count(),
            1,
            "exactly one viewport embeds: {modes:?}"
        );
        assert_eq!(
            modes
                .iter()
                .filter(|mode| matches!(mode, EngineMode::Remote { .. }))
                .count(),
            1,
            "the other attaches over IPC: {modes:?}"
        );

        for handle in [&a, &b] {
            let mut deferred = handle.deferred_state().expect("lifecycle tracked");
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                wait_for_deferred_engine(&mut deferred),
            )
            .await
            .expect("readiness resolves")
            .expect("both viewports reach Ready");
        }

        b.shutdown().await;
        a.shutdown().await;
    }

    #[tokio::test]
    async fn a_stranger_on_the_ipc_port_does_not_wedge_the_window() {
        // The port probe only proves *something* is listening. A process that
        // accepts TCP and never speaks WebSocket used to hang the dial forever;
        // now it times out and we embed instead, losing only the ability to
        // serve other viewports.
        let squatter = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = squatter.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .expect("a taken port must not fail the boot");
        assert_eq!(handle.mode(), EngineMode::InProcess);
        assert!(
            handle
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({}))
                .await
                .is_ok(),
            "the window still works over its own transport"
        );
        handle.shutdown().await;
        drop(squatter);
    }

    #[tokio::test]
    async fn production_bootstrap_opens_local_data_without_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: free_port().await,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: Some("client_test".into()),
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();

        assert_eq!(handle.engine_info().workspace_scope, WorkspaceScope::Local);
        let info: EngineInfo = handle
            .client()
            .call_as(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(info, *handle.engine_info());

        let mut auth = handle
            .client()
            .subscribe(methods::AUTH_STATUS, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            parse_auth_state(&auth.recv().await.unwrap()),
            Some(AuthState::SignedOut)
        );
        let harnesses = handle
            .client()
            .call(methods::LIST_HARNESSES, serde_json::json!({}))
            .await
            .expect("local data RPC is immediately available");
        assert!(harnesses.as_array().is_some_and(|items| !items.is_empty()));
        assert!(
            !dir.path().join("orgs/dev-org/dev-user").exists(),
            "production boot must not create dev-user data"
        );
        assert!(dir.path().join("profiles/local").is_dir());
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn engine_info_is_available_while_cloud_onboarding_is_deferred() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("session.json"),
            r#"{"refreshToken":"saved","user":{"id":"user_1","email":"u@example.com"}}"#,
        )
        .unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: dir.path().to_path_buf(),
            ipc_port: free_port().await,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: Some("client_test".into()),
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();

        assert!(matches!(
            handle
                .deferred_state()
                .expect("embedded lifecycle")
                .borrow()
                .clone(),
            DeferredEngineState::Waiting
        ));

        let info: EngineInfo = handle
            .client()
            .call_as(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .expect("EngineInfo bypasses deferred cloud stores");
        assert_eq!(info.workspace_scope, WorkspaceScope::Synced);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                handle
                    .client()
                    .call(methods::LIST_HARNESSES, serde_json::json!({})),
            )
            .await
            .is_err(),
            "cloud data waits for organization onboarding"
        );
        assert!(!dir.path().join("orgs").exists());
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn bootstrap_connects_when_daemon_is_listening() {
        // Stand in for `cypher headless`: an engine served over the WS IPC port.
        let daemon_dir = tempfile::tempdir().unwrap();
        let core = EngineCore::assemble(
            daemon_dir.path(),
            Arc::new(default_registry(daemon_dir.path().join("agent-sessions"))),
            HarnessId::Mock,
            None,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(cypher_rpc::serve_ws_listener(listener, core.rpc_service()));

        let ui_dir = tempfile::tempdir().unwrap();
        let handle = EngineHandle::bootstrap(EngineBootConfig {
            data_dir: ui_dir.path().to_path_buf(),
            ipc_port: port,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: HarnessId::Mock,
        })
        .await
        .unwrap();
        assert_eq!(
            handle.mode(),
            EngineMode::Remote {
                url: format!("ws://127.0.0.1:{port}")
            }
        );
        assert_eq!(
            handle.engine_info().workspace_scope,
            WorkspaceScope::Development
        );
        let harnesses = handle
            .client()
            .call(methods::LIST_HARNESSES, serde_json::json!({}))
            .await
            .unwrap();
        assert!(harnesses.as_array().is_some_and(|h| !h.is_empty()));
        assert!(matches!(
            handle
                .client()
                .call(methods::STOP_ENGINE, serde_json::json!({}))
                .await,
            Err(RpcError::Failed(message))
                if message == format!("unknown method: {}", methods::STOP_ENGINE)
        ));
    }

    fn chat(id: &str, created_min: i64, last_msg_min: Option<i64>) -> Chat {
        let base = DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
            .unwrap()
            .to_utc();
        Chat {
            id: id.into(),
            device_id: "dev".into(),
            title: None,
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            config: None,
            last_message_preview: None,
            last_message_at: last_msg_min.map(|m| base + TimeDelta::minutes(m)),
            created_at: base + TimeDelta::minutes(created_min),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            child: None,
        }
    }

    fn space(id: &str, device_id: &str, path: &str, created_min: i64) -> Space {
        let base = DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
            .unwrap()
            .to_utc();
        Space {
            id: id.into(),
            device_id: device_id.into(),
            path: path.into(),
            name: None,
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: base + TimeDelta::minutes(created_min),
        }
    }

    fn session(
        chat_id: &str,
        status: SessionStatus,
        updated_secs_ago: i64,
        now: DateTime<Utc>,
    ) -> Session {
        Session {
            chat_id: chat_id.into(),
            device_id: "dev".into(),
            status,
            started_at: None,
            updated_at: now - TimeDelta::seconds(updated_secs_ago),
            subagents: Vec::new(),
        }
    }

    fn user_entry(id: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: cypher_doc::MessageRole::User,
            parts: Vec::new(),
            created_at: 0,
            device_id: "dev".into(),
            status: None,
            continuation_of: None,
        }
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            platform: "macos".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
        }
    }

    #[test]
    fn local_workspace_hides_the_unknown_device_sentinel() {
        let mut state = AppState::new();
        state.workspace_scope = Some(WorkspaceScope::Local);
        state.local_device_id = Some("local".into());

        state.apply_devices(vec![
            device("local", "unknown-device"),
            device("remote", "unknown-device"),
        ]);

        assert_eq!(state.device_name("local"), Some("Local"));
        assert_eq!(state.device_name("remote"), Some("unknown-device"));

        state.apply_devices(vec![device("local", "José's MacBook Pro")]);
        assert_eq!(state.device_name("local"), Some("José's MacBook Pro"));
    }

    #[test]
    fn update_backoff_is_capped_and_restarts_from_zero() {
        // 2 → 4 → 8 → 16 → 30s cap.
        assert_eq!(update_backoff_delay(0), std::time::Duration::from_secs(2));
        assert_eq!(update_backoff_delay(1), std::time::Duration::from_secs(4));
        assert_eq!(update_backoff_delay(2), std::time::Duration::from_secs(8));
        assert_eq!(update_backoff_delay(3), std::time::Duration::from_secs(16));
        assert_eq!(update_backoff_delay(4), std::time::Duration::from_secs(30));
        // Any further step stays capped — a broken stream cannot spin faster.
        assert_eq!(
            update_backoff_delay(100),
            std::time::Duration::from_secs(30)
        );
        // A healthy stream resets the step, so the next retry is fast again.
        assert_eq!(update_backoff_delay(0), std::time::Duration::from_secs(2));
    }

    #[test]
    fn send_pending_overlays_working_until_ttl() {
        let now = Utc::now();
        let s_chat = chat("c", 0, Some(10)); // unseen, no session row
        let mut s = AppState::new();
        assert_eq!(s.display_status_for(&s_chat, now), ChatIndicator::Completed);
        assert_eq!(s.indicator_for("c", now), Indicator::None);
        s.begin_pending_send("c", "m1", now);
        assert_eq!(s.display_status_for(&s_chat, now), ChatIndicator::Working);
        assert_eq!(s.indicator_for("c", now), Indicator::Working);
        // Time-bounded: an offline host must not leave an eternal spinner.
        let later = now + TimeDelta::milliseconds(PENDING_SEND_TTL_MS + 1);
        assert_eq!(
            s.display_status_for(&s_chat, later),
            ChatIndicator::Completed
        );
        assert_eq!(s.indicator_for("c", later), Indicator::None);
    }

    #[test]
    fn send_pending_acked_when_the_host_writes_the_message_back() {
        let now = Utc::now();
        let mut s = AppState::new();
        s.selected_chat = Some("c".into());
        s.begin_pending_send("c", "m1", now);
        // A frame without the message keeps the overlay.
        s.apply_transcript(vec![user_entry("other")]);
        assert!(s.send_pending("c", now));
        // The host executed the command: our id comes back in the doc.
        s.apply_transcript(vec![user_entry("other"), user_entry("m1")]);
        assert!(!s.send_pending("c", now));
    }

    #[test]
    fn send_failure_cleanup_only_ends_its_own_overlay() {
        let now = Utc::now();
        let mut s = AppState::new();
        s.begin_pending_send("c", "m1", now);
        s.begin_pending_send("c", "m2", now); // quick resend superseded m1
        s.end_pending_send("c", "m1"); // m1's failure cleanup arrives late
        assert!(s.send_pending("c", now), "m2's overlay must survive");
        s.end_pending_send("c", "m2");
        assert!(!s.send_pending("c", now));
    }

    #[test]
    fn chats_sort_by_last_message_desc_with_created_fallback() {
        let mut chats = vec![
            chat("a", 0, Some(10)),
            chat("b", 5, None), // no messages → keys on created_at (+5min)
            chat("c", 1, Some(30)),
            chat("d", 40, None), // created after every message
        ];
        sort_chats(&mut chats);
        let order: Vec<&str> = chats.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }

    #[test]
    fn chat_sort_ties_are_deterministic() {
        let mut chats = vec![chat("z", 0, Some(10)), chat("a", 0, Some(10))];
        sort_chats(&mut chats);
        assert_eq!(chats[0].id, "a");
    }

    #[test]
    fn working_indicator_staleness() {
        let now = Utc::now();
        // Fresh working session shows.
        let fresh = session("c", SessionStatus::Working, 10, now);
        assert_eq!(effective_indicator(Some(&fresh), now), Indicator::Working);
        // Stale working session is suppressed — crashed backend, not eternal spinner.
        let stale = session("c", SessionStatus::Working, 46, now);
        assert_eq!(effective_indicator(Some(&stale), now), Indicator::None);
        // Exactly at the boundary still shows (strictly-older-than semantics).
        let edge = session("c", SessionStatus::Working, 45, now);
        assert_eq!(effective_indicator(Some(&edge), now), Indicator::Working);
        // Future timestamps (clock skew) count as fresh.
        let skewed = session("c", SessionStatus::Working, -30, now);
        assert_eq!(effective_indicator(Some(&skewed), now), Indicator::Working);
    }

    #[test]
    fn indicator_kinds() {
        let now = Utc::now();
        assert_eq!(effective_indicator(None, now), Indicator::None);
        let idle = session("c", SessionStatus::Idle, 0, now);
        assert_eq!(effective_indicator(Some(&idle), now), Indicator::None);
        // Errored is not staleness-gated: the error stays visible.
        let errored = session("c", SessionStatus::Errored, 600, now);
        assert_eq!(effective_indicator(Some(&errored), now), Indicator::Errored);
        let awaiting = session("c", SessionStatus::AwaitingInput, 5, now);
        assert_eq!(
            effective_indicator(Some(&awaiting), now),
            Indicator::AwaitingInput
        );
        let awaiting_stale = session("c", SessionStatus::AwaitingInput, 300, now);
        assert_eq!(
            effective_indicator(Some(&awaiting_stale), now),
            Indicator::None
        );
    }

    #[test]
    fn display_status_derivation() {
        let now = Utc::now();
        let mut c = chat("c", 0, Some(10));
        // Live states win regardless of seen.
        let working = session("c", SessionStatus::Working, 5, now);
        assert_eq!(
            display_status(&c, Some(&working), now),
            ChatIndicator::Working
        );
        let awaiting = session("c", SessionStatus::AwaitingInput, 5, now);
        assert_eq!(
            display_status(&c, Some(&awaiting), now),
            ChatIndicator::AwaitingInput
        );
        // Finished + unseen = Completed (no session row at all).
        assert_eq!(display_status(&c, None, now), ChatIndicator::Completed);
        // Idle session + unseen = Completed.
        let idle = session("c", SessionStatus::Idle, 5, now);
        assert_eq!(
            display_status(&c, Some(&idle), now),
            ChatIndicator::Completed
        );
        // Stale working session falls back to the seen check.
        let stale = session("c", SessionStatus::Working, 300, now);
        assert_eq!(
            display_status(&c, Some(&stale), now),
            ChatIndicator::Completed
        );
        // Seen after the last message = Idle.
        c.last_seen_at = c.last_message_at.map(|t| t + TimeDelta::minutes(1));
        assert_eq!(display_status(&c, Some(&idle), now), ChatIndicator::Idle);
        // Errored + unseen = Errored; seen clears it to Idle.
        let errored = session("c", SessionStatus::Errored, 600, now);
        assert_eq!(display_status(&c, Some(&errored), now), ChatIndicator::Idle);
        c.last_seen_at = None;
        assert_eq!(
            display_status(&c, Some(&errored), now),
            ChatIndicator::Errored
        );
        // No messages at all: nothing to see — Idle.
        let fresh = chat("f", 0, None);
        assert_eq!(display_status(&fresh, None, now), ChatIndicator::Idle);
    }

    #[test]
    fn active_list_sorts_by_recency_only_status_never_moves_rows() {
        let a = chat("a", 0, Some(10)); // Completed (older)
        let b = chat("b", 0, Some(20)); // Completed (newer)
        let c = chat("c", 0, Some(5)); // AwaitingInput
        let d = chat("d", 0, Some(1)); // Working
        let mut rows = vec![
            (ChatIndicator::Completed, &a),
            (ChatIndicator::Completed, &b),
            (ChatIndicator::AwaitingInput, &c),
            (ChatIndicator::Working, &d),
        ];
        sort_active(&mut rows);
        let order: Vec<&str> = rows.iter().map(|(_, c)| c.id.as_str()).collect();
        assert_eq!(order, ["b", "a", "c", "d"], "recency desc, status ignored");

        // Opening a completed session (completed → seen → idle) must NOT
        // change its position (user report: rows jumped under the pointer).
        let mut seen = vec![
            (ChatIndicator::Idle, &a),
            (ChatIndicator::Completed, &b),
            (ChatIndicator::AwaitingInput, &c),
            (ChatIndicator::Working, &d),
        ];
        sort_active(&mut seen);
        let order_after: Vec<&str> = seen.iter().map(|(_, c)| c.id.as_str()).collect();
        assert_eq!(order, order_after);
    }

    #[test]
    fn tabs_order_by_creation_not_activity() {
        let a = chat("a", 5, Some(100)); // created later, very active
        let b = chat("b", 1, Some(2));
        let mut tabs = vec![&a, &b];
        sort_tabs(&mut tabs);
        let order: Vec<&str> = tabs.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(order, ["b", "a"]);
    }

    #[test]
    fn apply_spaces_sorts_and_heals_selection() {
        let mut state = AppState::new();
        state.apply_spaces(vec![
            space("s2", "dev", "/b", 2),
            space("s1", "dev", "/a", 1),
        ]);
        let ids: Vec<&str> = state.spaces.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2"]);
        // First frame auto-selects the first space.
        assert_eq!(state.selected_space.as_deref(), Some("s1"));
        state.selected_space = Some("s2".into());
        // Vanished selection heals to the first space.
        state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
        assert_eq!(state.selected_space.as_deref(), Some("s1"));
        // No spaces at all: selection clears.
        state.apply_spaces(vec![]);
        assert_eq!(state.selected_space, None);
    }

    #[test]
    fn first_space_on_picked_device_is_deterministic() {
        let mut state = AppState::new();
        state.apply_spaces(vec![
            space("z", "laptop", "/z", 3),
            space("a", "phone", "/a", 1),
            space("m", "laptop", "/m", 2),
        ]);
        // Picked device wins, in display order ("laptop" spaces: m, z).
        state.selected_device = Some("laptop".into());
        assert_eq!(state.first_space_on_picked_device().as_deref(), Some("m"));
        // Unpicked device falls back to the local device.
        state.selected_device = None;
        state.local_device_id = Some("laptop".into());
        assert_eq!(state.first_space_on_picked_device().as_deref(), Some("m"));
        // Neither device matches: any space at all (display-first).
        state.local_device_id = Some("server".into());
        assert_eq!(state.first_space_on_picked_device().as_deref(), Some("a"));
        // No spaces: nothing to fall back to.
        state.apply_spaces(vec![]);
        assert_eq!(state.first_space_on_picked_device(), None);
    }

    #[test]
    fn selected_space_if_live_rejects_dangling_ids() {
        let mut state = AppState::new();
        state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
        assert_eq!(state.selected_space.as_deref(), Some("s1"));
        assert_eq!(state.selected_space_if_live().as_deref(), Some("s1"));
        // A dangling selection (project deleted elsewhere) is not "live".
        state.selected_space = Some("ghost".into());
        assert_eq!(state.selected_space_if_live(), None);
        // No selection at all is not "live" either.
        state.selected_space = None;
        assert_eq!(state.selected_space_if_live(), None);
    }

    #[test]
    fn chats_in_space_filters_and_orders() {
        let mut state = AppState::new();
        state.apply_spaces(vec![space("s1", "dev", "/a", 1)]);
        let mut in_space_new = chat("new", 5, None);
        in_space_new.space_id = Some("s1".into());
        let mut in_space_old = chat("old", 1, Some(50)); // active but created first
        in_space_old.space_id = Some("s1".into());
        let mut other = chat("other", 2, None);
        other.space_id = Some("s2".into());
        let mut archived = chat("gone", 0, None);
        archived.space_id = Some("s1".into());
        archived.archived = true;
        let dangling = chat("dangling", 3, None); // no space id
        state.apply_chats(vec![in_space_new, in_space_old, other, archived, dangling]);
        let ids: Vec<&str> = state
            .chats_in_space("s1")
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["old", "new"]);
        // The overview shows every live-space chat (idle included) PLUS
        // project-less chats (first-class since the project selectors);
        // chats of unknown spaces stay hidden. Completed ("old") outranks
        // idle ("new"/"dangling").
        let now = Utc::now();
        let overview: Vec<&str> = state
            .overview_chats(now)
            .iter()
            .map(|(_, c)| c.id.as_str())
            .collect();
        assert_eq!(overview, ["old", "new", "dangling"]);
    }

    #[test]
    fn sidebar_groups_order_by_newest_chat_and_keep_overview_order() {
        let mut state = AppState::new();
        state.apply_spaces(vec![
            space("s1", "dev-a", "/a", 1),
            space("s2", "dev-b", "/b", 2),
            space("s3", "dev-a", "/c", 3),
        ]);
        let mut a = chat("a", 0, Some(10)); // in s1
        a.space_id = Some("s1".into());
        let mut b = chat("b", 0, Some(20)); // in s2 (newest)
        b.space_id = Some("s2".into());
        let mut c = chat("c", 0, Some(5)); // in s3 (oldest)
        c.space_id = Some("s3".into());
        let mut d = chat("d", 1, Some(15)); // in s1, newer than a
        d.space_id = Some("s1".into());
        state.apply_chats(vec![a, b, c, d]);
        let now = Utc::now();
        let groups = state.sidebar_groups(now);
        // Groups ordered by their newest chat: s2 (b=20), s1 (d=15), s3 (c=5).
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["s:s2", "s:s1", "s:s3"]);
        // Chats inside a group retain the overview (recency) order.
        let s1: Vec<&str> = groups[1].chats.iter().map(|(_, c)| c.id.as_str()).collect();
        assert_eq!(s1, ["d", "a"]);
        // Stability: the same call yields the same keys.
        let again_groups = state.sidebar_groups(now);
        let again: Vec<&str> = again_groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(again, keys);
    }

    #[test]
    fn sidebar_groups_status_changes_never_reorder_cards() {
        let mut state = AppState::new();
        state.apply_spaces(vec![
            space("s1", "dev", "/a", 1),
            space("s2", "dev", "/b", 2),
        ]);
        let mut a = chat("a", 0, Some(10));
        a.space_id = Some("s1".into());
        let mut b = chat("b", 0, Some(20));
        b.space_id = Some("s2".into());
        state.apply_chats(vec![a, b]);
        let now = Utc::now();
        let before: Vec<String> = state
            .sidebar_groups(now)
            .iter()
            .map(|g| g.key.clone())
            .collect();
        assert_eq!(before, ["s:s2", "s:s1"]);
        // s1's chat turns Working — status must never move the card.
        state.apply_sessions(vec![session("a", SessionStatus::Working, 5, now)]);
        let after: Vec<String> = state
            .sidebar_groups(now)
            .iter()
            .map(|g| g.key.clone())
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn sidebar_groups_append_empty_spaces_deterministically() {
        let mut state = AppState::new();
        state.apply_spaces(vec![
            space("active", "dev", "/z", 1),
            space("empty-b", "dev-b", "/b", 2),
            space("empty-a", "dev-a", "/a", 3),
        ]);
        let mut c = chat("c", 0, Some(5));
        c.space_id = Some("active".into());
        state.apply_chats(vec![c]);
        let now = Utc::now();
        let groups = state.sidebar_groups(now);
        let summary: Vec<(&str, usize)> = groups
            .iter()
            .map(|g| (g.title.as_str(), g.chats.len()))
            .collect();
        // The active space leads; quiet spaces are appended deterministically
        // (display name "a" < "b"), never by recency noise.
        assert_eq!(summary, [("z", 1), ("a", 0), ("b", 0)]);
        // Stable across renders.
        let again_groups = state.sidebar_groups(now);
        let again: Vec<&str> = again_groups.iter().map(|g| g.title.as_str()).collect();
        let first: Vec<&str> = groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(again, first);
    }

    #[test]
    fn sidebar_groups_show_every_host_together_with_device_labels() {
        let mut state = AppState::new();
        state.devices = vec![device("dev-a", "MacBook"), device("dev-b", "Desktop")];
        state.apply_spaces(vec![
            space("s-a", "dev-a", "/a", 1),
            space("s-b", "dev-b", "/b", 2),
        ]);
        let now = Utc::now();
        let groups = state.sidebar_groups(now);
        let keys: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["s:s-a", "s:s-b"], "both hosts in the one sidebar");
        assert_eq!(groups[0].device, "MacBook");
        assert_eq!(groups[1].device, "Desktop");
        // Quiet spaces are still cards (project management stays reachable).
        assert!(groups.iter().all(|g| g.chats.is_empty()));
    }

    #[test]
    fn sidebar_groups_synthetic_no_project_and_unavailable() {
        let mut state = AppState::new();
        state.devices = vec![device("dev-b", "Laptop"), device("dev-c", "Tablet")];
        state.apply_spaces(vec![space("s1", "dev-a", "/a", 1)]);
        let mut in_space = chat("in", 0, Some(30));
        in_space.space_id = Some("s1".into());
        let mut no_project = chat("np", 0, Some(20));
        no_project.space_id = None;
        no_project.device_id = "dev-b".into();
        let mut dangling = chat("dang", 0, Some(10));
        dangling.space_id = Some("gone".into());
        dangling.device_id = "dev-c".into();
        state.apply_chats(vec![in_space, no_project, dangling]);
        let now = Utc::now();
        let groups = state.sidebar_groups(now);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].key, "s:s1");
        assert_eq!(groups[0].kind, SidebarGroupKind::Space);
        assert_eq!(groups[0].space_id, Some("s1"));
        assert_eq!(groups[1].key, "np:dev-b");
        assert_eq!(groups[1].kind, SidebarGroupKind::NoProject);
        assert_eq!(groups[1].title, "No project");
        assert_eq!(groups[1].device, "Laptop");
        assert_eq!(groups[1].space_id, None, "synthetic cards have no menu");
        assert_eq!(groups[2].key, "u:gone");
        assert_eq!(groups[2].kind, SidebarGroupKind::Unavailable);
        assert_eq!(groups[2].title, "Unavailable project");
        assert_eq!(groups[2].device, "Tablet");
        assert_eq!(groups[2].space_id, None);
        // Dangling-id chats are included (not dropped like the old overview).
        let dangling_chats: Vec<&str> =
            groups[2].chats.iter().map(|(_, c)| c.id.as_str()).collect();
        assert_eq!(dangling_chats, ["dang"]);
    }

    #[test]
    fn apply_chats_drops_vanished_selection() {
        let mut state = AppState::new();
        state.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
        state.selected_chat = Some("a".into());
        state.transcript = vec![];
        state.apply_chats(vec![chat("b", 1, None)]);
        assert_eq!(state.selected_chat, None);
        // Still-present selection survives.
        state.selected_chat = Some("b".into());
        state.apply_chats(vec![chat("b", 1, None), chat("c", 2, None)]);
        assert_eq!(state.selected_chat.as_deref(), Some("b"));
    }

    #[test]
    fn insert_chat_optimistic_inserts_sorts_and_is_idempotent() {
        // Round 21: a promoted Side Chat lands optimistically before the next
        // chats frame — inserted (sorted), never duplicated, and never
        // clobbering an authoritative row that already arrived.
        let mut state = AppState::new();
        state.apply_chats(vec![chat("old", 0, None), chat("new", 5, None)]);
        let mut promoted = chat("promoted", 9, None);
        promoted.space_id = Some("s1".into());
        state.insert_chat_optimistic(promoted.clone());
        assert_eq!(
            state
                .chats
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["promoted", "new", "old"],
            "inserted row participates in the sort (newest created first)"
        );
        // Idempotent: the same id is never inserted twice.
        state.insert_chat_optimistic(promoted);
        assert_eq!(state.chats.len(), 3);
        // An already-present id is untouched (authoritative frame won).
        state.insert_chat_optimistic(chat("old", 0, None));
        assert_eq!(state.chats.len(), 3);
    }

    #[test]
    fn apply_chat_config_stamps_the_row() {
        let mut state = AppState::new();
        state.apply_chats(vec![chat("a", 0, None), chat("b", 1, None)]);
        let config = cypher_proto::ChatConfig {
            harness: HarnessId::ClaudeCode,
            model: Some("claude-fable-5".into()),
            reasoning: Some(cypher_proto::ReasoningLevel::XHigh),
            model_options: serde_json::Map::new(),
            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
        };
        state.apply_chat_config("a", config.clone());
        assert_eq!(
            state.chats.iter().find(|c| c.id == "a").unwrap().config,
            Some(config)
        );
        assert!(
            state
                .chats
                .iter()
                .find(|c| c.id == "b")
                .unwrap()
                .config
                .is_none()
        );
        // Unknown chat: no-op, no panic.
        state.apply_chat_config(
            "missing",
            cypher_proto::ChatConfig {
                harness: HarnessId::ClaudeCode,
                model: None,
                reasoning: None,
                model_options: serde_json::Map::new(),
                sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
            },
        );
    }

    // ---- temporary Side Chat fork (round 21 refactor) ----

    #[test]
    fn side_chat_synthetic_row_inherits_parent_context() {
        // The fork's synthetic row carries the parent's device/space/cwd/
        // branch/checkout/config so the reused Transcript/Composer read the
        // inherited working context.
        let mut parent = chat("parent", 0, Some(10));
        parent.device_id = "remote-dev".into();
        parent.cwd = Some("/home/w/dev/cypher".into());
        parent.branch = Some("cypher/side".into());
        parent.checkout_id = Some("co-1".into());
        parent.space_id = Some("s1".into());
        parent.config = Some(cypher_proto::ChatConfig {
            harness: HarnessId::ClaudeCode,
            model: Some("claude-fable-5".into()),
            reasoning: Some(cypher_proto::ReasoningLevel::High),
            model_options: serde_json::Map::new(),
            sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
        });
        let row = side_chat_synthetic_row(&parent, "side-1", "remote-dev");
        assert_eq!(row.id, "side-1");
        assert_eq!(row.device_id, "remote-dev");
        assert_eq!(row.cwd.as_deref(), Some("/home/w/dev/cypher"));
        assert_eq!(row.branch.as_deref(), Some("cypher/side"));
        assert_eq!(row.checkout_id.as_deref(), Some("co-1"));
        assert_eq!(row.space_id.as_deref(), Some("s1"));
        assert_eq!(row.config, parent.config);
        // It is its OWN row — no public row exists until promotion.
        assert_ne!(row.id, parent.id);
        assert!(row.archived == false && row.last_message_at.is_none());
    }

    #[test]
    fn side_chat_status_projects_into_sessions_by_id() {
        // The private WatchSideChatStatus frames upsert into `sessions` so
        // the reused status logic (indicator_for / run_live) works.
        let mut state = AppState::new();
        let status =
            |s: cypher_proto::SessionStatus, at: chrono::DateTime<chrono::Utc>| -> SideChatStatus {
                SideChatStatus {
                    side_chat_id: "side-1".into(),
                    status: s,
                    started_at: Some(at),
                    updated_at: at,
                }
            };
        let t0 = chrono::Utc::now();
        state.apply_side_chat_status(
            status(cypher_proto::SessionStatus::Working, t0),
            "remote-dev",
        );
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].chat_id, "side-1");
        assert_eq!(state.sessions[0].device_id, "remote-dev");
        assert_eq!(
            state.sessions[0].status,
            cypher_proto::SessionStatus::Working
        );
        // A later frame upserts, never duplicates.
        state.apply_side_chat_status(
            status(
                cypher_proto::SessionStatus::Idle,
                t0 + chrono::TimeDelta::seconds(1),
            ),
            "remote-dev",
        );
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].status, cypher_proto::SessionStatus::Idle);
        // The fork's indicator reads the projected row (no public session).
        assert_eq!(
            state.indicator_for("side-1", t0 + chrono::TimeDelta::seconds(2)),
            Indicator::None
        );
    }

    #[test]
    fn visible_chats_filters_archived() {
        let mut state = AppState::new();
        let mut archived = chat("a", 0, Some(99));
        archived.archived = true;
        state.apply_chats(vec![archived, chat("b", 1, None)]);
        let visible: Vec<&str> = state.visible_chats().map(|c| c.id.as_str()).collect();
        assert_eq!(visible, ["b"]);
    }

    /// Cypher child subagent chats are hidden from the root sidebar/overview
    /// (`visible_chats` / `overview_chats`), yet remain selectable: a selected
    /// child still resolves through `selected_chat_row` (the Inspector
    /// navigation path) and survives `apply_chats` (it stays in `self.chats`).
    #[test]
    fn child_chats_hidden_from_root_but_selected_row_works() {
        let mut state = AppState::new();
        let parent = chat("parent", 0, Some(10));
        let mut child = chat("child-1", 1, Some(11));
        child.child = Some(cypher_proto::ChildChat {
            parent_chat_id: "parent".into(),
            parent_run_id: "run-1".into(),
            agent: "planner".into(),
            task: "Plan the panel".into(),
            mode: cypher_proto::SubagentRunMode::Async,
            tool_call_id: None,
            profile: cypher_proto::ChildAgentProfile {
                system_prompt: "You are the planner.".into(),
                tools: vec![],
                model: None,
                thinking: None,
            },
        });
        let mut archived = chat("archived", 2, None);
        archived.archived = true;
        state.apply_chats(vec![parent, child, archived]);

        // Root lists exclude both children and archived rows.
        let visible: Vec<&str> = state.visible_chats().map(|c| c.id.as_str()).collect();
        assert_eq!(visible, ["parent"], "child chat hidden from root");
        let overview: Vec<&str> = state
            .overview_chats(
                DateTime::parse_from_rfc3339("2026-07-19T12:20:00Z")
                    .unwrap()
                    .to_utc(),
            )
            .into_iter()
            .map(|(_, c)| c.id.as_str())
            .collect();
        assert_eq!(overview, ["parent"], "child chat hidden from overview");

        // A selected child still resolves (Inspector navigation target).
        state.selected_chat = Some("child-1".into());
        assert_eq!(
            state.selected_chat_row().map(|c| c.id.as_str()),
            Some("child-1")
        );
        assert!(state.selected_chat_row().is_some_and(|c| c.is_child()));
        // And a later chats frame keeps it (apply_chats only clears a
        // selection whose row vanished from self.chats entirely).
        state.apply_chats(state.chats.clone());
        assert_eq!(state.selected_chat.as_deref(), Some("child-1"));
    }

    #[test]
    fn echoes_show_until_doc_frame_confirms() {
        let mut state = AppState::new();
        state.selected_chat = Some("c1".into());
        let echo = SessionMessageEntry {
            id: "m1".into(),
            role: cypher_doc::MessageRole::User,
            parts: vec![],
            created_at: 0,
            device_id: "local".into(),
            status: None,
            continuation_of: None,
        };
        state.push_echo("c1", echo.clone());
        // Duplicate pushes dedupe.
        state.push_echo("c1", echo.clone());
        assert_eq!(state.pending_echoes().len(), 1);
        // Frames without the id keep the echo.
        state.apply_transcript(vec![]);
        assert_eq!(state.pending_echoes().len(), 1);
        // The confirming frame prunes it.
        state.apply_transcript(vec![SessionMessageEntry {
            id: "m1".into(),
            ..echo.clone()
        }]);
        assert!(state.pending_echoes().is_empty());
        // Failure path: explicit removal.
        state.push_echo(
            "c1",
            SessionMessageEntry {
                id: "m2".into(),
                ..echo.clone()
            },
        );
        state.remove_echo("c1", "m2");
        assert!(state.pending_echoes().is_empty());
        // Echoes are per chat.
        state.push_echo(
            "other",
            SessionMessageEntry {
                id: "m3".into(),
                ..echo
            },
        );
        assert!(state.pending_echoes().is_empty());
    }

    #[test]
    fn gate_phases() {
        let user = UserProfile {
            id: "u".into(),
            email: "w@example.com".into(),
            name: None,
            avatar_url: None,
        };
        assert_eq!(
            gate_phase(&ConnectionStatus::Connecting, None, None),
            GatePhase::Loading
        );
        assert_eq!(
            gate_phase(&ConnectionStatus::Failed("boom".into()), None, None),
            GatePhase::Failed("boom".into())
        );
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Local),
                Some(&AuthState::SignedOut),
            ),
            GatePhase::Ready
        );
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedOut),
            ),
            GatePhase::SignIn
        );
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::SignedIn {
                    user: user.clone(),
                    org_id: None
                })
            ),
            GatePhase::Ready
        );
        // No org yet → org gate.
        assert_eq!(
            gate_phase(
                &ConnectionStatus::Ready,
                Some(WorkspaceScope::Synced),
                Some(&AuthState::NeedsOrganization { user })
            ),
            GatePhase::OrgGate
        );
    }

    #[test]
    fn auth_changes_do_not_change_a_local_runtime_scope_or_watches() {
        let mut state = AppState::new();
        state.workspace_scope = Some(WorkspaceScope::Local);
        state.watch_tasks.push(Task::ready(()));

        state.apply_auth(AuthState::NeedsOrganization {
            user: UserProfile {
                id: "u".into(),
                email: "w@example.com".into(),
                name: None,
                avatar_url: None,
            },
        });
        assert_eq!(state.workspace_scope, Some(WorkspaceScope::Local));
        assert_eq!(state.watch_tasks.len(), 1);

        state.apply_auth(AuthState::SignedIn {
            user: UserProfile {
                id: "u".into(),
                email: "w@example.com".into(),
                name: None,
                avatar_url: None,
            },
            org_id: Some("org-1".into()),
        });
        assert_eq!(state.workspace_scope, Some(WorkspaceScope::Local));
        assert_eq!(state.watch_tasks.len(), 1);
    }

    #[test]
    fn auth_frames_parse_both_wire_shapes() {
        // Proto shape.
        let proto = serde_json::json!({ "state": "signedOut" });
        assert_eq!(parse_auth_state(&proto), Some(AuthState::SignedOut));
        // Engine shape (`_tag`, PascalCase, orgId).
        let engine = serde_json::json!({
            "_tag": "SignedIn",
            "user": { "id": "u1", "email": "w@example.com" },
            "orgId": "org-1",
        });
        let Some(AuthState::SignedIn { user, org_id }) = parse_auth_state(&engine) else {
            panic!("expected SignedIn");
        };
        assert_eq!(user.email, "w@example.com");
        assert_eq!(org_id.as_deref(), Some("org-1"));
        let needs = serde_json::json!({
            "_tag": "NeedsOrganization",
            "user": { "id": "u1", "email": "w@example.com", "name": "W" },
        });
        assert!(matches!(
            parse_auth_state(&needs),
            Some(AuthState::NeedsOrganization { .. })
        ));
        // Garbage → None (frame dropped, not a crash).
        assert_eq!(
            parse_auth_state(&serde_json::json!({ "_tag": "Wat" })),
            None
        );
        assert_eq!(parse_auth_state(&serde_json::json!(42)), None);
    }

    fn chat_with_cwd(id: &str, created_min: i64, cwd: Option<&str>) -> Chat {
        let mut c = chat(id, created_min, None);
        c.cwd = cwd.map(str::to_string);
        c
    }

    #[test]
    fn project_labels_from_cwd() {
        assert_eq!(project_label(Some("/home/w/dev/cypher")), "cypher");
        assert_eq!(project_label(Some("/home/w/dev/cypher/")), "cypher");
        assert_eq!(project_label(None), "No project");
        assert_eq!(project_label(Some("   ")), "No project");
        assert_eq!(project_label(Some("/")), "/");
    }

    #[test]
    fn grouped_sidebar_preserves_recency_order() {
        // Input is sidebar-sorted (most recent first).
        let chats = [
            chat_with_cwd("a", 9, Some("/dev/cypher")),
            chat_with_cwd("b", 8, Some("/dev/zed")),
            chat_with_cwd("c", 7, Some("/dev/cypher")),
            chat_with_cwd("d", 6, None),
        ];
        let groups = group_chats(chats.iter());
        let labels: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
        // Groups ordered by their most recent chat; rows keep order.
        assert_eq!(labels, ["cypher", "zed", "No project"]);
        let cypher_ids: Vec<&str> = groups[0].chats.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(cypher_ids, ["a", "c"]);
        assert!(group_chats(std::iter::empty()).is_empty());
    }

    #[test]
    fn relative_times_match_cypher_format() {
        let now = Utc::now();
        let ago = |secs: i64| now - chrono::Duration::seconds(secs);
        assert_eq!(format_time_ago(ago(0), now), "now");
        assert_eq!(format_time_ago(ago(59), now), "now");
        assert_eq!(format_time_ago(ago(60), now), "1m");
        assert_eq!(format_time_ago(ago(59 * 60), now), "59m");
        assert_eq!(format_time_ago(ago(60 * 60), now), "1h");
        assert_eq!(format_time_ago(ago(23 * 3600 + 3599), now), "23h");
        assert_eq!(format_time_ago(ago(24 * 3600), now), "1d");
        assert_eq!(format_time_ago(ago(6 * 86400), now), "6d");
        assert_eq!(format_time_ago(ago(7 * 86400), now), "1w");
        assert_eq!(format_time_ago(ago(30 * 86400), now), "4w");
        assert_eq!(format_time_ago(ago(35 * 86400), now), "1mo");
        assert_eq!(format_time_ago(ago(400 * 86400), now), "1y");
        // Clock skew (future timestamps) clamps to "now".
        assert_eq!(
            format_time_ago(now + chrono::Duration::hours(2), now),
            "now"
        );
    }

    #[test]
    fn chat_location_joins_project_and_branch() {
        let mut c = chat_with_cwd("x", 1, Some("/home/w/dev/soccertcg"));
        c.branch = Some("cypher/rebalance".into());
        assert_eq!(
            chat_location(&c).as_deref(),
            Some("soccertcg · cypher/rebalance")
        );
        c.branch = None;
        assert_eq!(chat_location(&c).as_deref(), Some("soccertcg"));
        c.cwd = None;
        c.branch = Some("main".into());
        assert_eq!(chat_location(&c).as_deref(), Some("main"));
        c.branch = Some("   ".into());
        assert_eq!(chat_location(&c), None);
        c.branch = None;
        assert_eq!(chat_location(&c), None);
    }

    #[test]
    fn org_gate_reducers() {
        assert!(org_name_valid("Acme"));
        assert!(org_name_valid("  padded  "));
        assert!(!org_name_valid(""));
        assert!(!org_name_valid("   "));
        assert!(!org_name_valid(&"x".repeat(65)));

        let rows = parse_orgs(&serde_json::json!({ "orgs": [
            { "id": "m2", "organizationId": "o2", "name": "beta" },
            { "id": "m1", "organizationId": "o1", "name": "Alpha" },
            { "id": "m3", "organizationId": "o1", "name": "Alpha" },
        ]}));
        assert_eq!(rows.len(), 3);
        let sorted = sort_memberships(rows);
        let names: Vec<&str> = sorted.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            ["Alpha", "beta"],
            "case-insensitive sort + dedupe by org id"
        );
        // Bare-array replies parse too; garbage yields empty.
        assert_eq!(
            parse_orgs(&serde_json::json!([{ "id": "m", "organizationId": "o", "name": "n" }]))
                .len(),
            1
        );
        assert!(parse_orgs(&serde_json::json!("nope")).is_empty());
    }

    fn run_command(
        id: &str,
        message_id: &str,
        issued_at: i64,
        status: SessionCommandStatus,
    ) -> SessionCommandEntry {
        SessionCommandEntry {
            id: id.into(),
            payload: SessionCommandPayload::Run {
                request: cypher_proto::RunRequest {
                    prompt: format!("prompt-{id}"),
                    harness: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    attachments: Vec::new(),
                    pending_attachments: Vec::new(),
                    resume: None,
                    worktree: None,
                },
                message_id: message_id.into(),
                agent_prompt: None,
            },
            issued_by: "dev".into(),
            issued_at,
            based_on: None,
            expires_at: None,
            status,
            resolution: Some("nope".into()),
            sent_at: Some(issued_at),
        }
    }

    #[test]
    fn command_send_status_projects_durable_truth() {
        // No command for the message: no status.
        assert_eq!(command_send_status(&[], "m1"), None);
        // Pending first attempt = Queued.
        let queued = run_command("c1", "m1", 1, SessionCommandStatus::Pending);
        assert_eq!(
            command_send_status(&[queued.clone()], "m1"),
            Some(CommandSendStatus::Queued)
        );
        // Applied is resolved — nothing for the UI to show.
        let applied = run_command("c1", "m1", 1, SessionCommandStatus::Applied);
        assert_eq!(command_send_status(&[applied], "m1"), None);
        // Rejected / Expired = Failed.
        let rejected = run_command("c1", "m1", 1, SessionCommandStatus::Rejected);
        assert_eq!(
            command_send_status(&[rejected.clone()], "m1"),
            Some(CommandSendStatus::Failed)
        );
        let expired = run_command("c1", "m1", 1, SessionCommandStatus::Expired);
        assert_eq!(
            command_send_status(&[expired], "m1"),
            Some(CommandSendStatus::Failed)
        );
        // A live pending attempt AFTER a failure = Retrying.
        let retry = run_command("c2", "m1", 2, SessionCommandStatus::Pending);
        assert_eq!(
            command_send_status(&[rejected, retry], "m1"),
            Some(CommandSendStatus::Retrying)
        );
    }

    #[test]
    fn failed_commands_lists_only_retryable_latest_failures() {
        let rejected = run_command("c1", "m1", 1, SessionCommandStatus::Rejected);
        let retry_pending = run_command("c2", "m1", 2, SessionCommandStatus::Pending);
        let rejected_again = run_command("c3", "m1", 3, SessionCommandStatus::Rejected);
        let expired_other = run_command("c4", "m2", 4, SessionCommandStatus::Expired);
        let applied_other = run_command("c5", "m3", 5, SessionCommandStatus::Applied);
        let commands = vec![
            rejected,
            retry_pending,
            rejected_again,
            expired_other,
            applied_other,
        ];
        let failed = failed_commands(&commands);
        // m1: the retry is in flight (skip); m3 applied (skip); m2: expired.
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].command_id, "c4");
        assert_eq!(failed[0].message_id, "m2");
        assert_eq!(failed[0].resolution.as_deref(), Some("nope"));
        assert_eq!(failed[0].sent_at, Some(4));

        // After the retry fails again, the LATEST failed attempt is the target.
        let commands = vec![
            run_command("c1", "m1", 1, SessionCommandStatus::Rejected),
            run_command("c2", "m1", 2, SessionCommandStatus::Rejected),
        ];
        let failed = failed_commands(&commands);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].command_id, "c2");
        assert_eq!(failed[0].prompt, "prompt-c2");
    }

    #[test]
    fn failed_command_lifts_echo_veil_and_ends_send_overlay() {
        let now = Utc::now();
        let mut s = AppState::new();
        s.selected_chat = Some("c".into());
        s.begin_pending_send("c", "m1", now);
        // A fresh pending command keeps the echo pending + the overlay.
        s.apply_commands(vec![run_command(
            "c1",
            "m1",
            1,
            SessionCommandStatus::Pending,
        )]);
        assert!(s.echo_pending("m1"));
        assert!(s.send_pending("c", now));
        // The durable Rejected ends both: full-opacity echo + truthful dot.
        s.apply_commands(vec![run_command(
            "c1",
            "m1",
            1,
            SessionCommandStatus::Rejected,
        )]);
        assert!(!s.echo_pending("m1"));
        assert!(!s.send_pending("c", now));
        assert_eq!(s.failed_commands().len(), 1);
        // A retry in flight re-arms the echo veil (the message is sending again).
        s.apply_commands(vec![
            run_command("c1", "m1", 1, SessionCommandStatus::Rejected),
            run_command("c2", "m1", 2, SessionCommandStatus::Pending),
        ]);
        assert!(s.echo_pending("m1"));
        assert!(s.failed_commands().is_empty());
    }
}

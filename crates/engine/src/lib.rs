//! cypher-engine — the headless backend: sessions engine, doc host + command executor,
//! run journal + crash recovery, and the IPC RPC server.
//!
//! Spec: ARCHITECTURE.md §5 and docs/research/feature-inventory.md §3. M2 surface:
//! sessions + docs + commands + minimal IPC. Terminals, repos/diffs, uploads, auth,
//! agent accounts, and the device-room host land in later milestones.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
pub use cypher_proto::{EngineInfo, HarnessId, WorkspaceScope};
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};

use cypher_sync::DocsStore;

pub mod agent_accounts;
pub mod auth;
pub mod chat2_host;
pub mod diff_sync;
pub mod doc_host;
pub mod instance_lock;
pub mod local_import;
pub mod mcp;
pub mod pi_packages;
pub mod pi_providers;
pub mod pi_runtime;
pub mod profile;
pub mod registry;
pub mod repos;
pub mod rpc;
pub mod run_journal;
pub mod session_forks;
pub mod sessions;
pub mod side_chats;
pub mod spaces;
pub mod terminals;
pub mod titles;
pub mod uploads;
pub mod workspace_host;

pub use agent_accounts::{AgentAccounts, AgentAccountsConfig};
pub use auth::{Auth, AuthConfig, AuthState, AuthUser, OrgMembership};
pub use diff_sync::{
    CheckoutDiffSync, DiffFileTextPair, DiffSidecar, DiffSnapshot, TurnSnapshot,
    capture_commit_diff, capture_diff, capture_diff_against, capture_turn_diff, merge_base,
    read_diff_file_text, snapshot_tree, working_diff_base,
};
pub use doc_host::{ChatDocHandle, DocHost, DocHostConfig, EdgeConfig, attachment_refs_trailer};
pub use instance_lock::InstanceLock;
pub use profile::EngineProfile;
pub use registry::{
    HarnessDescriptor, HarnessRegistry, default_registry, default_registry_with_bridge,
    default_registry_with_bridge_and_runtime,
};
pub use repos::{CheckoutIdentity, Repos, worktree_branch_from_title};
pub use rpc::EngineRpc;
pub use run_journal::{JournalError, RunJournal};
pub use session_forks::SessionForks;
pub use sessions::{JournaledEvent, SessionsEngine, SteerOutcome};
pub use side_chats::SideChats;
pub use side_chats::bounded_transcript_context;
pub use spaces::SpacesSync;
pub use terminals::Terminals;
pub use titles::TitleGenerator;
pub use uploads::{AttachmentChunk, Uploads};
pub use workspace_host::{
    DEFAULT_ORG_ID, DEFAULT_USER_ID, WORKSPACE_DOC_ID, WorkspaceHost, WorkspaceHostConfig,
};

pub(crate) const LEGACY_UNKNOWN_DEVICE_NAME: &str = "unknown-device";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("doc: {0}")]
    Doc(#[from] cypher_doc::DocError),
    #[error("journal: {0}")]
    Journal(#[from] run_journal::JournalError),
    #[error("store: {0}")]
    Store(#[from] cypher_sync::StoreError),
    #[error("harness: {0}")]
    Harness(#[from] cypher_harness::HarnessError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Epoch millis now — the doc/journal timestamp base.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Data directory (default `~/.cypher`).
    pub data_dir: PathBuf,
    /// Edge base URL.
    pub edge_url: String,
    /// Explicit development bearer for edge room joins. Synced WorkOS runtimes
    /// obtain their bearer from [`Auth`]; development stays offline when this is absent.
    pub edge_token: Option<String>,
    /// Private Unix IPC socket for the UI.
    pub ipc_socket: PathBuf,
    /// Harness for doc-command runs on chats without a workspace `config` row.
    pub default_harness: HarnessId,
    /// Workspace-doc org (`ws/{orgId}` room). `None` = `$CYPHER_ORG_ID` or the
    /// dev default. In WorkOS mode the signed-in session's org wins.
    pub org_id: Option<String>,
    /// WorkOS client id — enables real auth; `None` = dev mode (bearer = `edge_token`).
    pub workos_client_id: Option<String>,
}

/// The assembled engine core — also constructible without the IPC server for tests
/// and the in-process (headed) mode.
pub struct EngineCore {
    pub sessions: SessionsEngine,
    pub doc_host: DocHost,
    pub workspace: WorkspaceHost,
    pub registry: Arc<HarnessRegistry>,
    pub repos: Repos,
    pub terminals: Terminals,
    pub diff_sync: CheckoutDiffSync,
    pub spaces_sync: SpacesSync,
    pub uploads: Uploads,
    pub agent_accounts: AgentAccounts,
    /// Temporary Side Chats (round 21): engine-hosted chats opened from a
    /// settled selection. Owned HERE (not by [`EngineRpc`]) so every RPC
    /// service built from this core shares one manager and shutdown reaps
    /// unpromoted chats.
    pub side_chats: SideChats,
    /// Session Fork (v1): clone a settled transcript prefix into a NEW
    /// durable root Pi chat on the source chat's host device.
    pub session_forks: SessionForks,
    pub device_id: String,
    /// Local→synced profile import (account-scoped runtimes only).
    pub local_import: Option<local_import::LocalImporter>,
    workspace_scope: WorkspaceScope,
    /// Auth service (attached by [`Engine::run`]; a lazy dev-mode instance otherwise).
    auth: std::sync::Mutex<Option<Auth>>,
    /// Peer link cache for `targetDeviceId` routing (attached when edge+auth are ready).
    links: std::sync::Mutex<Option<Arc<cypher_rpc::LinkCache>>>,
    /// Release checker (attached by [`Engine::assemble_runtime`]) — the
    /// UpdateStatus stream + ApplyUpdate.
    updater: std::sync::Mutex<Option<cypher_update::Updater>>,
    /// Downloaded, Cypher-owned Pi runtime + six-hour runtime update checker.
    pi_runtime: std::sync::Mutex<Option<pi_runtime::PiRuntimeManager>>,
    /// The updater's token-change wake forwarder — owned so shutdown can end it.
    updater_wake: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Exclusive data-dir lock — held for the engine's lifetime (single-instance).
    _instance_lock: InstanceLock,
}

impl EngineCore {
    /// Open stores under `data_dir`, wire sessions ⇄ doc host ⇄ workspace host, and
    /// recover stale journals from a previous crash. Identity comes from
    /// `$CYPHER_ORG_ID` / `$CYPHER_USER_ID` (dev defaults `dev-org` /
    /// `dev-user`); use [`Self::assemble_with_identity`] to pass one explicitly.
    pub fn assemble(
        data_dir: &Path,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
    ) -> Result<Self, EngineError> {
        let org_id = env_or("ORG_ID", DEFAULT_ORG_ID);
        let user_id = env_or("USER_ID", DEFAULT_USER_ID);
        let profile = EngineProfile::development(data_dir, &org_id, &user_id);
        Self::assemble_with_profile(profile, registry, default_harness, edge)
    }

    pub fn assemble_with_identity(
        data_dir: &Path,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        org_id: &str,
        user_id: &str,
    ) -> Result<Self, EngineError> {
        let profile = EngineProfile::synced(data_dir, org_id, user_id);
        Self::assemble_with_profile(profile, registry, default_harness, edge)
    }

    /// Assemble the engine against one resolved, immutable workspace profile.
    pub fn assemble_with_profile(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        // Single-instance guard: two engines on one data dir would race the
        // SQLite snapshots + journals. Taken before any store opens or the IPC
        // socket binds; held (and kernel-released on crash) for the engine's life.
        let lock = InstanceLock::acquire(data_dir)?;
        Self::assemble_with_profile_locked(profile, registry, default_harness, edge, lock)
    }

    /// Assemble against a pre-acquired [`InstanceLock`]. The headed app takes
    /// the lock before binding the IPC socket so the listener owner and the
    /// data-dir owner cannot diverge when several viewports bootstrap at once.
    pub fn assemble_with_profile_locked(
        profile: EngineProfile,
        registry: Arc<HarnessRegistry>,
        default_harness: HarnessId,
        edge: Option<EdgeConfig>,
        lock: InstanceLock,
    ) -> Result<Self, EngineError> {
        let data_dir = profile.device_root();
        std::fs::create_dir_all(data_dir)?;
        let legacy_uploads_root = profile.claim_legacy_uploads_root()?;
        let device_id = load_or_create_device_id(data_dir)?;
        // This device's harness enablement (Settings → Agents) rides the
        // engine data dir — per-device, like the CLI installs it gates.
        registry.load_prefs(data_dir);
        let store = Arc::new(DocsStore::open(profile.store_root())?);
        let store_for_import = store.clone();
        let journal = Arc::new(RunJournal::open(profile.store_root().join("journals"))?);
        let sessions = SessionsEngine::new(device_id.clone(), journal, registry.clone());
        let doc_host = DocHost::new(
            store.clone(),
            DocHostConfig {
                device_id: device_id.clone(),
                default_harness,
                edge: edge.clone(),
            },
        );
        let workspace = WorkspaceHost::open(
            store,
            WorkspaceHostConfig {
                device_id: device_id.clone(),
                device_name: local_device_name(&device_id),
                platform: std::env::consts::OS.to_string(),
                org_id: profile.org_id().to_string(),
                user_id: profile.user_id().to_string(),
                edge: edge.clone(),
                allow_device_rejoin: crate::auth::consume_sync_rejoin(data_dir),
            },
        )?;
        let side_chats = SideChats::new(sessions.clone(), doc_host.clone(), workspace.clone());
        let session_forks = SessionForks::new(
            sessions.clone(),
            doc_host.clone(),
            workspace.clone(),
            registry.clone(),
            profile.store_root().join("agent-sessions"),
        );
        doc_host.set_workspace(workspace.clone());
        doc_host.set_sessions(sessions.clone());
        sessions.set_doc_host(doc_host.clone());
        match sessions.recover_stale() {
            Ok(0) => {}
            Ok(recovered) => tracing::info!(recovered, "stale sessions recovered on boot"),
            Err(err) => tracing::error!(error = %err, "stale-session recovery failed"),
        }
        // The previous engine died with subagents in flight: terminalize this
        // device's durable Running projections (remote rows untouched — their
        // owners may still be live). Pure projection fix; never a status flip.
        if let Err(err) = sessions.recover_orphaned_subagents() {
            tracing::error!(error = %err, "orphaned-subagent recovery failed");
        }
        doc_host.spawn_transcript_salvage(profile.store_root().join("journals"));
        let repos = Repos::new(data_dir, &device_id);
        // Worktree materialization for Run commands carrying a WorktreeSpec
        // happens on the HOST at drain time (see `DocHost::materialize_worktree`).
        doc_host.set_repos(repos.clone());
        let terminals = Terminals::new();
        let uploads = Uploads::from_root_with_fallback(
            profile.uploads_root(),
            legacy_uploads_root.as_deref(),
        );
        // A recorded local→synced import grants this account the local
        // profile's uploads root read-only — transcripts imported earlier
        // embed absolute paths under it (same shape as the legacy adoption).
        if profile.scope() != WorkspaceScope::Local
            && let Some(root) =
                local_import::marker_grants_read_root(data_dir, profile.org_id(), profile.user_id())
        {
            uploads.add_read_only_root(&root);
        }
        let local_import = (profile.scope() == WorkspaceScope::Synced).then(|| {
            local_import::LocalImporter::new(
                data_dir,
                &device_id,
                profile.org_id(),
                profile.user_id(),
                store_for_import.clone(),
                profile.store_root().join("journals"),
                workspace.clone(),
                uploads.clone(),
            )
        });
        let agent_accounts = AgentAccounts::new(AgentAccountsConfig::detect(data_dir));
        sessions.set_titles(TitleGenerator::new(
            workspace.clone(),
            registry.clone(),
            repos.clone(),
        ));
        let diff_sync = CheckoutDiffSync::start(repos.clone(), workspace.clone(), &device_id, edge);
        // Turn starts snapshot the checkout tree — the "Latest turn" diff base.
        let turn_diff = diff_sync.clone();
        sessions.set_turn_listener(Arc::new(move |chat_id, cwd| {
            turn_diff.note_turn_start(chat_id, cwd);
        }));
        let spaces_sync = SpacesSync::start(repos.clone(), workspace.clone(), &device_id);
        Ok(Self {
            sessions,
            doc_host,
            workspace,
            registry,
            repos,
            terminals,
            diff_sync,
            spaces_sync,
            uploads,
            agent_accounts,
            side_chats,
            session_forks,
            device_id,
            local_import,
            workspace_scope: profile.scope(),
            auth: std::sync::Mutex::new(None),
            links: std::sync::Mutex::new(None),
            updater: std::sync::Mutex::new(None),
            pi_runtime: std::sync::Mutex::new(None),
            updater_wake: std::sync::Mutex::new(None),
            _instance_lock: lock,
        })
    }

    pub fn workspace_scope(&self) -> WorkspaceScope {
        self.workspace_scope
    }

    /// Attach the auth service (before building the RPC service / relays).
    pub fn set_auth(&self, auth: Auth) {
        *self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(auth);
    }

    /// The attached auth service, or a lazily-created dev-mode one (in-process embeds
    /// that never wired WorkOS still answer AuthStatus honestly).
    pub fn auth(&self) -> Auth {
        let mut slot = self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.get_or_insert_with(|| {
            let dev_user = cypher_env::var("EDGE_TOKEN")
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "dev-user".into());
            let mut config = AuthConfig::new("http://localhost:27640", std::env::temp_dir());
            config.dev_user_id = dev_user;
            Auth::new(config)
        })
        .clone()
    }

    /// Attach the peer link cache — enables `targetDeviceId` routing and [`Self::dial_device`].
    pub fn set_links(&self, links: Arc<cypher_rpc::LinkCache>) {
        *self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(links);
    }

    pub fn links(&self) -> Option<Arc<cypher_rpc::LinkCache>> {
        self.links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Attach the release checker (before building the RPC service).
    pub fn set_updater_wake(&self, handle: tokio::task::JoinHandle<()>) {
        *self
            .updater_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
    }

    pub fn set_updater(&self, updater: cypher_update::Updater) {
        *self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(updater);
    }

    pub fn updater(&self) -> Option<cypher_update::Updater> {
        self.updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_pi_runtime(&self, runtime: pi_runtime::PiRuntimeManager) {
        *self
            .pi_runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(runtime);
    }

    pub fn pi_runtime(&self) -> Option<pi_runtime::PiRuntimeManager> {
        self.pi_runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A live RPC client to another device's engine through its relay DO (the router's
    /// dial seam). Cached per device; invalidated + re-dialed on failure.
    pub async fn dial_device(
        &self,
        device_id: &str,
    ) -> Result<Arc<cypher_rpc::RpcClient>, EngineError> {
        let links = self
            .links()
            .ok_or_else(|| EngineError::Other("peer links unavailable (offline)".into()))?;
        links
            .client(device_id)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))
    }

    /// Start hosting our device room: serve the full RPC surface to relay clients and
    /// warm-open chat docs on nudges (§7 cold-chat command delivery). The token source
    /// re-reads auth on every (re)dial, so token refreshes take effect at reconnect.
    pub fn start_host_relay(&self, edge_url: &str) -> cypher_rpc::HostRelay {
        let auth = self.auth();
        let config =
            cypher_rpc::HostRelayConfig::new(edge_url, self.device_id.clone(), Arc::new(auth));
        let doc_host = self.doc_host.clone();
        let on_nudge: cypher_rpc::NudgeHandler = Arc::new(move |chat_id: String| {
            // Opening the doc joins its room + syncs; drain fires on the change
            // subscription — the command executes with no standing per-chat socket.
            match doc_host.open(&chat_id) {
                Ok(_) => tracing::info!(chat = %chat_id, "nudge: chat doc opened"),
                Err(err) => {
                    tracing::warn!(chat = %chat_id, error = %err, "nudge: open failed")
                }
            }
        });
        cypher_rpc::HostRelay::spawn(config, self.rpc_service(), on_nudge)
    }

    pub fn rpc_service(&self) -> Arc<EngineRpc> {
        let mut rpc = EngineRpc::new(
            self.sessions.clone(),
            self.doc_host.clone(),
            self.workspace.clone(),
            self.registry.clone(),
            self.repos.clone(),
            self.terminals.clone(),
            self.diff_sync.clone(),
            self.uploads.clone(),
            self.agent_accounts.clone(),
            self.side_chats.clone(),
            self.session_forks.clone(),
            self.workspace_scope,
        )
        .with_auth(self.auth());
        if let Some(links) = self.links() {
            rpc = rpc.with_links(links);
        }
        if let Some(updater) = self.updater() {
            rpc = rpc.with_updater(updater);
        }
        if let Some(runtime) = self.pi_runtime() {
            rpc = rpc.with_pi_runtime(runtime);
        }
        if let Some(importer) = self.local_import.clone() {
            rpc = rpc.with_local_import(importer);
        }
        Arc::new(rpc)
    }

    /// Revoke every account-scoped transport before any slower graceful
    /// draining. Connected sockets remain authorized by their handshake, so
    /// clearing credentials alone is not a security boundary.
    pub fn disconnect_edge(&self) {
        if let Some(links) = self.links() {
            links.disconnect_all();
        }
        self.doc_host.disconnect_edge();
        self.workspace.disconnect_edge();
    }

    /// Graceful teardown: settle live runs (streaming entries stamped `aborted`),
    /// kill live PTYs, stamp our workspace `lastSeenAt`, and flush every open doc
    /// snapshot.
    pub async fn shutdown(&self) {
        // Reap temporary Side Chats FIRST: interrupt their live runs and drop
        // every ephemeral doc (host-memory only — dispose leaves no durable
        // remnants; a promoted chat is untouched) BEFORE the general session
        // teardown settles/flushes the remaining normal chats.
        self.side_chats.shutdown().await;
        self.sessions.shutdown().await;
        self.terminals.shutdown();
        self.agent_accounts.shutdown();
        // Cancel + await every worker that can reach Edge before flushing: a
        // replaced synced runtime must not keep polling releases or draining
        // the attachment outbox under the old identity after Local boots.
        let wake = self
            .updater_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(wake) = wake {
            wake.abort();
            let _ = wake.await;
        }
        let updater = self
            .updater
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(updater) = updater {
            updater.shutdown().await;
        }
        let pi_runtime = self
            .pi_runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(runtime) = pi_runtime {
            runtime.shutdown().await;
        }
        self.diff_sync.shutdown().await;
        self.spaces_sync.shutdown().await;
        self.doc_host.shutdown_workers().await;
        self.doc_host.flush_all();
        self.workspace.shutdown();
        // Break the sessions ⇄ doc-host retain cycle so the replaced graph can
        // actually be freed once the last handle drops.
        self.sessions.clear_doc_host();
    }
}

pub struct Engine {
    pub config: EngineConfig,
}

/// A fully assembled identity-scoped engine plus the relay handle whose lifetime
/// keeps this device reachable. Used by both the headless server and the headed
/// in-process engine so their production authentication paths cannot diverge.
pub struct EngineRuntime {
    core: EngineCore,
    host_relay: std::sync::Mutex<Option<cypher_rpc::HostRelay>>,
}

/// IPC-only lifecycle control owned by `cypher headless`. The regular
/// [`EngineRpc`] deliberately does not expose this method, so a viewport
/// attached to another headed process cannot shut down that process's engine.
struct HeadlessRpc {
    inner: Arc<dyn RpcService>,
    stop_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl RpcService for HeadlessRpc {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        if method != methods::STOP_ENGINE {
            return self.inner.handle(method, params).await;
        }

        let stop_tx = self.stop_tx.clone();
        // Let the unary success frame reach the client before `Engine::run`
        // aborts the IPC server and drains the runtime.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = stop_tx.send(());
        });
        RpcReply::value(&serde_json::json!({ "ok": true }))
    }
}

impl EngineRuntime {
    pub fn core(&self) -> &EngineCore {
        &self.core
    }

    pub fn workspace_scope(&self) -> WorkspaceScope {
        self.core.workspace_scope()
    }

    pub fn disconnect_edge(&self) {
        // Revoke remote reachability before graceful draining. Sessions may
        // need time to settle; no authenticated relay RPC may enter during
        // that window after sign-out.
        self.host_relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.core.disconnect_edge();
    }

    pub async fn shutdown(&self) {
        self.disconnect_edge();
        self.core.shutdown().await;
    }
}

impl Drop for EngineRuntime {
    fn drop(&mut self) {
        self.host_relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    /// Resolve the shared dev/WorkOS auth configuration for headed and headless
    /// modes. A clean WorkOS boot deliberately avoids probing Edge: signed-out
    /// installations must be able to start locally without network access.
    pub async fn build_auth(config: &EngineConfig) -> Auth {
        let mut auth_config = AuthConfig::new(config.edge_url.clone(), config.data_dir.clone());
        auth_config.workos_client_id = config.workos_client_id.clone();
        if let Some(base) = cypher_env::var("WORKOS_API_BASE")
            && !base.trim().is_empty()
        {
            auth_config.workos_api_base = base;
        }
        // Production default: an EPHEMERAL loopback port (OS-assigned). Only an
        // explicit CYPHER_CALLBACK_PORT pins a concrete port (port-forwarded
        // dev boxes, firewalled hosts). The dashboard registers the wildcard
        // `http://127.0.0.1:*/callback`, so the exact port is irrelevant to it.
        auth_config.callback_port = cypher_env::var("CALLBACK_PORT").and_then(|p| p.parse().ok());
        if let Some(token) = &config.edge_token {
            auth_config.dev_user_id = token.clone();
        }
        Auth::new(auth_config)
    }

    /// Capture the workspace boundary once, before refresh or sign-in can mutate auth.
    pub fn initial_workspace_scope(auth: &Auth) -> WorkspaceScope {
        if !auth.workos_enabled() {
            WorkspaceScope::Development
        } else if auth.loaded_workos_session() {
            WorkspaceScope::Synced
        } else {
            WorkspaceScope::Local
        }
    }

    /// Resolve a profile for the captured scope. A synced session without an
    /// organization returns `None` until onboarding selects one; it never falls
    /// through to the local or development profile.
    pub fn resolve_profile(
        config: &EngineConfig,
        auth: &Auth,
        scope: WorkspaceScope,
    ) -> Result<Option<EngineProfile>, EngineError> {
        match scope {
            WorkspaceScope::Local => EngineProfile::local(&config.data_dir).map(Some),
            WorkspaceScope::Development => {
                let dev_token_org = config
                    .edge_token
                    .as_deref()
                    .and_then(|token| token.split_once('@'))
                    .map(|(_, org)| org.to_string())
                    .filter(|org| !org.is_empty());
                let org_id = dev_token_org
                    .or(config.org_id.clone())
                    .unwrap_or_else(|| env_or("ORG_ID", DEFAULT_ORG_ID));
                let user_id = auth
                    .user_id()
                    .unwrap_or_else(|| env_or("USER_ID", DEFAULT_USER_ID));
                Ok(Some(EngineProfile::development(
                    &config.data_dir,
                    &org_id,
                    &user_id,
                )))
            }
            WorkspaceScope::Synced => {
                let state = auth.state();
                let Some(user) = state.user() else {
                    return Err(EngineError::Other(
                        "captured synced session no longer exposes its user identity".into(),
                    ));
                };
                let Some(org_id) = state.org_id() else {
                    return Ok(None);
                };
                Ok(Some(EngineProfile::synced(
                    &config.data_dir,
                    org_id,
                    &user.id,
                )))
            }
        }
    }

    /// Resolve the one-shot identity served before profile stores are available.
    pub fn engine_info(
        config: &EngineConfig,
        workspace_scope: WorkspaceScope,
    ) -> Result<EngineInfo, EngineError> {
        std::fs::create_dir_all(&config.data_dir)?;
        Ok(EngineInfo {
            device_id: load_or_create_device_id(&config.data_dir)?,
            workspace_scope,
        })
    }

    /// Open one already-resolved profile. Synced profiles always keep their
    /// Edge supervisors alive; temporary token or network failures are runtime
    /// states, not a reason to permanently assemble an offline engine.
    pub async fn assemble_runtime(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, auth, profile, None).await
    }

    /// Like [`Self::assemble_runtime`], but against an [`InstanceLock`] the
    /// caller already holds on the profile's device root (headed bootstrap
    /// acquires it before binding IPC).
    pub async fn assemble_runtime_with_lock(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
        lock: InstanceLock,
    ) -> anyhow::Result<EngineRuntime> {
        Self::assemble_runtime_inner(config, auth, profile, Some(lock)).await
    }

    async fn assemble_runtime_inner(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
        lock: Option<InstanceLock>,
    ) -> anyhow::Result<EngineRuntime> {
        let edge_enabled = match profile.scope() {
            WorkspaceScope::Local => false,
            WorkspaceScope::Synced => {
                // Validate the persisted session in the BACKGROUND: the probe
                // still transitions auth to SignedOut on definitive revocation
                // (and warms the single-flight refresh every first dial waits
                // on), but assembly — and the viewport blocked on it — no
                // longer stalls on a WorkOS round trip that can take seconds
                // on a bad link. Everything shown at boot is local anyway.
                let auth_probe = auth.clone();
                tokio::spawn(async move {
                    let _ = auth_probe.access_token().await;
                });
                true
            }
            // Dev Auth always exposes `dev_user_id` as its synthetic access
            // token, including when WorkOS was merely disabled with
            // CYPHER_WORKOS_CLIENT_ID="". Only an explicitly configured,
            // non-empty bearer opts this runtime into Edge rooms and relays.
            WorkspaceScope::Development => config
                .edge_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty()),
        };
        let device_id = load_or_create_device_id(profile.device_root())?;
        let edge = edge_enabled.then(|| {
            EdgeConfig::new(config.edge_url.clone(), Arc::new(auth.clone())).with_device(device_id)
        });

        // The cypher-owned pi session store (`pi --mode rpc --session-dir`).
        let pi_sessions_root = profile.store_root().join("agent-sessions");
        // The executable/config boundary is device-global and fixed before
        // first-run installation. No system Pi fallback: publishing the
        // `current` symlink makes the lazy harness become installed.
        let pi_runtime_data_dir = profile.device_root().to_path_buf();
        let pi_runtime_paths = pi_runtime::PiRuntimePaths::for_data_dir(profile.device_root());
        // The engine-bridge URL every pi child gets as `CYPHER_ENGINE_SOCKET`:
        // this runtime's own IPC WebSocket (`serve_ipc` binds the same port in
        // headless and headed modes). Test-only `EngineCore::assemble` keeps
        // `None` — it never serves IPC.
        let engine_socket = Some(config.ipc_socket.to_string_lossy().into_owned());
        let synced_scope = profile.scope() == WorkspaceScope::Synced;
        let core = match lock {
            Some(lock) => EngineCore::assemble_with_profile_locked(
                profile,
                Arc::new(default_registry_with_bridge_and_runtime(
                    pi_sessions_root,
                    engine_socket,
                    Some(pi_runtime_paths.clone()),
                )),
                config.default_harness,
                edge.clone(),
                lock,
            )?,
            None => EngineCore::assemble_with_profile(
                profile,
                Arc::new(default_registry_with_bridge_and_runtime(
                    pi_sessions_root,
                    engine_socket,
                    Some(pi_runtime_paths),
                )),
                config.default_harness,
                edge.clone(),
            )?,
        };
        core.set_auth(auth.clone());
        if synced_scope {
            let mut evicted = core.workspace.watch_evicted();
            let auth_for_evict = auth.clone();
            tokio::spawn(async move {
                loop {
                    if *evicted.borrow() {
                        tracing::warn!("this device was unpaired; signing out of sync");
                        auth_for_evict.sign_out();
                        return;
                    }
                    if evicted.changed().await.is_err() {
                        return;
                    }
                }
            });
        }
        // Release checker: polls {edge}/releases on a 6h cadence; headless
        // installs with CYPHER_AUTO_UPDATE=1 apply + restart themselves — gated
        // on quiescence so a restart never lands under a live run or open PTY.
        //
        // Attached for EVERY runtime/profile: release endpoints are public and
        // updates are device-local, so UpdateStatus must be served even by a
        // local-only runtime (0.1.0 gated this on `edge_enabled`, leaving local
        // runtimes without the UpdateStatus RPC — the UI's stream closed and
        // resubscribed every 2s forever). Only the token-change wake stays
        // edge-gated: it exists to re-check when auth recovers.
        let quiescent: cypher_update::QuiescentCheck = {
            let sessions = core.sessions.clone();
            let terminals = core.terminals.clone();
            Arc::new(move || !sessions.any_active() && !terminals.any_open())
        };
        let updater = cypher_update::Updater::spawn(
            config.edge_url.clone(),
            Some(quiescent),
            config.data_dir.clone(),
        );
        if let Some(mut token_changes) = edge.as_ref().and_then(EdgeConfig::token_changes) {
            let updater_for_tokens = updater.clone();
            let wake = tokio::spawn(async move {
                while token_changes.changed().await.is_ok() {
                    updater_for_tokens.check_now();
                }
            });
            core.set_updater_wake(wake);
        }
        core.set_updater(updater);
        core.set_pi_runtime(pi_runtime::PiRuntimeManager::spawn(
            config.edge_url.clone(),
            &pi_runtime_data_dir,
        ));
        tracing::info!(device_id = %core.device_id, "engine core assembled");
        // Managed ACP adapters install in the background at boot (agents
        // whose CLI is present but whose adapter isn't yet), so a first chat
        // never waits on — or dies inside — an npm run.
        cypher_harness::acp::prewarm_managed_adapters();
        // First `/` in the composer waits on a short-lived `pi --mode rpc`
        // that loads every extension. Kick that probe off at boot so the
        // popup is a cache hit.
        {
            let registry = core.registry.clone();
            tokio::spawn(async move {
                let Ok(harness) = registry.resolve(HarnessId::Pi) else {
                    return;
                };
                if let Err(err) = harness.commands().await {
                    tracing::debug!(error = %err, "prewarm pi commands failed");
                }
            });
        }

        let host_relay = edge.as_ref().map(|edge| {
            let links = cypher_rpc::LinkCache::new(cypher_rpc::LinkCacheConfig::new(
                edge.url.clone(),
                Arc::new(auth.clone()),
            ));
            let links_for_presence = links.clone();
            core.workspace
                .set_peer_alive_hook(Arc::new(move |device_id: &str| {
                    links_for_presence.reset_cooldown(device_id);
                }));
            core.set_links(links);
            core.start_host_relay(&edge.url)
        });

        Ok(EngineRuntime {
            core,
            host_relay: std::sync::Mutex::new(host_relay),
        })
    }

    /// Run until ctrl-c: auth (dev or WorkOS), sessions engine + doc host + command
    /// executor, IPC server, and — when edge+auth are ready — the device-room host
    /// relay + peer link cache (targetDeviceId routing).
    pub async fn run(self) -> anyhow::Result<()> {
        let config = self.config;
        anyhow::ensure!(
            config.ipc_socket == cypher_env::ipc_socket(&config.data_dir)?,
            "Engine IPC socket does not match its data directory"
        );
        tracing::info!(data_dir = %config.data_dir.display(), "engine starting");

        std::fs::create_dir_all(&config.data_dir)?;
        // Auth construction can persist a sanitized session, and its refresh
        // loop rotates single-use credentials. Own the directory BEFORE either.
        let lock = InstanceLock::acquire(&config.data_dir)?;
        let listener = cypher_rpc::LocalListener::bind(&config.ipc_socket).await?;
        let auth = Self::build_auth(&config).await;
        let mut auth_state = auth.watch_state();
        let workspace_scope = Self::initial_workspace_scope(&auth);
        let mut profile = Self::resolve_profile(&config, &auth, workspace_scope)?;
        let _refresh_loop = tokio_util::task::AbortOnDropHandle::new(auth.spawn_refresh_loop());

        // A captured cloud session without an organization must finish onboarding
        // before its profile can open. A clean signed-out install is local and never
        // enters the terminal sign-in flow.
        if workspace_scope == WorkspaceScope::Synced && profile.is_none() {
            terminal_sign_in(&auth).await?;
            profile = Self::resolve_profile(&config, &auth, workspace_scope)?;
        }
        let profile = profile
            .ok_or_else(|| EngineError::Other("synced workspace profile is not ready".into()))?;

        let runtime = Self::assemble_runtime_with_lock(&config, auth, profile, lock).await?;

        // A daemon exists to serve this port, so a bind failure is fatal here —
        // unlike the headed app, which can still work over its in-process
        // transport (see `serve_ipc`).
        let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel();
        let service: Arc<dyn RpcService> = Arc::new(HeadlessRpc {
            inner: runtime.core().rpc_service(),
            stop_tx,
        });
        let server = tokio::spawn(listener.serve(service));

        tokio::select! {
            result = shutdown_signal() => result?,
            requested = stop_rx.recv() => {
                if requested.is_some() {
                    tracing::info!("headless shutdown requested over IPC");
                }
            }
            _ = wait_for_signed_out(&mut auth_state), if workspace_scope == WorkspaceScope::Synced => {
                // Edge transports observe the same auth signal and close at
                // once. Leave a brief reply window for a SignOut RPC before
                // the localhost server itself is aborted.
                runtime.disconnect_edge();
                tracing::info!("headless authentication revoked; stopping synced runtime");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        tracing::info!("shutting down");
        server.abort();
        let _ = server.await;
        _refresh_loop.abort();
        let _ = _refresh_loop.await;
        runtime.shutdown().await;
        Ok(())
    }
}

async fn wait_for_signed_out(state: &mut tokio::sync::watch::Receiver<AuthState>) {
    loop {
        if matches!(&*state.borrow(), AuthState::SignedOut) {
            return;
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}

/// Ctrl-C, SIGTERM, or SSH terminal hangup. systemd/launchd stop (and the auto-updater's service
/// restart) deliver SIGTERM — without catching it the daemon dies mid-write
/// and every stop takes the crash-recovery path instead of the graceful drain.
pub async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = sigterm.recv() => Ok(()),
            _ = sighup.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Serve an embedded engine to same-user viewports over private Unix IPC.
/// The caller must own its data-directory lock. Binding/validation failures
/// are fatal; there is no TCP or unserved embedded-engine fallback.
pub async fn serve_ipc(
    path: &std::path::Path,
    service: std::sync::Arc<dyn cypher_rpc::RpcService>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = cypher_rpc::LocalListener::bind(path).await?;
    tracing::info!(socket = %path.display(), "IPC server listening");
    Ok(tokio::spawn(listener.serve(service)))
}

const DEFAULT_PERSONAL_ORG_NAME: &str = "Personal";

/// Block until the WorkOS session is signed in AND org-scoped. On a TTY, print the
/// headless (paste-code) sign-in URL, read the pasted `state.code` from stdin, and
/// run workspace onboarding (auto-provision / auto-join / numbered picker). Off a
/// TTY this errors immediately — a daemon under systemd/launchd must load the
/// session that `cypher login` persisted, never wait on a prompt nobody can see.
pub async fn terminal_sign_in(auth: &Auth) -> Result<(), EngineError> {
    terminal_sign_in_until(auth, std::future::pending()).await
}

/// Cancellable CLI onboarding. Reader tasks are aborted and joined before
/// returning, so a caller may safely release its auth lock/restart a service.
pub async fn terminal_sign_in_until(
    auth: &Auth,
    cancelled: impl std::future::Future<Output = ()>,
) -> Result<(), EngineError> {
    tokio::pin!(cancelled);
    use std::io::IsTerminal;
    let interactive = std::io::stdin().is_terminal();
    let mut state_rx = auth.watch_state();
    let mut stdin_reader: Option<tokio::task::JoinHandle<()>> = None;
    let mut org_reader: Option<tokio::task::JoinHandle<()>> = None;
    let outcome = loop {
        let state = state_rx.borrow().clone();
        match state {
            AuthState::SignedIn { user, org_id } => {
                tracing::info!(email = %user.email, org = org_id.as_deref().unwrap_or("<none>"),
                    "auth: session ready");
                break Ok(());
            }
            AuthState::NeedsOrganization { user } => {
                if !interactive {
                    // No reader tasks have been spawned on this path (both spawns
                    // are TTY-gated), so an early return leaks nothing.
                    return Err(EngineError::Other(format!(
                        "signed in as {} but no workspace is selected — run `cypher login` on this machine to pick one",
                        user.email
                    )));
                }
                if org_reader.is_none() {
                    // Workspace onboarding on the TTY: provision a personal
                    // organization if none exists, auto-join a single
                    // membership, or show a numbered picker otherwise.
                    println!("Signed in as {}.", user.email);
                    org_reader = Some(tokio::spawn(run_org_onboarding(auth.clone())));
                }
            }
            AuthState::SignedOut => {
                if !interactive {
                    return Err(EngineError::Other(
                        "not signed in — run `cypher login` on this machine first".into(),
                    ));
                }
                if stdin_reader.is_none() {
                    let url = auth.start_headless_sign_in();
                    println!("Sign in to Cypher:\n\n  {url}\n");
                    println!("Then paste the code shown in the browser here and press enter.");
                    let auth = auth.clone();
                    stdin_reader = Some(tokio::spawn(async move {
                        loop {
                            let Some(line) = read_stdin_line().await else {
                                return;
                            };
                            let pasted = line.trim();
                            if pasted.is_empty() {
                                continue;
                            }
                            match auth.complete_sign_in(pasted).await {
                                Ok(()) => return,
                                Err(err) => {
                                    println!("Sign-in failed: {err}");
                                    if auth.email_verification_pending() {
                                        println!(
                                            "Enter the six-digit verification code sent to your email:"
                                        );
                                        loop {
                                            let Some(line) = read_stdin_line().await else {
                                                return;
                                            };
                                            let code = line.trim();
                                            if code.is_empty() {
                                                continue;
                                            }
                                            match auth.complete_email_verification(code).await {
                                                Ok(()) => return,
                                                Err(err) => {
                                                    println!("Email verification failed: {err}")
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }));
                }
            }
        }
        // EOF, a failed workspace request, or a reader panic does not change
        // auth state. Waiting only for that state used to hang login forever.
        tokio::select! {
            _ = &mut cancelled => {
                break Err(EngineError::Other("sign-in canceled; run `cypher setup` to continue".into()));
            }
            changed = state_rx.changed() => {
                if changed.is_err() {
                    break Err(EngineError::Other("authentication closed before sign-in completed".into()));
                }
            }
            result = wait_terminal_reader(&mut stdin_reader) => {
                stdin_reader = None;
                if result.is_err() || matches!(auth.state(), AuthState::SignedOut) {
                    break Err(EngineError::Other("sign-in did not complete (terminal input closed)".into()));
                }
            }
            result = wait_terminal_reader(&mut org_reader) => {
                org_reader = None;
                if result.is_err() || !auth.state().is_signed_in() {
                    break Err(EngineError::Other("workspace setup did not complete — retry `cypher login`".into()));
                }
            }
        }
    };
    if let Some(reader) = stdin_reader {
        reader.abort();
        let _ = reader.await;
    }
    if let Some(reader) = org_reader {
        reader.abort();
        let _ = reader.await;
    }
    outcome
}

async fn wait_terminal_reader(
    reader: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<(), tokio::task::JoinError> {
    match reader {
        Some(reader) => reader.await,
        None => std::future::pending().await,
    }
}

/// One line from stdin (blocking read off the runtime). `None` = stdin closed.
async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None, // EOF / error
            Ok(_) => Some(line),
        }
    })
    .await
    .ok()
    .flatten()
}

/// TTY workspace onboarding for an org-less session: no memberships →
/// automatically provision `Personal`; exactly one → auto-join; several →
/// numbered picker. Success flips the auth state to `SignedIn`, which ends
/// [`terminal_sign_in`]'s wait (and aborts this task).
async fn run_org_onboarding(auth: Auth) {
    let orgs = match auth.list_orgs().await {
        Ok(orgs) => orgs,
        Err(err) => {
            println!(
                "Could not list workspaces ({err}) — create or select one from the Cypher UI to continue."
            );
            return;
        }
    };
    match orgs.len() {
        0 => {
            println!("Preparing your synced workspace…");
            if let Err(err) = auth.create_org(DEFAULT_PERSONAL_ORG_NAME).await {
                println!("Creating workspace failed: {err}");
            }
        }
        1 => {
            let only = &orgs[0];
            println!("Joining workspace \"{}\"…", only.name);
            if let Err(err) = auth.select_org(&only.organization_id).await {
                println!("Joining workspace failed: {err}");
            }
        }
        _ => {
            println!("\nYour workspaces:");
            for (index, org) in orgs.iter().enumerate() {
                println!("  {}. {}", index + 1, org.name);
            }
            println!("Pick a workspace [1-{}]:", orgs.len());
            loop {
                let Some(line) = read_stdin_line().await else {
                    return;
                };
                let choice = line
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|index| orgs.get(index));
                let Some(org) = choice else {
                    println!("Pick a workspace [1-{}]:", orgs.len());
                    continue;
                };
                match auth.select_org(&org.organization_id).await {
                    Ok(()) => return,
                    Err(err) => println!("Joining workspace failed: {err}"),
                }
            }
        }
    }
}

/// Best-effort human name for this device's registry row.
fn local_device_name(device_id: &str) -> String {
    select_local_device_name(
        [
            cypher_env::var("DEVICE_NAME"),
            native_friendly_device_name(),
            std::env::var("HOSTNAME").ok(),
            gethostname::gethostname().into_string().ok(),
            std::fs::read_to_string("/etc/hostname").ok(),
        ],
        device_id,
        std::env::consts::OS,
    )
}

fn select_local_device_name(
    candidates: impl IntoIterator<Item = Option<String>>,
    device_id: &str,
    platform: &str,
) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(|name| name.trim().to_string())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let platform = match platform {
                "macos" => "macOS",
                "windows" => "Windows",
                "linux" => "Linux",
                _ => "Local",
            };
            let short_id: String = device_id.chars().take(8).collect();
            format!("{platform} device {short_id}")
        })
}

#[cfg(target_os = "macos")]
fn native_friendly_device_name() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn native_friendly_device_name() -> Option<String> {
    #[cfg(target_os = "windows")]
    return std::env::var("COMPUTERNAME").ok();

    #[cfg(not(target_os = "windows"))]
    None
}

#[cfg(test)]
mod device_name_tests {
    use super::select_local_device_name;

    fn name(candidates: &[Option<&str>], device_id: &str, platform: &str) -> String {
        select_local_device_name(
            candidates
                .iter()
                .map(|candidate| candidate.map(str::to_string)),
            device_id,
            platform,
        )
    }

    #[test]
    fn explicit_override_wins_and_is_trimmed() {
        assert_eq!(
            name(
                &[Some("  Studio Mac  "), Some("system-host")],
                "17bc0aa2-rest",
                "macos"
            ),
            "Studio Mac"
        );
    }

    #[test]
    fn native_friendly_name_wins_over_hostnames() {
        assert_eq!(
            name(
                &[
                    None,
                    Some("MacBook Pro de Jose"),
                    None,
                    Some("MacBook-Pro.local"),
                ],
                "17bc0aa2-rest",
                "macos"
            ),
            "MacBook Pro de Jose"
        );
    }

    #[test]
    fn windows_computer_name_is_used_when_present() {
        assert_eq!(
            name(
                &[None, Some("DESKTOP-123"), Some("shell-host")],
                "17bc0aa2-rest",
                "windows"
            ),
            "DESKTOP-123"
        );
    }

    #[test]
    fn blank_candidates_are_ignored() {
        assert_eq!(
            name(
                &[Some("  "), None, Some("\n"), Some("linux-box")],
                "17bc0aa2-rest",
                "linux"
            ),
            "linux-box"
        );
    }

    #[test]
    fn final_fallback_is_platform_specific_and_distinct() {
        assert_eq!(
            name(&[None, Some(" ")], "17bc0aa2-rest", "linux"),
            "Linux device 17bc0aa2"
        );
    }
}

/// Trimmed env var or the given default.
fn env_or(key: &str, default: &str) -> String {
    cypher_env::var(key).unwrap_or_else(|| default.to_string())
}

/// Stable per-installation device id, persisted at `{data_dir}/device-id`.
fn load_or_create_device_id(data_dir: &Path) -> Result<String, EngineError> {
    std::fs::create_dir_all(data_dir)?;
    // EngineInfo is resolved before the lifetime InstanceLock is acquired, so
    // identity creation and legacy repair need their own short critical section.
    // The OS releases this lock after a crash; unlike a create_new lockfile it
    // cannot strand an installation permanently.
    let _identity_lock = DeviceIdentityLock::acquire(data_dir)?;
    let path = data_dir.join("device-id");
    let recovering_empty = match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => return Ok(id.trim().to_string()),
        // Older releases used truncate+write. A crash between those operations
        // left a zero-byte file that is safe to replace with a fresh identity.
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err.into()),
    };

    let id = new_id();
    let temp_path = data_dir.join(format!(
        ".device-id.tmp-{}-{}",
        std::process::id(),
        new_id()
    ));
    let write_result = (|| -> Result<(), EngineError> {
        let mut temp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        temp.write_all(id.as_bytes())?;
        temp.sync_all()?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    // Fresh installs use create-if-absent. Legacy empty files need an atomic
    // same-directory replacement on Unix; the Windows fallback runs under the
    // identity lock and remains recoverable if interrupted.
    let publish_result = if recovering_empty {
        match std::fs::read_to_string(&path) {
            Ok(id) if !id.trim().is_empty() => {
                let _ = std::fs::remove_file(&temp_path);
                return Ok(id.trim().to_string());
            }
            Ok(_) => replace_empty_device_id(&temp_path, &path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::hard_link(&temp_path, &path)
            }
            Err(err) => Err(err),
        }
    } else {
        std::fs::hard_link(&temp_path, &path)
    };
    let _ = std::fs::remove_file(&temp_path);
    match publish_result {
        Ok(()) => Ok(id),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let winner = std::fs::read_to_string(&path)?;
            if winner.trim().is_empty() {
                Err(EngineError::Other(format!(
                    "invalid device identity {}: file is empty",
                    path.display()
                )))
            } else {
                Ok(winner.trim().to_string())
            }
        }
        Err(err) => Err(err.into()),
    }
}

struct DeviceIdentityLock {
    _file: std::fs::File,
}

impl DeviceIdentityLock {
    fn acquire(data_dir: &Path) -> Result<Self, EngineError> {
        let path = data_dir.join("device-id.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
            let mut retries = 200;
            let file = loop {
                match options.open(&path) {
                    Ok(file) => break file,
                    Err(err)
                        if err.kind() == std::io::ErrorKind::PermissionDenied && retries > 0 =>
                    {
                        retries -= 1;
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(err) => return Err(err.into()),
                }
            };
            return Ok(Self { _file: file });
        }

        #[cfg(not(windows))]
        let file = options.open(&path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    return Err(err.into());
                }
            }
        }

        #[cfg(not(windows))]
        Ok(Self { _file: file })
    }
}

fn replace_empty_device_id(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(temp_path, path)
    }
    #[cfg(not(unix))]
    {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        std::fs::hard_link(temp_path, path)
    }
}

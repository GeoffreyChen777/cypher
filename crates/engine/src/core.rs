//! [`EngineCore`]: opens the profile's stores and wires sessions, the doc
//! host, the workspace host and the other services into one engine.

use std::path::Path;
use std::sync::{Arc, Mutex};

use cypher_proto::{HarnessId, WorkspaceScope};
use cypher_sync::DocsStore;

use crate::device_identity::{load_or_create_device_id, local_device_name};
use crate::host::workspace_host::WorkspaceHostConfig;
use crate::rpc::EngineRpc;
use crate::session::side_chats::SideChats;
use crate::session::titles::TitleGenerator;
use crate::util::{env_or, lock};
use crate::{
    Auth, AuthConfig, CheckoutDiffSync, DEFAULT_ORG_ID, DEFAULT_USER_ID, DocHost, DocHostConfig,
    EdgeConfig, EngineError, EngineProfile, HarnessRegistry, InstanceLock, Repos, RunJournal,
    SessionForks, SessionsEngine, SpacesSync, Terminals, Uploads, WorkspaceHost, mcp,
};
use crate::{git, host, pi, session};

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
    pub title_settings: session::title_settings::TitleSettingsStore,
    /// This device's GitHub sign-in (device-scoped).
    pub github: git::github::Github,
    mcp_logins: Arc<mcp::login::Logins>,
    provider_logins: Arc<pi::providers::Logins>,
    /// Temporary Side Chats: engine-hosted chats opened from a
    /// settled selection. Owned HERE (not by [`EngineRpc`]) so every RPC
    /// service built from this core shares one manager and shutdown reaps
    /// unpromoted chats.
    pub side_chats: SideChats,
    /// Session Fork (v1): clone a settled transcript prefix into a NEW
    /// durable root Pi chat on the source chat's host device.
    pub session_forks: SessionForks,
    pub device_id: String,
    /// Local→synced profile import (account-scoped runtimes only).
    pub local_import: Option<host::local_import::LocalImporter>,
    workspace_scope: WorkspaceScope,
    /// Auth service (attached by [`crate::Engine::assemble_runtime`]; a lazy
    /// dev-mode instance otherwise).
    auth: Mutex<Option<Auth>>,
    /// Peer link cache for `targetDeviceId` routing (attached when edge+auth are ready).
    links: Mutex<Option<Arc<cypher_rpc::LinkCache>>>,
    /// Release checker (attached by [`Engine::assemble_runtime`]) — the
    /// UpdateStatus stream + ApplyUpdate.
    updater: Mutex<Option<cypher_update::Updater>>,
    /// Downloaded, Cypher-owned Pi runtime + six-hour runtime update checker.
    pi_runtime: Mutex<Option<pi::runtime::PiRuntimeManager>>,
    /// The updater's token-change wake forwarder — owned so shutdown can end it.
    updater_wake: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Reloads Pi discovery + parked sessions after a background Runtime
    /// install (see [`Self::set_pi_runtime`]) — owned so shutdown can end it.
    pi_runtime_reload: Mutex<Option<tokio::task::JoinHandle<()>>>,
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

    /// Test seam: assemble a synced profile for an explicit org/user identity.
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
                edge,
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
            && let Some(root) = host::local_import::marker_grants_read_root(
                data_dir,
                profile.org_id(),
                profile.user_id(),
            )
        {
            uploads.add_read_only_root(&root);
        }
        let local_import = (profile.scope() == WorkspaceScope::Synced).then(|| {
            host::local_import::LocalImporter::new(
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
        let title_settings = session::title_settings::TitleSettingsStore::new(data_dir);
        let github = git::github::Github::new(git::github::GithubConfig::detect(), data_dir);
        sessions.set_titles(
            TitleGenerator::new(workspace.clone(), registry.clone(), repos.clone())
                .with_settings(title_settings.clone()),
        );
        let diff_sync = CheckoutDiffSync::start(repos.clone(), workspace.clone(), &device_id);
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
            title_settings,
            github,
            mcp_logins: Arc::new(Default::default()),
            provider_logins: Arc::new(Default::default()),
            side_chats,
            session_forks,
            device_id,
            local_import,
            workspace_scope: profile.scope(),
            auth: Mutex::new(None),
            links: Mutex::new(None),
            updater: Mutex::new(None),
            pi_runtime: Mutex::new(None),
            updater_wake: Mutex::new(None),
            pi_runtime_reload: Mutex::new(None),
            _instance_lock: lock,
        })
    }

    pub fn workspace_scope(&self) -> WorkspaceScope {
        self.workspace_scope
    }

    /// Attach the auth service (before building the RPC service / relays).
    pub fn set_auth(&self, auth: Auth) {
        let workspace = self.workspace.clone();
        let expected_user = workspace.user_id().to_string();
        let expected_org = workspace.org_id().to_string();
        workspace.set_notification_event_hook(host::notification_events::hook(
            auth.clone(),
            expected_user,
            expected_org,
        ));
        *lock(&self.auth) = Some(auth);
    }

    /// The attached auth service, or a lazily-created dev-mode one (in-process embeds
    /// that never wired WorkOS still answer AuthStatus honestly).
    pub fn auth(&self) -> Auth {
        let mut slot = lock(&self.auth);
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

    /// Attach the peer link cache — enables `targetDeviceId` routing.
    pub fn set_links(&self, links: Arc<cypher_rpc::LinkCache>) {
        *lock(&self.links) = Some(links);
    }

    pub fn links(&self) -> Option<Arc<cypher_rpc::LinkCache>> {
        lock(&self.links).clone()
    }

    /// Attach the release checker (before building the RPC service).
    pub fn set_updater_wake(&self, handle: tokio::task::JoinHandle<()>) {
        *lock(&self.updater_wake) = Some(handle);
    }

    pub fn set_updater(&self, updater: cypher_update::Updater) {
        *lock(&self.updater) = Some(updater);
    }

    pub fn updater(&self) -> Option<cypher_update::Updater> {
        lock(&self.updater).clone()
    }

    /// Attach the Runtime manager. Every Runtime activation it performs —
    /// the six-hourly background install included — must reload Pi
    /// discovery and recycle parked children, exactly like the Settings →
    /// Install RPC path does, or the harness keeps serving the previous
    /// bundle's model catalog (minus any newly bundled provider) until the
    /// engine restarts.
    pub fn set_pi_runtime(&self, runtime: pi::runtime::PiRuntimeManager) {
        let registry = self.registry.clone();
        let sessions = self.sessions.clone();
        let reload = runtime.spawn_reload_on_install(move || {
            let registry = registry.clone();
            let sessions = sessions.clone();
            async move {
                tracing::info!("pi runtime replaced; reloading pi discovery and parked sessions");
                registry.invalidate_discovery(HarnessId::Pi);
                sessions.recycle_idle_sessions().await;
            }
        });
        if let Some(previous) = lock(&self.pi_runtime_reload).replace(reload) {
            previous.abort();
        }
        *lock(&self.pi_runtime) = Some(runtime);
    }

    pub fn pi_runtime(&self) -> Option<pi::runtime::PiRuntimeManager> {
        lock(&self.pi_runtime).clone()
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
            self.side_chats.clone(),
            self.session_forks.clone(),
            self.workspace_scope,
        )
        .with_auth(self.auth())
        .with_mcp_logins(self.mcp_logins.clone())
        .with_provider_logins(self.provider_logins.clone())
        .with_title_settings(self.title_settings.clone())
        .with_github(self.github.clone());
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
        self.mcp_logins.cancel_all();
        self.provider_logins.cancel_all();
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
        self.mcp_logins.cancel_all();
        self.provider_logins.cancel_all();
        // Reap temporary Side Chats FIRST: interrupt their live runs and drop
        // every ephemeral doc (host-memory only — dispose leaves no durable
        // remnants; a promoted chat is untouched) BEFORE the general session
        // teardown settles/flushes the remaining normal chats.
        self.side_chats.shutdown().await;
        self.sessions.shutdown().await;
        self.terminals.shutdown();
        // Cancel + await every worker that can reach Edge before flushing: a
        // replaced synced runtime must not keep polling releases or draining
        // the attachment outbox under the old identity after Local boots.
        let wake = lock(&self.updater_wake).take();
        if let Some(wake) = wake {
            wake.abort();
            let _ = wake.await;
        }
        let updater = lock(&self.updater).take();
        if let Some(updater) = updater {
            updater.shutdown().await;
        }
        let reload = lock(&self.pi_runtime_reload).take();
        if let Some(reload) = reload {
            reload.abort();
            let _ = reload.await;
        }
        let pi_runtime = lock(&self.pi_runtime).take();
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

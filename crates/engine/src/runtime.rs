//! Runtime assembly: [`Engine`] resolves auth and the workspace profile and
//! builds an [`EngineRuntime`] (core + device-room relay). Also the IPC
//! server entry point and the process shutdown signal.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cypher_proto::{EngineInfo, HarnessId, WorkspaceScope};

use crate::device_identity::load_or_create_device_id;
use crate::pi;
use crate::registry::default_registry_with_bridge_and_runtime;
use crate::util::{env_or, lock};
use crate::{
    Auth, AuthConfig, DEFAULT_ORG_ID, DEFAULT_USER_ID, EdgeConfig, EngineCore, EngineError,
    EngineProfile, InstanceLock,
};

#[derive(Clone)]
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

impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("data_dir", &self.data_dir)
            .field("edge_url", &self.edge_url)
            .field("org_id", &self.org_id)
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, feature = "development"))]
mod development_auth_tests {
    use super::*;
    #[tokio::test]
    async fn secret_never_becomes_user_identity_or_debug_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = EngineConfig {
            data_dir: dir.path().into(),
            edge_url: String::new(),
            edge_token: None,
            ipc_socket: cypher_env::ipc_socket(dir.path()).unwrap(),
            default_harness: HarnessId::Mock,
            org_id: Some("dev-org".into()),
            workos_client_id: None,
        };
        config.edge_url = "http://127.0.0.1:27640".into();
        let secret = "a".repeat(64);
        config.edge_token = Some(secret.clone());
        config.workos_client_id = None;
        let auth = Engine::build_auth(&config).await;
        assert_eq!(auth.access_token().await.as_deref(), Some(secret.as_str()));
        assert_eq!(auth.user_id().as_deref(), Some("dev-user"));
        assert_eq!(auth.state().user().unwrap().id, "dev-user");
        assert!(!format!("{config:?}").contains(&secret));
        let profile = Engine::resolve_profile(&config, &auth, WorkspaceScope::Development)
            .unwrap()
            .unwrap();
        assert!(!format!("{:?}", profile.store_root()).contains(&secret));
    }
}

/// Entry points that resolve auth and a workspace profile and assemble an
/// [`EngineRuntime`]; shared by the headless server and the headed app.
pub struct Engine;

/// A fully assembled identity-scoped engine plus the relay handle whose lifetime
/// keeps this device reachable. Used by both the headless server and the headed
/// in-process engine so their production authentication paths cannot diverge.
pub struct EngineRuntime {
    core: EngineCore,
    host_relay: Mutex<Option<cypher_rpc::HostRelay>>,
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
        lock(&self.host_relay).take();
        self.core.disconnect_edge();
    }

    pub async fn shutdown(&self) {
        self.disconnect_edge();
        self.core.shutdown().await;
    }
}

impl Drop for EngineRuntime {
    fn drop(&mut self) {
        lock(&self.host_relay).take();
    }
}

/// Is this Edge URL a development endpoint, entitled to the locked `dev-user`
/// identity and the preview relay?
///
/// The hosted development Worker was retired on 2026-09-22, so a loopback
/// `wrangler dev` is the default answer. `CYPHER_DEV_EDGE_URL` may name a
/// self-hosted staging endpoint instead; the client-side guard that keeps a
/// development bearer away from production lives in `apps/cypher`
/// (`development_edge_is_safe`), and this only has to agree with it.
#[cfg(feature = "development")]
fn is_development_edge(edge_url: &str) -> bool {
    let trimmed = edge_url.trim_end_matches('/');
    if cypher_env::var("DEV_EDGE_URL").is_some_and(|configured| {
        configured
            .trim_end_matches('/')
            .eq_ignore_ascii_case(trimmed)
    }) {
        return true;
    }
    reqwest::Url::parse(edge_url).is_ok_and(|url| {
        matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
    })
}

impl Engine {
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
            #[cfg(feature = "development")]
            if is_development_edge(&config.edge_url) {
                auth_config.dev_user_id = "dev-user".into();
                auth_config.dev_access_token = Some(token.clone());
            }
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
    ) -> Result<EngineRuntime, EngineError> {
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
    ) -> Result<EngineRuntime, EngineError> {
        Self::assemble_runtime_inner(config, auth, profile, Some(lock)).await
    }

    async fn assemble_runtime_inner(
        config: &EngineConfig,
        auth: Auth,
        profile: EngineProfile,
        lock: Option<InstanceLock>,
    ) -> Result<EngineRuntime, EngineError> {
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
        #[allow(unused_mut)]
        let mut edge = edge_enabled.then(|| {
            EdgeConfig::new(config.edge_url.clone(), Arc::new(auth.clone()))
                .with_device(device_id)
                .with_viewport_activity(auth.viewport_activity())
        });
        #[cfg(feature = "development")]
        if std::env::var("CYPHER_DEV_STREAM_PREVIEW").as_deref() == Ok("1") {
            let url = reqwest::Url::parse(&config.edge_url)
                .map_err(|err| EngineError::Other(err.to_string()))?;
            if !(is_development_edge(&config.edge_url)
                && matches!(url.scheme(), "http" | "https")
                && profile.scope() == WorkspaceScope::Development
                && config.edge_token.is_some())
            {
                return Err(EngineError::Other(
                    "Preview requires an isolated development Edge".into(),
                ));
            }
            let token = std::env::var("CYPHER_DEV_PREVIEW_PUBLISH_TOKEN").ok();
            if !token.as_ref().is_none_or(|t| {
                t.len() == 64
                    && t.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && Some(t) != config.edge_token.as_ref()
            }) {
                return Err(EngineError::Other(
                    "Invalid independent preview publishing credential".into(),
                ));
            }
            if let Some(edge) = edge.as_mut() {
                edge.preview = Some(cypher_sync::preview_link::PreviewOptions {
                    publisher_token: token,
                });
            }
        }

        // The cypher-owned pi session store (`pi --mode rpc --session-dir`).
        let pi_sessions_root = profile.store_root().join("agent-sessions");
        // The executable/config boundary is device-global and fixed before
        // first-run installation. No system Pi fallback: publishing the
        // `current` symlink makes the lazy harness become installed.
        let pi_runtime_data_dir = profile.device_root().to_path_buf();
        let pi_runtime_paths = pi::runtime::PiRuntimePaths::for_data_dir(profile.device_root());
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
        // resubscribed every 2s forever). Token changes only expedite a failed
        // check or a new sign-in, not every healthy token rotation.
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
            let auth_for_updates = auth.clone();
            let mut signed_in = auth.state().is_signed_in();
            let wake = tokio::spawn(async move {
                while token_changes.changed().await.is_ok() {
                    let now_signed_in = auth_for_updates.state().is_signed_in();
                    updater_for_tokens
                        .check_after_auth_change(now_signed_in, !signed_in && now_signed_in);
                    signed_in = now_signed_in;
                }
            });
            core.set_updater_wake(wake);
        }
        core.set_updater(updater);
        let pi_runtime =
            pi::runtime::PiRuntimeManager::spawn(config.edge_url.clone(), &pi_runtime_data_dir);
        // Assembly holds the data-dir instance lock, so no other engine has Pi
        // children running from this runtime tree: keep only the live bundle.
        pi_runtime.enable_cleanup();
        core.set_pi_runtime(pi_runtime);
        tracing::info!(device_id = %core.device_id, "engine core assembled");
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
}

/// Ctrl-C, SIGTERM, or SSH terminal hangup. systemd/launchd stop (and the auto-updater's service
/// restart) deliver SIGTERM — without catching it the daemon dies mid-write
/// and every stop takes the crash-recovery path instead of the graceful drain.
pub async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        // SIGHUP is an interactive hangup (SSH/TTY). `nohup` and launchd
        // processes have no controlling terminal; treating HUP as stop made
        // detached `cypher headless` exit as soon as the launching shell
        // closed, which then showed `connection closed` in every Settings page.
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let mut sighup =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result,
                _ = sigterm.recv() => Ok(()),
                _ = sighup.recv() => Ok(()),
            }
        } else {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result,
                _ = sigterm.recv() => Ok(()),
            }
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

//! The headless engine process: assemble the runtime, serve IPC, and drain
//! on a signal, an IPC stop request, or sign-out.

use std::sync::Arc;

use async_trait::async_trait;
use cypher_proto::WorkspaceScope;
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};

use crate::{Auth, AuthState, Engine, EngineConfig, EngineError, InstanceLock, shutdown_signal};

/// IPC-only lifecycle control owned by `cypher headless`. The regular
/// [`EngineRpc`](crate::rpc::EngineRpc) deliberately does not expose this method, so a viewport
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
        // Let the unary success frame reach the client before `Engine::run_headless`
        // aborts the IPC server and drains the runtime.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = stop_tx.send(());
        });
        RpcReply::value(&serde_json::json!({ "ok": true }))
    }
}

impl Engine {
    /// `cypher headless`: run until ctrl-c, an IPC stop request, or (synced)
    /// sign-out: auth (dev or WorkOS), sessions engine + doc host + command
    /// executor, IPC server, and — when edge+auth are ready — the device-room
    /// host relay + peer link cache (targetDeviceId routing).
    ///
    /// `onboard` runs only when a captured synced session has no workspace
    /// yet; the CLI passes its terminal sign-in flow, which owns all prompts.
    pub async fn run_headless<F, Fut>(config: EngineConfig, onboard: F) -> Result<(), EngineError>
    where
        F: FnOnce(Auth) -> Fut,
        Fut: Future<Output = Result<(), EngineError>>,
    {
        if config.ipc_socket != cypher_env::ipc_socket(&config.data_dir).map_err(startup_io)? {
            return Err(EngineError::Other(
                "Engine IPC socket does not match its data directory".into(),
            ));
        }
        tracing::info!(data_dir = %config.data_dir.display(), "engine starting");

        std::fs::create_dir_all(&config.data_dir).map_err(startup_io)?;
        // Auth construction can persist a sanitized session, and its refresh
        // loop rotates single-use credentials. Own the directory BEFORE either.
        let lock = InstanceLock::acquire(&config.data_dir)?;
        let listener = cypher_rpc::LocalListener::bind(&config.ipc_socket)
            .await
            .map_err(startup_io)?;
        let auth = Engine::build_auth(&config).await;
        let mut auth_state = auth.watch_state();
        let workspace_scope = Engine::initial_workspace_scope(&auth);
        let mut profile = Engine::resolve_profile(&config, &auth, workspace_scope)?;
        let _refresh_loop = tokio_util::task::AbortOnDropHandle::new(auth.spawn_refresh_loop());

        // A captured cloud session without an organization must finish onboarding
        // before its profile can open. A clean signed-out install is local and never
        // enters the terminal sign-in flow.
        if workspace_scope == WorkspaceScope::Synced && profile.is_none() {
            onboard(auth.clone()).await?;
            profile = Engine::resolve_profile(&config, &auth, workspace_scope)?;
        }
        let profile = profile
            .ok_or_else(|| EngineError::Other("synced workspace profile is not ready".into()))?;

        let runtime = Engine::assemble_runtime_with_lock(&config, auth, profile, lock).await?;

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

/// Startup I/O failures read as the bare OS message (`cypher headless` prints
/// `Error: <message>`), as they did before the engine had its own error type.
fn startup_io(err: std::io::Error) -> EngineError {
    EngineError::Other(err.to_string())
}

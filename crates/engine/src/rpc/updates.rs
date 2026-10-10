//! Engine and Pi update checks and installs.

use serde_json::Value;

use super::*;

pub(super) async fn apply_update(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    // `force` skips the idle guard (active runs / open terminals).
    let force = params
        .get("force")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let version = rpc
        .updater()?
        .apply(force)
        .await
        .map_err(|e| RpcError::Failed(format!("{e:#}")))?;
    RpcReply::value(&serde_json::json!({ "ok": true, "version": version }))
}

/// One manual sweep: the scheduled six-hour check on demand, with
/// its auto-apply intact. `spawn_reload_on_install` reloads Pi for
/// an install started here exactly as it does for the timer.
pub(super) async fn check_pi_update(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    rpc.pi_runtime()?.check_updates().await;
    RpcReply::value(&rpc.pi_runtime()?.update_status())
}

pub(super) async fn apply_pi_updates(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    rpc.pi_runtime()?
        .install_latest()
        .await
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&rpc.pi_runtime()?.update_status())
}

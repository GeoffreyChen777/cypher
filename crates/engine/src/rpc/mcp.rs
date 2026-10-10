//! MCP server configuration and sign-in. The MCP helpers read and rewrite
//! config files: blocking work, so each runs on the blocking pool.

use serde_json::Value;

use super::*;

pub(super) async fn list_mcp_servers(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let paths = rpc.pi_runtime()?.paths().clone();
    let snapshot = crate::off_runtime(move || crate::mcp::list(&paths))
        .await
        .map_err(RpcError::Failed)?;
    RpcReply::value(&snapshot)
}

pub(super) async fn add_mcp_servers(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let request = serde_json::from_value::<crate::mcp::AddMcpServers>(body)
        .map_err(|_| RpcError::BadParams("Invalid MCP configuration.".into()))?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let snapshot = crate::off_runtime(move || crate::mcp::add_servers(&paths, request))
        .await
        .and_then(|result| result)
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&snapshot)
}

pub(super) async fn set_mcp_server_enabled(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: crate::mcp::SetMcpServerEnabled = parse_params(params)?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let snapshot = crate::off_runtime(move || crate::mcp::set_enabled(&paths, p))
        .await
        .and_then(|result| result)
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&snapshot)
}

pub(super) async fn remove_mcp_server(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let body = strip_target(params);
    let request = serde_json::from_value::<crate::mcp::RemoveMcpServer>(body)
        .map_err(|_| RpcError::BadParams("Invalid MCP deletion request.".into()))?;
    if rpc.sessions.any_active() {
        return Err(RpcError::Failed(
            "Finish or stop active runs on this device before deleting an MCP server.".into(),
        ));
    }
    rpc.sessions.recycle_idle_sessions().await;
    let paths = rpc.pi_runtime()?.paths().clone();
    let result = crate::off_runtime(move || crate::mcp::remove_server(&paths, request))
        .await
        .and_then(|result| result);
    rpc.registry.invalidate_discovery(HarnessId::Pi);
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}

pub(super) fn begin_mcp_login(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: crate::mcp::McpServerName = parse_params(params)?;
    let harness = rpc
        .registry
        .resolve(HarnessId::Pi)
        .map_err(|_| RpcError::Failed("Pi runtime unavailable.".into()))?;
    let status = rpc
        .mcp_logins
        .begin(rpc.pi_runtime()?.paths(), p.name, harness)
        .map_err(RpcError::Failed)?;
    RpcReply::value(&status)
}

pub(super) async fn mcp_login(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let id = params
        .get("attemptId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| id.len() <= 64)
        .ok_or_else(|| RpcError::BadParams("MCP sign-in attempt ID required.".into()))?;
    let status = match method {
        methods::COMPLETE_MCP_LOGIN => {
            let callback = params
                .get("callbackUrl")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RpcError::BadParams("Callback URL required.".into()))?;
            rpc.mcp_logins.respond(id, callback)
        }
        methods::CANCEL_MCP_LOGIN => rpc.mcp_logins.cancel(id),
        _ => rpc.mcp_logins.status(id),
    }
    .map_err(RpcError::Failed)?;
    if status.phase == "succeeded" {
        rpc.reload_pi_runtime().await;
    }
    RpcReply::value(&status)
}

pub(super) async fn start_mcp_auth(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: crate::mcp::McpServerName = parse_params(params)?;
    let harness = rpc
        .registry
        .resolve(cypher_proto::HarnessId::Pi)
        .map_err(failed)?;
    let snapshot = crate::mcp::authenticate(rpc.pi_runtime()?.paths(), &p.name, harness.as_ref())
        .await
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&snapshot)
}

pub(super) async fn logout_mcp_server(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: crate::mcp::McpServerName = parse_params(params)?;
    let paths = rpc.pi_runtime()?.paths().clone();
    let snapshot = crate::off_runtime(move || crate::mcp::logout(&paths, &p.name))
        .await
        .and_then(|result| result)
        .map_err(RpcError::Failed)?;
    rpc.reload_pi_runtime().await;
    RpcReply::value(&snapshot)
}

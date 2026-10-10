//! Harness catalog toggles and slash-command discovery.

use serde_json::Value;

use super::*;

pub(super) fn set_harness_enabled(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: SetHarnessEnabledParams = parse_params(params)?;
    rpc.registry
        .set_enabled(p.harness, p.enabled)
        .map_err(RpcError::Failed)?;
    // Fresh catalog in the reply: the page repaints from it in one
    // round trip, and a refused/raced toggle self-corrects.
    RpcReply::value(&rpc.registry.descriptors())
}

pub(super) async fn list_commands(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    // Same shape as ListModels: forces a lazy resolve, then the
    // harness's own (cached) discovery. A harness without slash commands
    // returns an empty list from the trait default.
    let p: ListModelsParams = parse_params(params)?;
    let harness = rpc.registry.resolve(p.harness).map_err(failed)?;
    let commands = harness.commands().await.map_err(failed)?;
    RpcReply::value(&commands)
}

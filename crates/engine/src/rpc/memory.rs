//! Memory (docs/design/memory.md): the agents' memory tool calls, and the
//! app's view and edits of one store.

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryToolParams {
    chat_id: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    tool_call_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScopeParams {
    scope: cypher_proto::MemoryScope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditParams {
    scope: cypher_proto::MemoryScope,
    edit: cypher_proto::MemoryEdit,
}

/// A refusal is an answer the model should read, so tool failures come
/// back as text; only a malformed call is an RPC error.
pub(super) async fn memory_tool(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: MemoryToolParams = parse_params(params)?;
    let _ = p.tool_call_id;
    let args = if p.args.is_null() {
        serde_json::json!({})
    } else {
        p.args
    };
    let text = match rpc.memory()?.tool(&p.chat_id, &p.tool, args).await {
        Ok(text) => text,
        Err(err) => err.to_string(),
    };
    RpcReply::value(&serde_json::json!({ "text": text }))
}

pub(super) fn watch_memory(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: ScopeParams = parse_params(params)?;
    let stream = rpc.memory()?.watch(p.scope).map_err(failed)?;
    Ok(RpcReply::Stream(stream))
}

pub(super) async fn edit_memory(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: EditParams = parse_params(params)?;
    let text = rpc.memory()?.edit(p.scope, p.edit).await.map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "text": text }))
}

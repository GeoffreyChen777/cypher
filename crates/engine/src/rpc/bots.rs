//! Agent bots: creation on the host and the bot tool surface.

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BotToolParams {
    chat_id: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
}

pub(super) fn create_bot(rpc: &EngineRpc, params: serde_json::Value) -> Result<RpcReply, RpcError> {
    let request: crate::bots::CreateBot = parse_params(strip_target(params))?;
    let bot = rpc.bots()?.create(request).map_err(failed)?;
    RpcReply::value(&bot)
}

/// A refusal is still an answer the model should read, so tool failures
/// come back as text; only a malformed call is an RPC error.
pub(super) async fn bot_tool(
    rpc: &EngineRpc,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    let p: BotToolParams = parse_params(params)?;
    let args = if p.args.is_null() {
        serde_json::json!({})
    } else {
        p.args
    };
    let text = match rpc.bots()?.tool(&p.chat_id, &p.tool, args).await {
        Ok(text) => text,
        Err(err) => err.to_string(),
    };
    RpcReply::value(&serde_json::json!({ "text": text }))
}

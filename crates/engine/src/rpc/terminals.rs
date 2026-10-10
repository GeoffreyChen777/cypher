//! Terminals hosted on this device.

use serde_json::Value;

use super::*;

pub(super) fn open_terminal(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: OpenTerminalParams = parse_params(params)?;
    // The terminal runs in the chat's checkout; a chat with no cwd (or
    // no row yet) gets the home directory. A project-less chat stores
    // the literal `~` — expand it here or the shell never spawns.
    let cwd = rpc
        .workspace
        .chat(&p.chat_id)
        .ok()
        .flatten()
        .and_then(|chat| chat.cwd)
        .map(|cwd| expand_home(&cwd))
        .unwrap_or_else(|| home_dir().to_string_lossy().to_string());
    let session = rpc.terminals.open(&cwd, p.cols, p.rows).map_err(failed)?;
    RpcReply::value(&session)
}

pub(super) fn subscribe_terminal(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: SubscribeTerminalParams = parse_params(params)?;
    let rx = rpc
        .terminals
        .subscribe(&p.terminal_id, p.after_seq)
        .map_err(failed)?;
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let event = rx.recv().await?;
        let value = serde_json::to_value(&event).ok()?;
        Some((value, rx))
    });
    Ok(RpcReply::Stream(stream.boxed()))
}

pub(super) fn write_terminal(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: WriteTerminalParams = parse_params(params)?;
    rpc.terminals
        .write(&p.terminal_id, &p.data)
        .map_err(failed)?;
    RpcReply::ok()
}

pub(super) fn resize_terminal(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: ResizeTerminalParams = parse_params(params)?;
    rpc.terminals
        .resize(&p.terminal_id, p.cols, p.rows)
        .map_err(failed)?;
    RpcReply::ok()
}

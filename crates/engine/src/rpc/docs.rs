//! Session-doc commands and watches, sync status, and the local-profile import.

use serde_json::Value;

use super::*;

pub(super) fn queue_command(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: QueueCommandParams = parse_params(params)?;
    let command_id = rpc
        .doc_host
        .queue_command(&p.chat_id, p.command)
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "commandId": command_id }))
}

pub(super) fn retry_command(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: RetryCommandParams = parse_params(params)?;
    let command_id = rpc
        .doc_host
        .retry_command(&p.chat_id, &p.command_id)
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "commandId": command_id }))
}

pub(super) fn watch_doc_messages(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: ChatParams = parse_params(params)?;
    let handle = rpc.doc_host.open(&p.chat_id).map_err(failed)?;
    Ok(RpcReply::Stream(doc_messages_stream(
        handle.watch_messages(),
    )))
}

pub(super) fn watch_doc_commands(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: ChatParams = parse_params(params)?;
    let handle = rpc.doc_host.open(&p.chat_id).map_err(failed)?;
    // Same watch_stream shape as the other standing watches: the
    // current command ledger first, then every doc change.
    Ok(RpcReply::Stream(watch_stream(handle.watch_commands())))
}

pub(super) fn sync_status(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    fn room_json(s: &cypher_sync::RoomStatsSnapshot) -> serde_json::Value {
        serde_json::json!({
            "connected": s.connected,
            "lastPushedMs": s.last_pushed_ms,
            "lastAckMs": s.last_ack_ms,
            "rejoins": s.rejoins,
            "probes": s.probes,
            "fullResyncs": s.full_resyncs,
            "disconnects": s.disconnects,
            "rejected": s.rejected,
        })
    }
    fn chat2_json(s: &cypher_sync::ChatStatsSnapshot) -> serde_json::Value {
        serde_json::json!({
            "connected": s.connected,
            "cursor": s.cursor,
            "headSeq": s.head_seq,
            "seqFloor": s.seq_floor,
            "checkpointSeq": s.checkpoint_seq,
            "checkpointSize": s.checkpoint_size,
            "rowCount": s.row_count,
            "rowBytes": s.row_bytes,
            "pendingPushes": s.pending_pushes,
            "rejoins": s.rejoins,
            "disconnects": s.disconnects,
            "rejected": s.rejected,
            "serverResets": s.server_resets,
        })
    }
    let workspace = rpc.workspace.sync_status();
    let chats: Vec<serde_json::Value> = rpc
        .doc_host
        .sync_statuses()
        .iter()
        .map(|(chat_id, room)| {
            serde_json::json!({
                "chatId": chat_id,
                "room": room.as_ref().map(chat2_json),
            })
        })
        .collect();
    RpcReply::value(&serde_json::json!({
        "deviceId": rpc.doc_host.device_id(),
        "nowMs": crate::now_ms(),
        "workspace": workspace.as_ref().map(room_json),
        "chats": chats,
    }))
}

pub(super) fn watch_sessions(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    // Local live statuses merged with remote devices' workspace rows.
    let merged = rpc
        .workspace
        .merged_sessions_watch(rpc.sessions.watch_sessions());
    Ok(RpcReply::Stream(watch_stream(merged)))
}

pub(super) async fn local_import_status(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let importer = rpc.local_importer()?.clone();
    let status = tokio::task::spawn_blocking(move || importer.status())
        .await
        .map_err(failed)?
        .map_err(failed)?;
    RpcReply::value(&status)
}

pub(super) fn import_local_workspace(rpc: &EngineRpc) -> Result<RpcReply, RpcError> {
    let importer = rpc.local_importer()?.clone();
    // Progress rides an unbounded channel: the importer is
    // blocking (sqlite + fs) and must never wedge on a slow
    // viewer; items are tiny and bounded by the chat count.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    tokio::task::spawn_blocking(move || {
        let emit = |event: crate::local_import::ImportEvent| {
            if let Ok(item) = serde_json::to_value(&event) {
                let _ = tx.send(item);
            }
        };
        if let Err(err) = importer.run(emit) {
            tracing::error!(error = %err, "local import failed");
            let _ = tx.send(serde_json::json!({
                "kind": "summary",
                "importedChats": 0, "importedSpaces": 0,
                "skippedChats": 0, "skippedSpaces": 0,
                "journalsCopied": 0, "ledgerRowsMerged": 0,
                "errors": [format!("{err}")],
            }));
        }
        // tx drops here — the stream ends after the summary item.
    });
    Ok(RpcReply::Stream(Box::pin(futures::stream::poll_fn(
        move |cx| rx.poll_recv(cx),
    ))))
}

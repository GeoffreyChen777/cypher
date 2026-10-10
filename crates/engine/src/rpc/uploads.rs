//! Attachment uploads, attachment reads and tool-output blobs.

use serde_json::Value;

use super::*;

pub(super) fn upload_chunk(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: UploadChunkParams = parse_params(params)?;
    rpc.uploads
        .append(&p.upload_id, &p.data, p.seq)
        .map_err(failed)?;
    RpcReply::ok()
}

pub(super) fn upload_commit(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: UploadCommitParams = parse_params(params)?;
    let path = rpc
        .uploads
        .commit(&p.upload_id, &p.file_name)
        .map_err(failed)?;
    if let Some(chat_id) = &p.chat_id {
        // Queue-first send: seal against the chat so its host's
        // drain releases the waiting Run. Best-effort — the
        // durable path is already committed; an unsealable chat
        // just leaves the Run's grace window to expire it.
        rpc.doc_host
            .seal_attachment(chat_id, &p.upload_id, &path, &p.file_name);
    }
    RpcReply::value(&serde_json::json!({ "path": path }))
}

pub(super) fn read_attachment_chunk(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: ReadAttachmentChunkParams = parse_params(params)?;
    // Path jail: the uploads dir plus every workspace-known chat cwd.
    let roots: Vec<std::path::PathBuf> = rpc
        .workspace
        .read_chats()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|chat| chat.cwd)
        .map(|cwd| std::path::PathBuf::from(expand_home(&cwd)))
        .collect();
    let chunk = rpc
        .uploads
        .read_chunk(&p.path, p.offset, &roots)
        .map_err(failed)?;
    RpcReply::value(&chunk)
}

pub(super) async fn fetch_tool_blob(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: FetchToolBlobParams = parse_params(params)?;
    let text = rpc
        .doc_host
        .fetch_tool_blob(&p.blob_ref)
        .await
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "text": text }))
}

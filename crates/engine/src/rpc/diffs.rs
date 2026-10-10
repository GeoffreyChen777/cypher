//! One-shot checkout diffs for the Changes pane.

use serde_json::Value;

use super::*;

/// One-shot scoped capture for the Changes pane: `branch` diffs the
/// working tree against merge-base(baseRef, HEAD); `turn` diffs the
/// turn-start tree snapshot against the current tree; anything else
/// is the plain working-tree capture.
pub(super) async fn get_checkout_diff(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: CheckoutDiffParams = parse_params(params)?;
    let identity = rpc
        .repos
        .checkout_identity(std::path::Path::new(&p.cwd))
        .await
        .map_err(failed)?;
    let root = identity.root.as_path();
    let snapshot = match p.mode.as_str() {
        "branch" => {
            let base_ref = p
                .base_ref
                .as_deref()
                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
            let base = crate::diff_sync::merge_base(root, base_ref)
                .await
                .map_err(failed)?;
            crate::diff_sync::capture_diff_against(&rpc.repos, root, Some(&base)).await
        }
        // One commit's own changes (History → per-commit tab):
        // parent (or the empty tree) vs the commit itself.
        "commit" => {
            let sha = p
                .commit_sha
                .as_deref()
                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
            crate::diff_sync::capture_commit_diff(&rpc.repos, root, sha).await
        }
        "turn" => {
            let chat_id = p
                .chat_id
                .as_deref()
                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
            let snapshot = rpc
                .diff_sync
                .turn_snapshot(chat_id)
                .filter(|s| s.root == identity.root)
                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
            crate::diff_sync::capture_turn_diff(&rpc.repos, root, &snapshot.tree).await
        }
        _ => crate::diff_sync::capture_diff(&rpc.repos, root).await,
    }
    .map_err(failed)?;
    RpcReply::value(&cypher_proto::CheckoutDiff {
        checkout_id: identity.id,
        device_id: rpc.doc_host.device_id().to_string(),
        cwd: identity.root.to_string_lossy().to_string(),
        patch: snapshot.patch,
        files: snapshot.files,
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        truncated: snapshot.truncated,
        checksum: snapshot.checksum,
        updated_at: chrono::Utc::now(),
    })
}

pub(super) async fn get_checkout_file_diff_text(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: cypher_proto::GetCheckoutFileDiffTextRequest = parse_params(params)?;
    let identity = Box::pin(rpc.repos.checkout_identity(std::path::Path::new(&p.cwd)))
        .await
        .map_err(failed)?;
    if identity.id != p.checkout_id {
        return Err(RpcError::Failed("checkoutId does not match cwd".into()));
    }
    let root = identity.root.as_path();
    let (snapshot, base, target) = match p.mode.as_str() {
        "branch" => {
            let base_ref = p
                .base_ref
                .as_deref()
                .ok_or_else(|| RpcError::Failed("baseRef required".into()))?;
            let base = Box::pin(crate::diff_sync::merge_base(root, base_ref))
                .await
                .map_err(failed)?;
            let snapshot = Box::pin(crate::diff_sync::capture_diff_against(
                &rpc.repos,
                root,
                Some(&base),
            ))
            .await
            .map_err(failed)?;
            (snapshot, base, None)
        }
        "commit" => {
            let sha = p
                .commit_sha
                .as_deref()
                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
            let base = Box::pin(crate::diff_sync::commit_diff_base(root, sha)).await;
            let snapshot = Box::pin(crate::diff_sync::capture_commit_diff(&rpc.repos, root, sha))
                .await
                .map_err(failed)?;
            (snapshot, base, Some(sha.to_string()))
        }
        "turn" => {
            let chat_id = p
                .chat_id
                .as_deref()
                .ok_or_else(|| RpcError::Failed("chatId required".into()))?;
            let turn = rpc
                .diff_sync
                .turn_snapshot(chat_id)
                .filter(|snapshot| snapshot.root == identity.root)
                .ok_or_else(|| RpcError::Failed("no turn recorded".into()))?;
            let snapshot = Box::pin(crate::diff_sync::capture_turn_diff(
                &rpc.repos, root, &turn.tree,
            ))
            .await
            .map_err(failed)?;
            (snapshot, turn.tree, None)
        }
        _ => {
            let base = Box::pin(crate::diff_sync::working_diff_base(root))
                .await
                .map_err(failed)?;
            let snapshot = Box::pin(crate::diff_sync::capture_diff(&rpc.repos, root))
                .await
                .map_err(failed)?;
            (snapshot, base, None)
        }
    };
    let stale = || cypher_proto::CheckoutFileDiffText {
        diff_checksum: p.diff_checksum.clone(),
        old_text: None,
        new_text: None,
        old_content_hash: None,
        new_content_hash: None,
        binary: false,
        truncated: false,
        stale: true,
    };
    if snapshot.checksum != p.diff_checksum {
        return RpcReply::value(&stale());
    }
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path == p.path)
        .ok_or_else(|| RpcError::Failed("path is not part of diff snapshot".into()))?;
    let pair = Box::pin(crate::diff_sync::read_diff_file_text_at(
        root,
        &base,
        target.as_deref(),
        file,
    ))
    .await
    .map_err(failed)?;
    let current = match p.mode.as_str() {
        "branch" => {
            Box::pin(crate::diff_sync::capture_diff_against(
                &rpc.repos,
                root,
                Some(&base),
            ))
            .await
        }
        "turn" => Box::pin(crate::diff_sync::capture_turn_diff(&rpc.repos, root, &base)).await,
        "commit" => {
            let sha = p
                .commit_sha
                .as_deref()
                .ok_or_else(|| RpcError::Failed("commitSha required".into()))?;
            Box::pin(crate::diff_sync::capture_commit_diff(&rpc.repos, root, sha)).await
        }
        _ => Box::pin(crate::diff_sync::capture_diff(&rpc.repos, root)).await,
    }
    .map_err(failed)?;
    if current.checksum != p.diff_checksum {
        return RpcReply::value(&stale());
    }
    RpcReply::value(&cypher_proto::CheckoutFileDiffText {
        diff_checksum: p.diff_checksum,
        old_text: pair.old_text,
        new_text: pair.new_text,
        old_content_hash: pair.old_content_hash,
        new_content_hash: pair.new_content_hash,
        binary: pair.binary,
        truncated: pair.truncated,
        stale: false,
    })
}

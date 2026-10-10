//! Git repository queries and worktrees.

use serde_json::Value;

use super::*;

pub(super) async fn list_branches(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: RepoPathParams = parse_params(params)?;
    let branches = rpc
        .repos
        .branches(std::path::Path::new(&p.repo_path))
        .await
        .map_err(failed)?;
    RpcReply::value(&branches)
}

pub(super) async fn list_refs(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: RepoPathParams = parse_params(params)?;
    let refs = rpc
        .repos
        .refs(std::path::Path::new(&p.repo_path))
        .await
        .map_err(failed)?;
    RpcReply::value(&refs)
}

pub(super) async fn list_git_history(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: GitHistoryParams = parse_params(params)?;
    let history = rpc
        .repos
        .history(std::path::Path::new(&p.cwd), p.cursor, p.limit)
        .await
        .map_err(failed)?;
    RpcReply::value(&history)
}

pub(super) async fn fetch_all(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: RepoPathParams = parse_params(params)?;
    rpc.repos
        .fetch_all(std::path::Path::new(&p.repo_path))
        .await
        .map_err(failed)?;
    // Remote refs are repository state too. Force the checkout
    // watchers to publish a fresh snapshot instead of waiting for
    // the repair tick (some platforms do not report packed-refs).
    rpc.diff_sync.sync_all();
    RpcReply::ok()
}

pub(super) async fn switch_ref(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: SwitchRefParams = parse_params(params)?;
    let branch = rpc
        .repos
        .switch_ref(std::path::Path::new(&p.repo_path), &p.ref_name)
        .await
        .map_err(failed)?;
    RpcReply::value(&serde_json::json!({ "branch": branch }))
}

pub(super) async fn create_worktree(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: CreateWorktreeParams = parse_params(params)?;
    let worktree = rpc
        .repos
        .create_worktree(std::path::Path::new(&p.repo_path), &p.branch)
        .await
        .map_err(failed)?;
    RpcReply::value(&worktree)
}

pub(super) async fn delete_worktree(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: DeleteWorktreeParams = parse_params(params)?;
    rpc.repos
        .delete_worktree(
            std::path::Path::new(&p.repo_path),
            std::path::Path::new(&p.worktree_path),
        )
        .await
        .map_err(failed)?;
    RpcReply::ok()
}

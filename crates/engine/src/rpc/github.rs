//! GitHub issue lookup, chat ref previews and the device sign-in flow.

use serde_json::Value;

use super::*;

pub(super) async fn search_github_issues(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: FileSearchParams = parse_params(params)?;
    if p.query.chars().count() > crate::git::github::MAX_QUERY_CHARS {
        return Err(RpcError::BadParams(
            "SearchGithubIssues query must not exceed 256 characters".into(),
        ));
    }
    let github = rpc.github()?;
    let root = rpc.file_search_root(&p).await?;
    let search = github
        .search_issues(&root, &p.query)
        .await
        .map_err(failed)?;
    RpcReply::value(&search)
}

pub(super) async fn get_github_issue(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: GithubIssueParams = parse_params(params)?;
    if !crate::git::github::valid_repo(&p.repo) || p.number == 0 {
        return Err(RpcError::BadParams("invalid GitHub issue reference".into()));
    }
    let snapshot = rpc
        .github()?
        .issue_snapshot(&p.repo, p.number)
        .await
        .map_err(failed)?;
    RpcReply::value(&snapshot)
}

/// `ResolveGitRef`: the hover card of a ref in a chat on this device. An
/// unknown chat or malformed ref is an error; everything about the lookup
/// itself answers quietly (`notFound` / `unavailable`).
pub(super) async fn resolve_git_ref(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: ResolveGitRefParams = parse_params(params)?;
    if !p.git_ref.is_valid() {
        return Err(RpcError::BadParams("invalid git reference".into()));
    }
    let root = rpc
        .chat_file_root(&p.chat_id, p.root_space_id.as_deref())
        .await?;
    let resolution = crate::git::ref_previews::resolve(rpc.github().ok(), &root, &p.git_ref).await;
    RpcReply::value(&resolution)
}

pub(super) fn github_login(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: GithubLoginParams = parse_params(params)?;
    let github = rpc.github()?;
    if method == methods::CANCEL_GITHUB_LOGIN {
        github.cancel_login(&p.login_id);
        RpcReply::ok()
    } else {
        RpcReply::value(&github.poll_login(&p.login_id))
    }
}

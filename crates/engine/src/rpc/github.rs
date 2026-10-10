//! GitHub issue lookup and the device sign-in flow.

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

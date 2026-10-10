//! Workspace file access and mention search, jailed to a chat or space checkout.

use serde_json::Value;

use super::*;

const FILE_SEARCH_RPC_TIMEOUT: Duration = Duration::from_secs(6);
const FILE_SEARCH_FEATURED_PATHS: usize = 32;

pub(super) fn tool_file_path(call: &ToolCall) -> Option<&str> {
    match call {
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. } => Some(path),
        ToolCall::ApplyPatch { path } | ToolCall::Search { path, .. } => path.as_deref(),
        ToolCall::Exec { .. }
        | ToolCall::Glob { .. }
        | ToolCall::WebFetch { .. }
        | ToolCall::WebSearch { .. }
        | ToolCall::Todo { .. }
        | ToolCall::Mcp { .. }
        | ToolCall::Unknown { .. } => None,
    }
}

impl EngineRpc {
    /// Resolve a mention-search root from synced workspace rows. A client may
    /// name an existing linked worktree for a new chat, but it is verified
    /// against the space repository before any filesystem walk begins.
    pub(super) async fn file_search_root(
        &self,
        p: &FileSearchParams,
    ) -> Result<std::path::PathBuf, RpcError> {
        let local_device = self.doc_host.device_id();
        match (&p.chat_id, &p.space_id) {
            (Some(_), Some(_)) | (None, None) => Err(RpcError::BadParams(
                "SearchFiles needs exactly one of chatId or spaceId".into(),
            )),
            (Some(chat_id), None) => {
                if p.path.is_some() {
                    return Err(RpcError::BadParams(
                        "SearchFiles path applies only to a space".into(),
                    ));
                }
                let chat = self
                    .workspace
                    .chat(chat_id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::Failed("chat not found".into()))?;
                if chat.device_id != local_device {
                    return Err(RpcError::Failed("chat belongs to another device".into()));
                }
                let cwd = chat
                    .cwd
                    .map(|cwd| std::path::PathBuf::from(expand_home(&cwd)))
                    .ok_or_else(|| RpcError::Failed("chat has no workspace folder".into()))?;
                let space_id = chat
                    .space_id
                    .ok_or_else(|| RpcError::Failed("chat has no workspace space".into()))?;
                let space = self
                    .workspace
                    .space(&space_id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::Failed("chat workspace space not found".into()))?;
                if space.device_id != local_device {
                    return Err(RpcError::Failed(
                        "chat space belongs to another device".into(),
                    ));
                }
                if let Some(cwd) = self
                    .repos
                    .workspace_checkout(std::path::Path::new(&space.path), &cwd)
                    .await
                {
                    Ok(cwd)
                } else {
                    Err(RpcError::Failed(
                        "chat folder is not a workspace checkout".into(),
                    ))
                }
            }
            (None, Some(space_id)) => {
                let space = self
                    .workspace
                    .space(space_id)
                    .map_err(failed)?
                    .ok_or_else(|| RpcError::Failed("space not found".into()))?;
                if space.device_id != local_device {
                    return Err(RpcError::Failed("space belongs to another device".into()));
                }
                let space_path = std::path::PathBuf::from(&space.path);
                let requested = p
                    .path
                    .as_deref()
                    .map_or_else(|| space_path.clone(), std::path::PathBuf::from);
                if let Some(requested) =
                    self.repos.workspace_checkout(&space_path, &requested).await
                {
                    Ok(requested)
                } else {
                    Err(RpcError::BadParams(
                        "SearchFiles path is not a workspace checkout".into(),
                    ))
                }
            }
        }
    }

    /// Most-recent-first paths the current chat actually touched, followed by
    /// files still changed in its checkout. The search worker validates and
    /// normalizes them against the resolved root before using them as ranking
    /// hints, so stale or out-of-workspace tool paths simply disappear.
    pub(super) fn featured_file_paths(&self, chat_id: &str) -> Vec<String> {
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        if let Ok(handle) = self.doc_host.open(chat_id)
            && let Ok(entries) = handle.doc().read_entries()
        {
            for entry in entries.into_iter().rev() {
                for part in entry.parts.into_iter().rev() {
                    if let MessagePart::Tool { call, .. } = part
                        && let Some(path) = tool_file_path(&call)
                        && !path.trim().is_empty()
                        && seen.insert(path.to_string())
                    {
                        paths.push(path.to_string());
                        if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                            break;
                        }
                    }
                }
                if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                    break;
                }
            }
        }

        if let Ok(Some(chat)) = self.workspace.chat(chat_id) {
            let diffs = self.diff_sync.watch_diffs().borrow().clone();
            let diff = chat
                .checkout_id
                .as_deref()
                .and_then(|id| diffs.iter().find(|diff| diff.checkout_id == id))
                .or_else(|| {
                    // `diff.cwd` is a real canonical checkout root, so the row's
                    // `~` has to be expanded before it can ever match.
                    chat.cwd
                        .as_deref()
                        .map(expand_home)
                        .and_then(|cwd| diffs.iter().find(|diff| diff.cwd == cwd))
                });
            if let Some(diff) = diff {
                for file in &diff.files {
                    if paths.len() == FILE_SEARCH_FEATURED_PATHS {
                        break;
                    }
                    if seen.insert(file.path.clone()) {
                        paths.push(file.path.clone());
                    }
                }
            }
        }
        paths
    }
}

pub(super) async fn workspace_file(
    rpc: &EngineRpc,
    method: &str,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: WorkspaceFileParams = parse_params(params)?;
    let directory = method == methods::LIST_WORKSPACE_FILES;
    let write = (method == methods::WRITE_WORKSPACE_FILE)
        .then(|| {
            p.text
                .clone()
                .ok_or_else(|| RpcError::BadParams("WriteWorkspaceFile needs text".into()))
        })
        .transpose()?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        let same_checkout = || -> Result<(), RpcError> {
            let chat = rpc
                .workspace
                .chat(&p.chat_id)
                .map_err(failed)?
                .ok_or_else(|| RpcError::Failed("chat not found".into()))?;
            if chat.device_id != rpc.doc_host.device_id()
                || chat.cwd.as_deref() != Some(p.cwd.as_str())
            {
                return Err(RpcError::Failed(
                    "chat device or checkout changed; reopen Files".into(),
                ));
            }
            Ok(())
        };
        same_checkout()?;
        let root = rpc
            .file_search_root(&FileSearchParams {
                query: String::new(),
                chat_id: Some(p.chat_id.clone()),
                space_id: None,
                path: None,
            })
            .await?;
        same_checkout()?;
        let value = match write {
            Some(text) => crate::git::workspace_files::write(root, p.path.clone(), text).await,
            None => crate::git::workspace_files::read(root, p.path.clone(), directory).await,
        }
        .map_err(failed)?;
        same_checkout()?;
        RpcReply::value(&value)
    })
    .await
    .map_err(|_| RpcError::Failed("workspace file access timed out".into()))?
}

pub(super) async fn search_files(rpc: &EngineRpc, params: Value) -> Result<RpcReply, RpcError> {
    let p: FileSearchParams = parse_params(params)?;
    if p.query.chars().count() > 256 {
        return Err(RpcError::BadParams(
            "SearchFiles query must not exceed 256 characters".into(),
        ));
    }
    let matches = tokio::time::timeout(FILE_SEARCH_RPC_TIMEOUT, async {
        let root = rpc.file_search_root(&p).await?;
        let featured_paths = p
            .chat_id
            .as_deref()
            .filter(|_| p.query.is_empty())
            .map(|chat_id| rpc.featured_file_paths(chat_id))
            .unwrap_or_default();
        rpc.repos
            .search_files(root, p.query, featured_paths)
            .await
            .map_err(failed)
    })
    .await
    .map_err(|_| RpcError::Failed("file search timed out".into()))??;
    RpcReply::value(&matches)
}

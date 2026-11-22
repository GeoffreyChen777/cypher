//! Workspace file access and mention search, jailed to a chat or space checkout
//! (or, for a workspace's own chat, a project its scope grants).

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

/// How long one root of `ListChatFileRoots` may take to resolve; a project
/// on a dead mount is left out rather than holding up the list.
const ROOT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

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
                self.chat_file_root(chat_id, p.root_space_id.as_deref())
                    .await
            }
            (None, Some(space_id)) => {
                if p.root_space_id.is_some() {
                    return Err(RpcError::BadParams(
                        "SearchFiles rootSpaceId applies only to a chat".into(),
                    ));
                }
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

    /// The folder a chat's file request addresses, canonical. Without
    /// `root_space_id` that is the chat's own folder: a session's checkout,
    /// or a workspace's private folder for the workspace's own chat. With
    /// it, the folder of that project, which only a workspace's own chat may
    /// name and only when its scope grants the project on this device.
    pub(super) async fn chat_file_root(
        &self,
        chat_id: &str,
        root_space_id: Option<&str>,
    ) -> Result<std::path::PathBuf, RpcError> {
        let chat = self.local_chat(chat_id)?;
        match root_space_id {
            None => self.default_file_root(&chat).await,
            Some(space_id) => {
                let bot = self.owner_workspace(&chat)?.ok_or_else(|| {
                    RpcError::Failed("rootSpaceId applies only to a workspace's own chat".into())
                })?;
                Ok(self.granted_project_root(&bot, space_id).await?.0)
            }
        }
    }

    /// A chat row hosted on this device.
    fn local_chat(&self, chat_id: &str) -> Result<cypher_proto::Chat, RpcError> {
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::Failed("chat not found".into()))?;
        if chat.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed("chat belongs to another device".into()));
        }
        Ok(chat)
    }

    /// The workspace `chat` is the own chat of, when that workspace is hosted
    /// here. Hidden threads (memory passes, routines, plans, peer answers),
    /// a marker naming another chat, and other devices' workspaces are not.
    fn owner_workspace(
        &self,
        chat: &cypher_proto::Chat,
    ) -> Result<Option<cypher_proto::Bot>, RpcError> {
        let Some(marker) = chat.bot.as_ref().filter(|marker| marker.is_owner_chat()) else {
            return Ok(None);
        };
        let Some(bot) = self.workspace.bot(&marker.bot_id).map_err(failed)? else {
            return Ok(None);
        };
        Ok(
            (bot.chat_id == chat.id && bot.host_device_id == self.doc_host.device_id())
                .then_some(bot),
        )
    }

    async fn default_file_root(
        &self,
        chat: &cypher_proto::Chat,
    ) -> Result<std::path::PathBuf, RpcError> {
        let cwd = chat
            .cwd
            .as_deref()
            .map(|cwd| std::path::PathBuf::from(expand_home(cwd)))
            .ok_or_else(|| RpcError::Failed("chat has no workspace folder".into()))?;
        if let Some(bot) = self.owner_workspace(chat)? {
            // Only the workspace's own private folder, however the row's
            // cwd was written.
            let folder = self
                .repos
                .canonical_dir(&self.bots()?.folder(&bot.id))
                .await;
            let cwd = self.repos.canonical_dir(&cwd).await;
            return match (folder, cwd) {
                (Some(folder), Some(cwd)) if folder == cwd => Ok(folder),
                _ => Err(RpcError::Failed(
                    "chat folder is not the workspace's folder".into(),
                )),
            };
        }
        let space_id = chat
            .space_id
            .as_deref()
            .ok_or_else(|| RpcError::Failed("chat has no workspace space".into()))?;
        let space = self
            .workspace
            .space(space_id)
            .map_err(failed)?
            .ok_or_else(|| RpcError::Failed("chat workspace space not found".into()))?;
        if space.device_id != self.doc_host.device_id() {
            return Err(RpcError::Failed(
                "chat space belongs to another device".into(),
            ));
        }
        self.repos
            .workspace_checkout(std::path::Path::new(&space.path), &cwd)
            .await
            .ok_or_else(|| RpcError::Failed("chat folder is not a workspace checkout".into()))
    }

    /// A project `bot` grants on this device, with its canonical folder.
    /// Unknown and ungranted projects answer alike.
    async fn granted_project_root(
        &self,
        bot: &cypher_proto::Bot,
        space_id: &str,
    ) -> Result<(std::path::PathBuf, cypher_proto::Space), RpcError> {
        let space = self
            .workspace
            .space(space_id)
            .map_err(failed)?
            .filter(|space| bot.scope.sees_space(&space.id))
            .ok_or_else(|| RpcError::Failed("project is not available to this workspace".into()))?;
        let Some(space) =
            crate::bots::granted_local_spaces(bot, vec![space], self.doc_host.device_id()).pop()
        else {
            return Err(RpcError::Failed(
                "project is on another device; open it from a session there".into(),
            ));
        };
        let root = self
            .repos
            .canonical_dir(std::path::Path::new(&space.path))
            .await
            .ok_or_else(|| RpcError::Failed("project folder is missing".into()))?;
        Ok((root, space))
    }

    /// `ListChatFileRoots`: every folder the chat's file RPCs accept, the
    /// default first. A chat with no browsable folder (a hidden workspace
    /// thread, a damaged row) lists none.
    pub(super) async fn chat_file_roots(
        &self,
        chat_id: &str,
    ) -> Result<Vec<cypher_proto::ChatFileRoot>, RpcError> {
        let chat = self.local_chat(chat_id)?;
        let bot = self.owner_workspace(&chat)?;
        let mut roots = Vec::new();
        if let Ok(Ok(path)) =
            tokio::time::timeout(ROOT_RESOLVE_TIMEOUT, self.default_file_root(&chat)).await
        {
            let name = match &bot {
                Some(bot) => bot.name.clone(),
                None => chat
                    .space_id
                    .as_deref()
                    .and_then(|id| self.workspace.space(id).ok().flatten())
                    .map(|space| space.display_name().to_string())
                    .unwrap_or_default(),
            };
            roots.push(cypher_proto::ChatFileRoot {
                space_id: None,
                name,
                path: path.to_string_lossy().into_owned(),
                checkout_id: chat.checkout_id.clone(),
            });
        }
        let Some(bot) = bot else {
            return Ok(roots);
        };
        let spaces = self.workspace.read_spaces().map_err(failed)?;
        let mut projects = Vec::new();
        for space in crate::bots::granted_local_spaces(&bot, spaces, self.doc_host.device_id()) {
            let resolved = tokio::time::timeout(
                ROOT_RESOLVE_TIMEOUT,
                self.repos.canonical_dir(std::path::Path::new(&space.path)),
            )
            .await;
            let Ok(Some(path)) = resolved else { continue };
            projects.push(cypher_proto::ChatFileRoot {
                space_id: Some(space.id.clone()),
                name: space.display_name().to_string(),
                path: path.to_string_lossy().into_owned(),
                checkout_id: space.checkout_id.clone(),
            });
        }
        projects.sort_by_cached_key(|root| (root.name.to_lowercase(), root.path.clone()));
        roots.extend(projects);
        Ok(roots)
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
    use crate::git::workspace_files::Op;
    let p: WorkspaceFileParams = parse_params(params)?;
    let needs = |value: Option<String>, what: &str| {
        value.ok_or_else(|| RpcError::BadParams(format!("{method} needs {what}")))
    };
    let op = match method {
        methods::LIST_WORKSPACE_FILES => Op::List,
        methods::READ_WORKSPACE_FILE => Op::ReadText,
        methods::WRITE_WORKSPACE_FILE => Op::ReplaceText(needs(p.text.clone(), "text")?),
        methods::READ_WORKSPACE_FILE_CHUNK => Op::ReadChunk { offset: p.offset },
        methods::CREATE_WORKSPACE_ENTRY => Op::Create {
            directory: p.directory,
        },
        methods::WRITE_WORKSPACE_FILE_CHUNK => Op::WriteChunk {
            offset: p.offset,
            data: needs(p.data.clone(), "data")?,
        },
        methods::MOVE_WORKSPACE_ENTRY => Op::Move {
            to: needs(p.to.clone(), "to")?,
        },
        methods::COPY_WORKSPACE_ENTRY => Op::Copy {
            to: needs(p.to.clone(), "to")?,
        },
        methods::DELETE_WORKSPACE_ENTRY => Op::Delete {
            permanent: p.permanent,
        },
        other => return Err(RpcError::UnknownMethod(other.to_string())),
    };
    // Copies and deletes walk whole trees; everything else is one entry.
    let limit = if matches!(op, Op::Copy { .. } | Op::Delete { .. }) {
        std::time::Duration::from_secs(120)
    } else {
        std::time::Duration::from_secs(8)
    };
    tokio::time::timeout(limit, async {
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
            .chat_file_root(&p.chat_id, p.root_space_id.as_deref())
            .await?;
        same_checkout()?;
        let value = crate::git::workspace_files::run(root, p.path.clone(), op)
            .await
            .map_err(failed)?;
        same_checkout()?;
        RpcReply::value(&value)
    })
    .await
    .map_err(|_| RpcError::Failed("workspace file access timed out".into()))?
}

pub(super) async fn list_chat_file_roots(
    rpc: &EngineRpc,
    params: Value,
) -> Result<RpcReply, RpcError> {
    let p: ChatParams = parse_params(params)?;
    RpcReply::value(&rpc.chat_file_roots(&p.chat_id).await?)
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

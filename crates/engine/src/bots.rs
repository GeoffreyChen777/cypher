//! Agent bots hosted on this engine (docs/design/agent-bots.md §6–7).
//!
//! A bot's threads run through the normal sessions engine; what makes one a
//! bot thread is its run profile ([`profile::bot_env`]: the tools its origin
//! allows plus the system prompt for that origin) and the `BotTool` RPC its
//! tools call ([`Bots::tool`]). Every tool
//! call re-reads the bot row and resolves its scope against the current
//! registry, so a scope edit applies to the next call, and anything outside
//! the scope answers exactly like something that does not exist.
//!
//! Each bot has a private, empty working folder under the profile store
//! (`<store>/bots/<bot id>`): Pi needs a cwd, and an empty one loads no
//! project context files. Beside it, `<store>/bots-index.sqlite` is the
//! search index over what this device's bots may read ([`index`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;

use cypher_proto::{
    Bot, BotChat, BotOrigin, BotScope, Chat, ChatConfig, HarnessId, ReasoningLevel, SandboxLevel,
};

use crate::EngineError;
use crate::git::repos::Repos;
use crate::host::doc_host::DocHost;
use crate::host::workspace_host::WorkspaceHost;
use crate::util::{lock, new_id};

mod git;
mod index;
mod memory;
mod network;
pub mod profile;
mod prompt;
mod taint;
#[cfg(test)]
mod tests;
mod tools;
mod work;

/// `CreateBot` parameters (the target device is the bot's host).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBot {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub scope: BotScope,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<ReasoningLevel>,
    /// Absent from older clients: a new bot's defaults.
    #[serde(default)]
    pub capabilities: Option<cypher_proto::BotCapabilities>,
}

/// This engine's bot service: creation on the host and the tool surface.
#[derive(Clone)]
pub struct Bots {
    inner: Arc<Inner>,
}

struct Inner {
    workspace: WorkspaceHost,
    doc_host: DocHost,
    /// Git for spaces on this device (`project_git`).
    repos: Repos,
    /// Peer links for another device's git state; `None` offline.
    links: std::sync::Mutex<Option<Arc<cypher_rpc::LinkCache>>>,
    /// `<store>/bots`: one private folder per bot hosted here.
    root: PathBuf,
    /// `<store>/bots-index.sqlite`, kept current by the indexer task.
    index: Arc<index::SearchIndex>,
    indexer: std::sync::Mutex<Option<Indexer>>,
    /// Threads that received another user's bot's answer (§4.3).
    #[allow(dead_code)]
    taint: taint::Taint,
    /// Cancels the host loops (work, schedule, network) at shutdown.
    hosts_cancel: tokio_util::sync::CancellationToken,
    hosts: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

struct Indexer {
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Bots {
    pub fn new(workspace: WorkspaceHost, doc_host: DocHost, repos: Repos, root: PathBuf) -> Self {
        let index = index::SearchIndex::new(root.with_file_name("bots-index.sqlite"));
        Self {
            inner: Arc::new(Inner {
                workspace,
                doc_host,
                repos,
                links: Default::default(),
                root,
                index: Arc::new(index),
                indexer: Default::default(),
                taint: Default::default(),
                hosts_cancel: Default::default(),
                hosts: Default::default(),
            }),
        }
    }

    /// Start keeping the search index current (needs a tokio runtime).
    /// Idempotent.
    pub fn start_indexer(&self) {
        let mut slot = lock(&self.inner.indexer);
        if slot.is_some() {
            return;
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(index::run(
            self.inner.index.clone(),
            self.inner.workspace.clone(),
            self.inner.doc_host.clone(),
            cancel.clone(),
        ));
        *slot = Some(Indexer { cancel, task });
    }

    /// Start the host loops that act on bot records: orchestrations, the
    /// scheduler and the network (needs a tokio runtime). Each track's
    /// `start_*` keeps its task with [`Bots::keep_host_task`].
    pub fn start_hosts(&self) {
        self.start_work_host();
        self.start_schedule_host();
        self.start_network_host();
    }

    /// Keep a host loop's task so shutdown can wait for it.
    #[allow(dead_code)]
    fn keep_host_task(&self, task: tokio::task::JoinHandle<()>) {
        lock(&self.inner.hosts).push(task);
    }

    /// Cancelled at shutdown: host loops end when it fires.
    #[allow(dead_code)]
    fn hosts_cancelled(&self) -> tokio_util::sync::CancellationToken {
        self.inner.hosts_cancel.clone()
    }

    /// Stop the indexer and wait for it (a pass in flight finishes first).
    pub async fn shutdown(&self) {
        self.inner.hosts_cancel.cancel();
        let hosts: Vec<_> = lock(&self.inner.hosts).drain(..).collect();
        for task in hosts {
            let _ = task.await;
        }
        let indexer = lock(&self.inner.indexer).take();
        if let Some(indexer) = indexer {
            indexer.cancel.cancel();
            let _ = indexer.task.await;
        }
    }

    /// Bring the search index up to date now (tests and callers that just
    /// wrote rows). Returns the chats re-read.
    pub async fn index_now(&self) -> Result<usize, EngineError> {
        let mut total = 0;
        loop {
            let pass = index::pass(
                &self.inner.index,
                &self.inner.workspace,
                &self.inner.doc_host,
            )
            .await?;
            total += pass.indexed;
            if pass.remaining == 0 {
                return Ok(total);
            }
        }
    }

    /// Attach the peer link cache: `project_git` reads other devices' spaces
    /// through it.
    pub fn set_links(&self, links: Arc<cypher_rpc::LinkCache>) {
        *lock(&self.inner.links) = Some(links);
    }

    fn device_id(&self) -> &str {
        self.inner.workspace.device_id()
    }

    /// Create a bot hosted on THIS device: its folder, its chat and its row.
    pub fn create(&self, request: CreateBot) -> Result<Bot, EngineError> {
        let now = Utc::now();
        let bot = Bot {
            id: new_id(),
            name: request.name.trim().to_string(),
            icon: request.icon.filter(|s| !s.is_empty()),
            color: request.color.filter(|s| !s.is_empty()),
            host_device_id: self.device_id().to_string(),
            chat_id: new_id(),
            instructions: request
                .instructions
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            scope: request.scope,
            capabilities: request
                .capabilities
                .unwrap_or_else(cypher_proto::BotCapabilities::new_bot),
            paused: false,
            created_at: now,
        };
        bot.validate().map_err(EngineError::Other)?;
        let folder = self.inner.root.join(&bot.id);
        create_private_dir(&folder)?;
        let chat = Chat {
            id: bot.chat_id.clone(),
            device_id: bot.host_device_id.clone(),
            title: Some(bot.name.clone()),
            archived: false,
            pinned: false,
            cwd: Some(folder.to_string_lossy().into_owned()),
            branch: None,
            checkout_id: None,
            config: Some(ChatConfig {
                harness: HarnessId::Pi,
                model: request.model.filter(|m| !m.is_empty()),
                reasoning: request.reasoning,
                model_options: Default::default(),
                sandbox: SandboxLevel::ReadOnly,
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: now,
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            // Seen on birth: a new bot must not open with an unread badge.
            last_seen_at: Some(now),
            room_gen: Some(2),
            child: None,
            bot: Some(BotChat::owner(&bot.id)),
        };
        self.inner.workspace.create_bot(&bot, &chat)?;
        tracing::info!(bot = %bot.id, chat = %bot.chat_id, "bot created");
        Ok(bot)
    }

    /// Remove the folder of a bot hosted here (after its rows are gone).
    pub fn remove_folder(&self, bot_id: &str) {
        if bot_id.is_empty() || bot_id.contains(['/', '\\']) || bot_id.starts_with('.') {
            return;
        }
        let folder = self.inner.root.join(bot_id);
        if let Err(err) = std::fs::remove_dir_all(&folder)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(bot = %bot_id, error = %err, "could not remove the bot folder");
        }
    }
}

fn create_private_dir(path: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Runtime slash commands an owner thread refuses: each would change the
/// run's tools or modes behind the bot tool allowlist.
pub const REFUSED_COMMANDS: [&str; 3] = ["scripts", "orchestrate", "goal"];

/// Refuse a bot thread's prompt that is a slash command it may not run:
/// one of [`REFUSED_COMMANDS`] in an owner thread, any command at all in a
/// routine or peer thread (§4.4). Ordinary chats run every command.
pub fn refuse_widening_command(chat: &Chat, prompt: &str) -> Result<(), EngineError> {
    let Some(marker) = &chat.bot else {
        return Ok(());
    };
    let Some(rest) = prompt.trim_start().strip_prefix('/') else {
        return Ok(());
    };
    let command = rest.split_whitespace().next().unwrap_or_default();
    match marker.origin {
        BotOrigin::Owner if !REFUSED_COMMANDS.contains(&command) => Ok(()),
        BotOrigin::Owner => Err(EngineError::Other(format!(
            "/{command} isn't available in a bot chat."
        ))),
        BotOrigin::Routine | BotOrigin::Peer => Err(EngineError::Other(
            "Slash commands aren't available in this bot thread.".into(),
        )),
    }
}

/// Recreate a bot chat's folder before a run spawns in it (a restored or
/// moved data directory may have lost it). Only a path that IS this bot's
/// folder (`…/bots/<bot id>`) is created.
pub fn ensure_folder_for_run(chat: &Chat, cwd: &str) {
    let Some(marker) = &chat.bot else {
        return;
    };
    let path = Path::new(cwd);
    let is_bot_folder = path
        .file_name()
        .is_some_and(|name| name == marker.bot_id.as_str())
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "bots");
    if is_bot_folder
        && !path.is_dir()
        && let Err(err) = create_private_dir(path)
    {
        tracing::warn!(bot = %marker.bot_id, error = %err, "could not recreate the bot folder");
    }
}

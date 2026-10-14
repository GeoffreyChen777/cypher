//! Agent bots hosted on this engine (docs/design/agent-bots.md §6–7).
//!
//! A bot's chat runs through the normal sessions engine; what makes it a bot
//! is the run env ([`run_env`]: the bot tools only, plus the bot's system
//! prompt) and the `BotTool` RPC its tools call ([`Bots::tool`]). Every tool
//! call re-reads the bot row and resolves its scope against the current
//! registry, so a scope edit applies to the next call, and anything outside
//! the scope answers exactly like something that does not exist.
//!
//! Each bot has a private, empty working folder under the profile store
//! (`<store>/bots/<bot id>`): Pi needs a cwd, and an empty one loads no
//! project context files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;

use cypher_harness::BotRunEnv;
use cypher_proto::{
    Bot, BotChat, BotScope, Chat, ChatConfig, HarnessId, ReasoningLevel, SandboxLevel,
};

use crate::EngineError;
use crate::host::doc_host::DocHost;
use crate::host::workspace_host::WorkspaceHost;
use crate::util::new_id;

mod prompt;
#[cfg(test)]
mod tests;
mod tools;

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
}

/// This engine's bot service: creation on the host and the tool surface.
#[derive(Clone)]
pub struct Bots {
    inner: Arc<Inner>,
}

struct Inner {
    workspace: WorkspaceHost,
    doc_host: DocHost,
    /// `<store>/bots`: one private folder per bot hosted here.
    root: PathBuf,
}

impl Bots {
    pub fn new(workspace: WorkspaceHost, doc_host: DocHost, root: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                workspace,
                doc_host,
                root,
            }),
        }
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
            bot: Some(BotChat {
                bot_id: bot.id.clone(),
            }),
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

/// The locked-down run env for a bot's chat; `None` for every other chat.
/// A bot chat whose bot row is gone still runs locked down, seeing nothing.
pub fn run_env(workspace: &WorkspaceHost, chat: &Chat) -> Option<BotRunEnv> {
    let marker = chat.bot.as_ref()?;
    let bot = workspace.bot(&marker.bot_id).ok().flatten();
    let spaces = workspace.read_spaces().unwrap_or_default();
    let devices = workspace.read_devices().unwrap_or_default();
    let system_prompt = match &bot {
        Some(bot) => prompt::system_prompt(bot, &spaces, &devices),
        None => prompt::orphan_prompt(),
    };
    Some(BotRunEnv {
        bot_id: marker.bot_id.clone(),
        system_prompt,
    })
}

/// Runtime slash commands a bot chat refuses: each would change the run's
/// tools or modes behind the bot tool allowlist.
pub const REFUSED_COMMANDS: [&str; 3] = ["scripts", "orchestrate", "goal"];

/// Refuse a bot chat's prompt that is one of [`REFUSED_COMMANDS`].
pub fn refuse_widening_command(chat: &Chat, prompt: &str) -> Result<(), EngineError> {
    if chat.bot.is_none() {
        return Ok(());
    }
    let command = prompt
        .trim_start()
        .strip_prefix('/')
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default();
    if REFUSED_COMMANDS.contains(&command) {
        return Err(EngineError::Other(format!(
            "/{command} isn't available in a bot chat."
        )));
    }
    Ok(())
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

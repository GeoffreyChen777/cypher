//! The bot network (docs/design/agent-bots.md §11, P4): publishing,
//! invites, connections, peer threads, `ask_bot` and Review mode.

use cypher_proto::{Bot, BotChat, Chat};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::WorkspaceHost;

impl Bots {
    /// Start this track's host loop (called once at engine start).
    pub(super) fn start_network_host(&self) {}

    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) async fn network_tool(
        &self,
        _chat: &Chat,
        _marker: &BotChat,
        _tool: &str,
        _args: &serde_json::Value,
    ) -> Option<Result<String, EngineError>> {
        None
    }
}

/// This track's tools a thread may call, beyond the read tools.
pub(super) fn network_tools(_marker: &BotChat, _bot: Option<&Bot>) -> Vec<&'static str> {
    Vec::new()
}

/// This track's section of the thread's system prompt, if any.
pub(super) fn network_prompt(
    _workspace: &WorkspaceHost,
    _bot: &Bot,
    _marker: &BotChat,
) -> Option<String> {
    None
}

//! Memory and routines (docs/design/agent-bots.md §7–§8, P3): the
//! notebook, the scheduler, routines and recurring project tasks.

use cypher_proto::{Bot, BotChat, Chat};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::WorkspaceHost;

impl Bots {
    /// Start this track's host loop (called once at engine start).
    pub(super) fn start_schedule_host(&self) {}

    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) async fn memory_tool(
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
pub(super) fn memory_tools(_marker: &BotChat, _bot: Option<&Bot>) -> Vec<&'static str> {
    Vec::new()
}

/// This track's section of the thread's system prompt, if any.
pub(super) fn memory_prompt(
    _workspace: &WorkspaceHost,
    _bot: &Bot,
    _marker: &BotChat,
) -> Option<String> {
    None
}

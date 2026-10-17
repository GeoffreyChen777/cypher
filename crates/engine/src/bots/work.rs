//! Coordination (docs/design/agent-bots.md §6, P2): *Work directly*,
//! plans, orchestration threads, worker sessions, Stop, limits, approvals
//! and the activity log.

use cypher_proto::{Bot, BotChat, Chat};

use super::Bots;
use crate::EngineError;
use crate::host::workspace_host::WorkspaceHost;

impl Bots {
    /// Start this track's host loop (called once at engine start).
    pub(super) fn start_work_host(&self) {}

    /// Answer `tool` when it is one of this track's; `None` otherwise.
    pub(super) async fn work_tool(
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
pub(super) fn work_tools(_marker: &BotChat, _bot: Option<&Bot>) -> Vec<&'static str> {
    Vec::new()
}

/// This track's section of the thread's system prompt, if any.
pub(super) fn work_prompt(
    _workspace: &WorkspaceHost,
    _bot: &Bot,
    _marker: &BotChat,
) -> Option<String> {
    None
}

/// The workspace roots an owner thread with *Work directly* gets: the
/// bot's scoped checkouts on this device (§4.4). Empty for now.
pub(super) fn workspace_roots(
    _workspace: &WorkspaceHost,
    _bot: &Bot,
    _marker: &BotChat,
) -> Vec<String> {
    Vec::new()
}

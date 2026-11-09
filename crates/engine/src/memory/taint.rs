//! Runs that read untrusted content (docs/design/memory.md §7): a web
//! page, a web search, an MCP tool's answer, or a command that fetches a
//! URL. Their memory writes are held for the owner's review, because
//! memory reaches every later run and injected text would persist there.
//!
//! The run task reports every event here ([`Taint::observe`]); a new run
//! clears its chat ([`Taint::clear`], when its memory prompt is built). In
//! memory only: an engine restart ends every run. It also counts the token
//! use consolidation passes record.

use std::collections::HashMap;
use std::sync::Mutex;

use cypher_proto::{AgentEvent, ToolCall};

use crate::util::lock;

#[derive(Default)]
pub(super) struct Taint {
    /// Chat id → why its current run is untrusted.
    tainted: Mutex<HashMap<String, String>>,
    /// Chat id → tokens used so far, for the chats [`Taint::track_usage`]
    /// named.
    usage: Mutex<HashMap<String, u64>>,
}
/// Why `call` makes its run untrusted, when it does.
fn untrusted(call: &ToolCall) -> Option<String> {
    match call {
        ToolCall::WebFetch { url, .. } => Some(format!("fetched a web page ({url})")),
        ToolCall::WebSearch { .. } => Some("searched the web".into()),
        ToolCall::Mcp { server, tool, .. } => Some(format!("used an MCP tool ({server}/{tool})")),
        ToolCall::Exec { command } if fetches(command) => {
            Some("ran a command that fetches from the network".into())
        }
        _ => None,
    }
}

/// Whether a shell command reads from the network.
fn fetches(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.contains("://")
        || lower
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .any(|word| matches!(word, "curl" | "wget" | "http" | "https" | "lynx" | "w3m"))
}

impl Taint {
    /// Called for every event of a run in `chat_id`.
    pub(super) fn observe(&self, chat_id: &str, event: &AgentEvent) {
        match event {
            AgentEvent::ToolCall { call, .. } => {
                if let Some(reason) = untrusted(call) {
                    lock(&self.tainted)
                        .entry(chat_id.to_string())
                        .or_insert(reason);
                }
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                if let Some(total) = lock(&self.usage).get_mut(chat_id) {
                    // Session totals, not deltas: keep the largest seen.
                    *total = (*total).max(input_tokens + output_tokens);
                }
            }
            _ => {}
        }
    }

    pub(super) fn reason(&self, chat_id: &str) -> Option<String> {
        lock(&self.tainted).get(chat_id).cloned()
    }

    pub(super) fn clear(&self, chat_id: &str) {
        lock(&self.tainted).remove(chat_id);
    }

    /// Start counting `chat_id`'s token use.
    pub(super) fn track_usage(&self, chat_id: &str) {
        lock(&self.usage).entry(chat_id.to_string()).or_insert(0);
    }

    /// What `chat_id` used, and stop counting it.
    pub(super) fn take_usage(&self, chat_id: &str) -> u64 {
        lock(&self.usage).remove(chat_id).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(call: ToolCall) -> AgentEvent {
        AgentEvent::ToolCall {
            id: "t".into(),
            call,
        }
    }

    #[test]
    fn web_mcp_and_fetching_commands_taint_a_run() {
        let taint = Taint::default();
        let chat = "chat";
        taint.observe(chat, &call(ToolCall::ReadFile { path: "a".into() }));
        taint.observe(
            chat,
            &call(ToolCall::Exec {
                command: "cargo test".into(),
            }),
        );
        assert!(taint.reason(chat).is_none());
        taint.observe(
            chat,
            &call(ToolCall::Exec {
                command: "curl -s https://example.com".into(),
            }),
        );
        assert!(taint.reason(chat).unwrap().contains("fetches"));
        taint.clear(chat);
        taint.observe(
            chat,
            &call(ToolCall::WebFetch {
                url: "https://example.com".into(),
                prompt: None,
            }),
        );
        assert!(taint.reason(chat).unwrap().contains("example.com"));
        taint.clear(chat);
        assert!(taint.reason(chat).is_none());
    }

    #[test]
    fn usage_counts_only_tracked_chats() {
        let usage = |n: u64| AgentEvent::Usage {
            input_tokens: n,
            output_tokens: 1,
        };
        let taint = Taint::default();
        taint.observe("untracked-chat", &usage(10));
        assert_eq!(taint.take_usage("untracked-chat"), 0);
        taint.track_usage("pass-chat");
        taint.observe("pass-chat", &usage(10));
        taint.observe("pass-chat", &usage(40));
        assert_eq!(taint.take_usage("pass-chat"), 41);
    }
}

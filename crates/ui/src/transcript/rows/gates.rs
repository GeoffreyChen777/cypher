//! Fork and rewind gates: whether a message can be forked from or rewound
//! to, and the tooltip that says why not.

use cypher_doc::MessageRole;
use cypher_proto::{Chat, HarnessId};

/// Session Fork affordance state for the timestamp strip's git-branch icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkGate {
    /// Shown and clickable: the source is a settled Pi root chat.
    Enabled,
    /// Shown but inert (dimmed), with the reason for the tooltip.
    Disabled(&'static str),
}

/// The fork affordance gate (pure — tests exercise the real gating without a
/// gpui App). Embedded (Side Chat) panels, offline engines, child chats,
/// non-Pi configs, live (Working/AwaitingInput) chats, and an OFFLINE SOURCE
/// HOST DEVICE are all disabled. Remote Pi chats stay ENABLED while their
/// host is online: the shell relays `ForkSession` to the source chat's host
/// device.
pub fn fork_gate(
    embedded: bool,
    chat: Option<&Chat>,
    live: bool,
    offline: bool,
    host_online: bool,
) -> ForkGate {
    if embedded {
        return ForkGate::Disabled("Side chats can't be forked.");
    }
    if offline {
        return ForkGate::Disabled("The engine is offline.");
    }
    let Some(chat) = chat else {
        return ForkGate::Disabled("No chat selected.");
    };
    if chat.is_child() {
        return ForkGate::Disabled("Subagent chats can't be forked.");
    }
    if chat.config.as_ref().map(|c| c.harness) != Some(HarnessId::Pi) {
        return ForkGate::Disabled("Only Pi chats can be forked.");
    }
    if live {
        return ForkGate::Disabled("Wait for the chat to finish before forking.");
    }
    if !host_online {
        return ForkGate::Disabled("The device hosting this chat is offline.");
    }
    ForkGate::Enabled
}

/// The fork affordance's tooltip: role-specific while enabled, the disabled
/// reason otherwise. Enabled text is exactly `Fork before this message` for
/// User and `Fork after this response` for Assistant.
pub fn fork_tooltip(role: MessageRole, gate: &ForkGate) -> &'static str {
    match gate {
        ForkGate::Disabled(reason) => reason,
        ForkGate::Enabled => match role {
            MessageRole::User => "Fork before this message",
            MessageRole::Assistant => "Fork after this response",
            // System rows never emit a fork affordance; keep a fallback text
            // so a misroute is still coherent.
            MessageRole::System => "Fork after this message",
        },
    }
}

/// The rewind affordance gate (pure, like [`fork_gate`]). Restarting the
/// conversation from a message runs the SAME pi machinery as a fork — it just
/// lands in place — so the prerequisites match, worded for a restart. One
/// extra rule: the NEWEST entry has nothing after it, so restarting there
/// would delete nothing.
pub fn rewind_gate(
    embedded: bool,
    chat: Option<&Chat>,
    live: bool,
    offline: bool,
    host_online: bool,
    is_last_entry: bool,
) -> ForkGate {
    if embedded {
        return ForkGate::Disabled("Side chats can't be restarted from a message.");
    }
    if offline {
        return ForkGate::Disabled("The engine is offline.");
    }
    let Some(chat) = chat else {
        return ForkGate::Disabled("No chat selected.");
    };
    if chat.is_child() {
        return ForkGate::Disabled("Subagent chats can't be restarted from a message.");
    }
    if chat.config.as_ref().map(|c| c.harness) != Some(HarnessId::Pi) {
        return ForkGate::Disabled("Only Pi chats can be restarted from a message.");
    }
    if live {
        return ForkGate::Disabled("Wait for the chat to finish before restarting it.");
    }
    if !host_online {
        return ForkGate::Disabled("The device hosting this chat is offline.");
    }
    if is_last_entry {
        return ForkGate::Disabled("Nothing to remove after the last message.");
    }
    ForkGate::Enabled
}

/// The rewind affordance's tooltip. The ARMED text (after the first click)
/// spells out what the confirming click deletes — the removal is permanent,
/// so the count is never left implicit.
pub fn rewind_tooltip(role: MessageRole, gate: &ForkGate, armed: bool, later: usize) -> String {
    match gate {
        ForkGate::Disabled(reason) => (*reason).to_string(),
        ForkGate::Enabled if armed => match role {
            MessageRole::User => format!(
                "Click again to delete this message and {} after it",
                plural_messages(later)
            ),
            _ => format!(
                "Click again to delete {} after this response",
                plural_messages(later)
            ),
        },
        ForkGate::Enabled => match role {
            MessageRole::User => {
                "Restart from here — deletes this message and everything after it".to_string()
            }
            _ => "Restart from here — deletes everything after this response".to_string(),
        },
    }
}

fn plural_messages(count: usize) -> String {
    if count == 1 {
        "1 message".to_string()
    } else {
        format!("{count} messages")
    }
}

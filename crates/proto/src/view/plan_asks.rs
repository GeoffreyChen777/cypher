//! Which chats wait on the owner's answer (docs/design/plan-asks.md §5).
//!
//! One rule for every reader (desktop Needs you, logo count, Dock badge,
//! the work host's step states, the iPhone and the Worker): a chat asks
//! while its session row carries an open question ([`Session::asking`]),
//! whatever the row's status or age; a plan's question is one open in a
//! chat the plan started or steered, or in any child chat under one. A row
//! from a host that doesn't write `asking` falls back to the old rule
//! (`AwaitingInput` and fresh). The shared vectors are
//! `protocol/vectors/plan-asks-v1.json`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::{Indicator, effective_indicator};
use crate::{Chat, Session};

/// How deep under a chat its child chats are searched for a question: the
/// engine forwards a subagent's question at most this many levels up.
pub const MAX_CHILD_DEPTH: usize = 8;

/// Whether one session row waits on an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    None,
    Asking,
    /// Asking, but the chat's host device is offline: the question is
    /// open and is answered once the device is back.
    AskingOffline,
}

impl Ask {
    pub fn is_asking(self) -> bool {
        self != Ask::None
    }
}

/// Whether `session` waits on an answer. `host_online`: the row's
/// `deviceId` is reachable now (its presence). Pure.
pub fn session_ask(session: Option<&Session>, now: DateTime<Utc>, host_online: bool) -> Ask {
    let Some(session) = session else {
        return Ask::None;
    };
    if session.asking.is_some() {
        return if host_online {
            Ask::Asking
        } else {
            Ask::AskingOffline
        };
    }
    // A host that doesn't write `asking`: the status, while the row is fresh.
    if effective_indicator(Some(session), now) == Indicator::AwaitingInput {
        Ask::Asking
    } else {
        Ask::None
    }
}

/// A question found under a chat ([`ChatTree::question`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanQuestion {
    /// The chat that asks: the chat itself (its own question, or a
    /// subagent's mirrored into it), else the nearest child chat that does.
    pub chat_id: String,
    /// Its host device is offline ("Waiting for its device").
    pub offline: bool,
}

/// One chat's place in the tree, as [`ChatTree`] reads it: `chat.id`,
/// `chat.child.parentChatId` and `chat.archived`.
#[derive(Debug, Clone, Copy)]
pub struct ChatLink<'a> {
    pub id: &'a str,
    pub parent_chat_id: Option<&'a str>,
    /// An archived chat has ended: it asks nothing, and neither do its
    /// child chats on its behalf.
    pub archived: bool,
}

impl<'a> From<&'a Chat> for ChatLink<'a> {
    fn from(chat: &'a Chat) -> Self {
        Self {
            id: &chat.id,
            parent_chat_id: chat.parent_chat_id(),
            archived: chat.archived,
        }
    }
}

/// Chats by parent, to find a question anywhere under a chat.
#[derive(Debug, Default)]
pub struct ChatTree<'a> {
    children: HashMap<&'a str, Vec<&'a str>>,
    ended: HashSet<&'a str>,
}

impl<'a> ChatTree<'a> {
    pub fn new<L: Into<ChatLink<'a>>>(chats: impl IntoIterator<Item = L>) -> Self {
        let mut tree = Self::default();
        for link in chats {
            let link = link.into();
            if link.archived {
                tree.ended.insert(link.id);
            }
            if let Some(parent) = link.parent_chat_id {
                tree.children.entry(parent).or_default().push(link.id);
            }
        }
        for children in tree.children.values_mut() {
            children.sort_unstable();
        }
        tree
    }

    /// The child chats directly under `chat_id`, by id.
    pub fn children(&self, chat_id: &str) -> &[&'a str] {
        self.children.get(chat_id).map_or(&[], Vec::as_slice)
    }

    /// The question open in `chat_id` or in a child chat under it, by
    /// `ask(chat id)` ([`session_ask`] on the chat's row): the chat's own
    /// first, then its children level by level, `MAX_CHILD_DEPTH` deep. An
    /// archived chat asks nothing and hides its subtree.
    pub fn question(&self, chat_id: &str, ask: impl Fn(&str) -> Ask) -> Option<PlanQuestion> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut level: Vec<&str> = vec![chat_id];
        for _ in 0..=MAX_CHILD_DEPTH {
            let mut next = Vec::new();
            for id in level {
                if self.ended.contains(id) || !seen.insert(id) {
                    continue;
                }
                match ask(id) {
                    Ask::None => next.extend(self.children(id).iter().copied()),
                    found => {
                        return Some(PlanQuestion {
                            chat_id: id.to_string(),
                            offline: found == Ask::AskingOffline,
                        });
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        None
    }
}

/// [`ChatTree::question`] over rows by chat id and presence by device id.
/// The convenience the clients and the vectors share.
pub fn plan_question<'s>(
    tree: &ChatTree<'_>,
    chat_id: &str,
    sessions: impl Fn(&str) -> Option<&'s Session>,
    device_online: impl Fn(&str) -> bool,
    now: DateTime<Utc>,
) -> Option<PlanQuestion> {
    tree.question(chat_id, |id| {
        let row = sessions(id);
        session_ask(row, now, row.is_none_or(|r| device_online(&r.device_id)))
    })
}

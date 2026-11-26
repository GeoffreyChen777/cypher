//! The one rule for whether a question in a chat's doc is still open, and
//! who can answer it.
//!
//! A question is an unresolved [`MessagePart::Input`]. It is open until its
//! part resolves — never because of the status of the entry that holds it.
//! What the entry and the chat's run decide is only WHO answers it:
//!
//! - a live run's own question sits on the entry that run is streaming: the
//!   run's resolver answers it ([`Holder::LiveRun`]);
//! - a subagent's question mirrored into its parent's chat is answered by
//!   the subagent while its forward is live ([`Holder::LiveRun`] too);
//! - anything else has no live run behind it — a settled entry, a mirror
//!   whose forward is gone, or a `Streaming` entry no run is writing any
//!   more (the run died without settling it: a crash, a quit cut short).
//!   Such a question is dead but still answerable: `RespondInput` delivers
//!   the answer as the next turn ([`Holder::Nobody`]).
//!
//! Boot recovery, the session row's `asking` (and so Needs you), Stop with no
//! run and the `RespondInput` fallback all read questions through
//! [`open_questions`].

use std::collections::HashSet;

use cypher_doc::{MessagePart, MessageStatus, SessionMessageEntry};
use cypher_proto::UserInputQuestion;

/// Who answers an open question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holder {
    /// A live run (the chat's own, or a subagent's live forward) holds the
    /// question's resolver.
    LiveRun,
    /// No live run is behind the question: it is answered by the next turn,
    /// or closed.
    Nobody,
}

/// What is live in the chat right now, as far as its questions go.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Liveness<'a> {
    /// The chat has a run (working, asking or parked).
    pub run_live: bool,
    /// Request ids of subagent questions mirrored here whose forward is live.
    pub live_mirrors: &'a HashSet<String>,
}

/// One open question in a chat's doc.
#[derive(Debug, Clone)]
pub(crate) struct OpenQuestion {
    pub request_id: String,
    pub questions: Vec<UserInputQuestion>,
    /// When the entry holding it was created (the question's `asked_at`).
    pub asked_at: i64,
    /// A subagent's question mirrored into this chat (`asked_by` set).
    pub mirror: bool,
    pub holder: Holder,
}

/// Every open question in `entries`, oldest first, with who answers it.
pub(crate) fn open_questions(
    entries: &[SessionMessageEntry],
    live: Liveness<'_>,
) -> Vec<OpenQuestion> {
    let mut open = Vec::new();
    for entry in entries {
        for part in &entry.parts {
            let MessagePart::Input {
                request_id,
                questions,
                resolved: false,
                ..
            } = part
            else {
                continue;
            };
            let mirror = questions.iter().any(|q| q.asked_by.is_some());
            let held = if mirror {
                live.live_mirrors.contains(request_id)
            } else {
                live.run_live && entry.status == Some(MessageStatus::Streaming)
            };
            open.push(OpenQuestion {
                request_id: request_id.clone(),
                questions: questions.clone(),
                asked_at: entry.created_at,
                mirror,
                holder: if held {
                    Holder::LiveRun
                } else {
                    Holder::Nobody
                },
            });
        }
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use cypher_doc::MessageRole;

    fn question(asked_by: Option<&str>) -> UserInputQuestion {
        UserInputQuestion {
            id: "q1".into(),
            header: "Run this command?".into(),
            question: "Run this command?".into(),
            options: vec!["Allow".into(), "Deny".into()],
            multi_select: false,
            permission: None,
            asked_by: asked_by.map(str::to_owned),
        }
    }

    fn entry(
        id: &str,
        status: MessageStatus,
        request_id: &str,
        resolved: bool,
        asked_by: Option<&str>,
    ) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: format!("in-{request_id}"),
                request_id: request_id.into(),
                questions: vec![question(asked_by)],
                resolved,
            }],
            created_at: 7,
            device_id: "dev".into(),
            status: Some(status),
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
            models: Vec::new(),
            turn_changes: None,
            recovery: None,
        }
    }

    fn holders(entries: &[SessionMessageEntry], run_live: bool, mirrors: &[&str]) -> Vec<Holder> {
        let live_mirrors: HashSet<String> = mirrors.iter().map(|m| m.to_string()).collect();
        open_questions(
            entries,
            Liveness {
                run_live,
                live_mirrors: &live_mirrors,
            },
        )
        .into_iter()
        .map(|q| q.holder)
        .collect()
    }

    #[test]
    fn a_streaming_entry_with_no_run_behind_it_is_dead_and_answerable() {
        let entries = [entry("a", MessageStatus::Streaming, "r1", false, None)];
        assert_eq!(holders(&entries, false, &[]), [Holder::Nobody]);
        assert_eq!(holders(&entries, true, &[]), [Holder::LiveRun]);
    }

    #[test]
    fn a_settled_entry_never_has_a_live_run_behind_it() {
        for status in [MessageStatus::Complete, MessageStatus::Aborted] {
            let entries = [entry("a", status, "r1", false, None)];
            assert_eq!(holders(&entries, true, &[]), [Holder::Nobody]);
            assert_eq!(holders(&entries, false, &[]), [Holder::Nobody]);
        }
    }

    #[test]
    fn a_resolved_part_is_never_open() {
        let entries = [
            entry("a", MessageStatus::Streaming, "r1", true, None),
            entry("b", MessageStatus::Aborted, "r2", true, None),
        ];
        assert!(holders(&entries, false, &[]).is_empty());
    }

    #[test]
    fn a_mirror_is_held_by_its_live_forward_only() {
        let entries = [entry(
            "m",
            MessageStatus::Complete,
            "m1",
            false,
            Some("Explorer"),
        )];
        assert_eq!(holders(&entries, false, &["m1"]), [Holder::LiveRun]);
        assert_eq!(holders(&entries, true, &[]), [Holder::Nobody]);
    }
}

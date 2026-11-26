//! What a surface shows for a question it answered or a Stop it sent: the
//! host's outcome of that very command, read from the doc's ledger, never a
//! local guess. One rule for the composer, the Needs you cards and the
//! Asking page, pinned by `protocol/vectors/command-outcomes-v1.json`, which
//! the iPhone's twin (`apps/ios/Cypher/Sync/CommandOutcome.swift`) runs too.
//!
//! The host writes `Pending → Applied | Rejected | Expired` with a free-text
//! `resolution` (docs/design/plan-asks.md §7). An answer is done once it is
//! applied and its question resolved; a Stop once it is applied. A refused
//! one says so in the host's own words.

use cypher_doc::{SessionCommandEntry, SessionCommandStatus};

/// The host's outcome of one command, as far as this client has seen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome<'a> {
    /// `None`: the command isn't in the ledger this client reads yet.
    pub status: Option<SessionCommandStatus>,
    pub resolution: Option<&'a str>,
}

impl<'a> Outcome<'a> {
    /// The ledger entry `command_id` names in `commands`, if this client has
    /// seen it (`command_id` `None`: the queue call hasn't answered yet).
    pub fn of(commands: &'a [SessionCommandEntry], command_id: Option<&str>) -> Self {
        let entry = command_id.and_then(|id| commands.iter().find(|c| c.id == id));
        Self {
            status: entry.map(|c| c.status),
            resolution: entry.and_then(|c| c.resolution.as_deref()),
        }
    }
}

/// A question as the surface that may have answered it shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// Nothing sent from here: the question with its answers.
    Open,
    /// On its way, or applied while the resolution syncs back.
    Sending,
    /// Answered (here or elsewhere): the item leaves.
    Done,
    /// The host turned the answer down; the line says why.
    NoLongerWaiting(String),
}

impl AnswerOutcome {
    /// The line in place of the answers, if any.
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Sending => Some("Sending…".into()),
            Self::NoLongerWaiting(text) => Some(text.clone()),
            Self::Open | Self::Done => None,
        }
    }
}

/// A question's display from the answer this client sent (`None`: none)
/// and whether its part is resolved. Pure.
pub fn answer_outcome(sent: Option<Outcome<'_>>, resolved: bool) -> AnswerOutcome {
    use SessionCommandStatus::*;
    let Some(sent) = sent else {
        return if resolved {
            AnswerOutcome::Done
        } else {
            AnswerOutcome::Open
        };
    };
    match sent.status {
        None if resolved => AnswerOutcome::Done,
        None | Some(Pending) => AnswerOutcome::Sending,
        Some(Applied) if resolved => AnswerOutcome::Done,
        Some(Applied) => AnswerOutcome::Sending,
        Some(status @ (Rejected | Expired | Superseded)) => {
            AnswerOutcome::NoLongerWaiting(format!(
                "No longer waiting: {}",
                reason(
                    sent.resolution,
                    status,
                    "the answer expired before the session took it",
                    "the session didn't take the answer",
                )
            ))
        }
    }
}

/// A Stop as the surface that sent it shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    Stopping,
    /// Applied, or superseded (the turn it was aimed at already ended): the
    /// run's own state says the rest.
    Done,
    /// The host turned it down; the line says why.
    Refused(String),
}

impl StopOutcome {
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Stopping => Some("Stopping…".into()),
            Self::Refused(text) => Some(text.clone()),
            Self::Done => None,
        }
    }
}

/// A Stop's display from its outcome. Pure.
pub fn stop_outcome(sent: Outcome<'_>) -> StopOutcome {
    use SessionCommandStatus::*;
    match sent.status {
        None | Some(Pending) => StopOutcome::Stopping,
        Some(Applied | Superseded) => StopOutcome::Done,
        Some(status @ (Rejected | Expired)) => StopOutcome::Refused(format!(
            "Didn't stop: {}",
            reason(
                sent.resolution,
                status,
                "the Stop expired before the session took it",
                "the session didn't take the Stop",
            )
        )),
    }
}

/// The host's words, or the generic phrase for a status without any.
fn reason<'a>(
    resolution: Option<&'a str>,
    status: SessionCommandStatus,
    expired: &'a str,
    refused: &'a str,
) -> &'a str {
    match resolution.map(str::trim).filter(|r| !r.is_empty()) {
        Some(words) => words,
        None if status == SessionCommandStatus::Expired => expired,
        None => refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn vectors() -> Value {
        serde_json::from_str(include_str!(
            "../../../protocol/vectors/command-outcomes-v1.json"
        ))
        .unwrap()
    }

    fn status(case: &Value) -> Option<SessionCommandStatus> {
        (!case["status"].is_null()).then(|| serde_json::from_value(case["status"].clone()).unwrap())
    }

    fn answer_state(outcome: &AnswerOutcome) -> &'static str {
        match outcome {
            AnswerOutcome::Open => "open",
            AnswerOutcome::Sending => "sending",
            AnswerOutcome::Done => "done",
            AnswerOutcome::NoLongerWaiting(_) => "noLongerWaiting",
        }
    }

    fn stop_state(outcome: &StopOutcome) -> &'static str {
        match outcome {
            StopOutcome::Stopping => "stopping",
            StopOutcome::Done => "done",
            StopOutcome::Refused(_) => "refused",
        }
    }

    #[test]
    fn answer_vectors() {
        let vectors = vectors();
        let cases = vectors["answer"].as_array().unwrap();
        assert!(cases.len() >= 15);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let sent = case["sent"].as_bool().unwrap().then(|| Outcome {
                status: status(case),
                resolution: case["resolution"].as_str(),
            });
            let outcome = answer_outcome(sent, case["resolved"].as_bool().unwrap());
            assert_eq!(answer_state(&outcome), case["expect"]["state"], "{name}");
            assert_eq!(
                outcome.text().as_deref(),
                case["expect"]["text"].as_str(),
                "{name}"
            );
        }
    }

    #[test]
    fn stop_vectors() {
        let vectors = vectors();
        let cases = vectors["stop"].as_array().unwrap();
        assert!(cases.len() >= 8);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let outcome = stop_outcome(Outcome {
                status: status(case),
                resolution: case["resolution"].as_str(),
            });
            assert_eq!(stop_state(&outcome), case["expect"]["state"], "{name}");
            assert_eq!(
                outcome.text().as_deref(),
                case["expect"]["text"].as_str(),
                "{name}"
            );
        }
    }

    #[test]
    fn outcome_reads_the_named_command() {
        let entry = |id: &str, status, resolution: Option<&str>| SessionCommandEntry {
            id: id.into(),
            payload: cypher_doc::SessionCommandPayload::Interrupt {},
            issued_by: "d".into(),
            issued_at: 1,
            based_on: None,
            expires_at: None,
            status,
            resolution: resolution.map(str::to_string),
            sent_at: None,
            dismissed: false,
            author: None,
        };
        let commands = vec![
            entry("a", SessionCommandStatus::Applied, None),
            entry("b", SessionCommandStatus::Rejected, Some("no")),
        ];
        assert_eq!(
            Outcome::of(&commands, Some("b")),
            Outcome {
                status: Some(SessionCommandStatus::Rejected),
                resolution: Some("no"),
            }
        );
        assert_eq!(Outcome::of(&commands, Some("c")).status, None);
        assert_eq!(Outcome::of(&commands, None).status, None);
    }
}

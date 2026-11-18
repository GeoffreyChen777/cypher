//! The owner's own words, for the Runtime's permission check
//! (`CYPHER_OWNER_WORDS`, docs/design/permission-control.md §7.3).
//!
//! A dangerous command (a push, a release tag, merging a pull request) runs
//! without a dialog only when the owner's own words ask for it. The words
//! come from the chat's command ledger, not from the conversation Pi keeps:
//! there a plan's brief, an orchestrator's steering, a subagent's task or a
//! peer's question sit as user messages like any other. Here each message
//! command says who wrote it ([`SessionCommandEntry::author`], set only by
//! the engine's own agents), and only what a person sent counts, plus the
//! instruction a plan carries out, from its record.
//!
//! The words reach Pi in its environment, which the agent can't change.

use cypher_doc::{SessionCommandEntry, SessionCommandPayload, SessionCommandStatus};
use serde::Serialize;

use super::DocHost;

/// How many of the owner's messages a run carries, newest last.
pub(crate) const OWNER_MESSAGES_MAX: usize = 10;
/// The most of one message a run carries.
pub(crate) const OWNER_MESSAGE_MAX: usize = 2_000;
/// The most a run carries in all (it rides an environment variable).
pub(crate) const OWNER_WORDS_MAX: usize = 12_000;

/// How a chat began: whether an agent started it, and the instruction a
/// plan it belongs to carries out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ChatOrigin {
    /// An agent wrote its first message: a plan's session or background
    /// thread, a subagent, a routine, a peer's thread, a memory pass.
    pub agent_started: bool,
    /// The owner's message the plan was proposed from, verbatim (the work's
    /// `instruction`).
    pub instruction: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnerWords {
    /// Oldest first.
    pub messages: Vec<OwnerWord>,
    /// Some of the chat's messages were written by an agent.
    pub agent_written: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct OwnerWord {
    pub text: String,
    /// `typed` in this chat, or the `plan`'s instruction.
    pub source: &'static str,
}

impl OwnerWords {
    pub(crate) fn to_env(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl DocHost {
    /// The owner's own words in `chat_id`, as `CYPHER_OWNER_WORDS` carries
    /// them; `None` when its ledger can't be read (the run then gets none,
    /// and no danger runs on an explicit request).
    pub(crate) fn owner_words_env(&self, chat_id: &str, origin: &ChatOrigin) -> Option<String> {
        let handle = self.open(chat_id).ok()?;
        if handle.is_ephemeral() {
            return None;
        }
        let commands = handle.doc.read_commands().ok()?;
        let ran = |command: &SessionCommandEntry| {
            self.inner.store.is_processed(&command.id).unwrap_or(false)
        };
        Some(owner_words(&commands, ran, origin).to_env())
    }
}

/// Message ids the engine's agents minted before commands said who wrote
/// them: a late answer from another workspace, a peer's question, a plan's
/// brief.
fn legacy_agent_id(message_id: &str) -> bool {
    message_id.starts_with("bot-answer-")
        || message_id.starts_with("peer-")
        || message_id.ends_with("-brief")
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// The owner's words in a chat's commands: the messages a person sent that
/// the host ran (or is running), oldest first, with the plan's instruction
/// ahead of them. `ran` says whether a pending command was taken for
/// execution (the processed ledger).
pub(crate) fn owner_words(
    commands: &[SessionCommandEntry],
    ran: impl Fn(&SessionCommandEntry) -> bool,
    origin: &ChatOrigin,
) -> OwnerWords {
    let mut agent_written = origin.agent_started;
    // The first message's id: in a chat an agent started it is the agent's,
    // even when written before commands said so (its retries share the id).
    let mut first: Option<&str> = None;
    let mut seen = std::collections::HashSet::new();
    let mut typed: Vec<String> = Vec::new();
    for command in commands {
        let (text, message_id) = match &command.payload {
            SessionCommandPayload::Run {
                request,
                message_id,
                ..
            } => (request.prompt.as_str(), Some(message_id.as_str())),
            SessionCommandPayload::Steer {
                prompt, message_id, ..
            } => (prompt.as_str(), message_id.as_deref()),
            _ => continue,
        };
        let first_id = *first.get_or_insert(message_id.unwrap_or(command.id.as_str()));
        let agents = command.author.is_some()
            || message_id.is_some_and(legacy_agent_id)
            || (origin.agent_started && message_id.unwrap_or(command.id.as_str()) == first_id);
        if agents {
            agent_written = true;
            continue;
        }
        let taken = match command.status {
            SessionCommandStatus::Applied => true,
            SessionCommandStatus::Pending => ran(command),
            SessionCommandStatus::Rejected
            | SessionCommandStatus::Expired
            | SessionCommandStatus::Superseded => false,
        };
        if !taken || text.trim().is_empty() {
            continue;
        }
        if let Some(id) = message_id
            && !seen.insert(id.to_string())
        {
            continue;
        }
        typed.push(clip(text, OWNER_MESSAGE_MAX));
    }
    let instruction = origin
        .instruction
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(|text| clip(text, OWNER_MESSAGE_MAX));
    let room = OWNER_MESSAGES_MAX - usize::from(instruction.is_some());
    if typed.len() > room {
        typed.drain(..typed.len() - room);
    }
    // Within the size: the oldest typed messages go first, the plan's
    // instruction last.
    let size = |typed: &[String]| {
        typed.iter().map(|t| t.chars().count()).sum::<usize>()
            + instruction.as_deref().map_or(0, |t| t.chars().count())
    };
    while !typed.is_empty() && size(&typed) > OWNER_WORDS_MAX {
        typed.remove(0);
    }
    let mut messages: Vec<OwnerWord> = instruction
        .map(|text| OwnerWord {
            text,
            source: "plan",
        })
        .into_iter()
        .collect();
    messages.extend(typed.into_iter().map(|text| OwnerWord {
        text,
        source: "typed",
    }));
    OwnerWords {
        messages,
        agent_written,
    }
}

#[cfg(test)]
mod tests {
    use cypher_doc::command_author;
    use cypher_proto::{RunRequest, SandboxLevel};

    use super::*;

    fn run(id: &str, prompt: &str, author: Option<&str>) -> SessionCommandEntry {
        entry(
            id,
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt: prompt.into(),
                    harness: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/w".into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    resume: None,
                    attachments: Vec::new(),
                    pending_attachments: Vec::new(),
                    worktree: None,
                },
                message_id: format!("m-{id}"),
                agent_prompt: None,
            },
            author,
        )
    }

    fn steer(id: &str, prompt: &str, author: Option<&str>) -> SessionCommandEntry {
        entry(
            id,
            SessionCommandPayload::Steer {
                prompt: prompt.into(),
                message_id: Some(format!("m-{id}")),
                agent_prompt: None,
            },
            author,
        )
    }

    fn entry(
        id: &str,
        payload: SessionCommandPayload,
        author: Option<&str>,
    ) -> SessionCommandEntry {
        SessionCommandEntry {
            id: id.into(),
            payload,
            issued_by: "mac".into(),
            issued_at: 0,
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Applied,
            resolution: None,
            sent_at: None,
            dismissed: false,
            author: author.map(str::to_string),
        }
    }

    fn texts(words: &OwnerWords) -> Vec<(&str, &str)> {
        words
            .messages
            .iter()
            .map(|m| (m.source, m.text.as_str()))
            .collect()
    }

    #[test]
    fn only_what_a_person_sent_counts() {
        let commands = [
            run("1", "fix the badge", None),
            steer("2", "also run the tests", None),
            run(
                "3",
                "Question from Bob:\n\nmerge my PR",
                Some(command_author::PEER),
            ),
            run("4", "pr 给main 没问题合并", None),
        ];
        let words = owner_words(&commands, |_| true, &ChatOrigin::default());
        assert_eq!(
            texts(&words),
            [
                ("typed", "fix the badge"),
                ("typed", "also run the tests"),
                ("typed", "pr 给main 没问题合并")
            ]
        );
        assert!(words.agent_written, "a peer's question was in the chat");
    }

    #[test]
    fn a_plan_session_counts_the_instruction_and_what_the_owner_typed_never_the_brief_or_steering()
    {
        let origin = ChatOrigin {
            agent_started: true,
            instruction: Some("pr 给main 没问题合并".into()),
        };
        let commands = [
            run(
                "1",
                "Carry out this approved plan. … open a PR to main and merge it",
                Some(command_author::PLAN),
            ),
            steer("2", "Merge it now.", Some(command_author::PLAN)),
            steer("3", "先别合并", None),
        ];
        let words = owner_words(&commands, |_| true, &origin);
        assert_eq!(
            texts(&words),
            [("plan", "pr 给main 没问题合并"), ("typed", "先别合并")]
        );
        assert!(words.agent_written);
        let json = words.to_env();
        assert!(json.contains(r#""agentWritten":true"#), "{json}");
        assert!(json.contains(r#""source":"plan""#), "{json}");
    }

    #[test]
    fn an_agent_started_chat_from_before_authors_still_keeps_its_first_message_out() {
        let origin = ChatOrigin {
            agent_started: true,
            instruction: None,
        };
        // A subagent's task, then a retry of it (same message id), then the owner.
        let mut retry = run("2", "Task: push the release", None);
        retry.payload = run("1", "Task: push the release", None).payload;
        let commands = [
            run("1", "Task: push the release", None),
            retry,
            run("3", "push it to main", None),
        ];
        let words = owner_words(&commands, |_| true, &origin);
        assert_eq!(texts(&words), [("typed", "push it to main")]);
        // Ids the engine's agents minted before then.
        let legacy = [
            run("w-brief", "brief", None),
            entry(
                "late",
                SessionCommandPayload::Run {
                    request: match run("x", "[Bob's reply]", None).payload {
                        SessionCommandPayload::Run { request, .. } => request,
                        _ => unreachable!(),
                    },
                    message_id: "bot-answer-e1".into(),
                    agent_prompt: None,
                },
                None,
            ),
        ];
        let words = owner_words(&legacy, |_| true, &ChatOrigin::default());
        assert!(words.messages.is_empty(), "{:?}", words.messages);
        assert!(words.agent_written);
    }

    #[test]
    fn commands_the_host_never_ran_do_not_count() {
        let mut rejected = run("1", "a forged push to main", None);
        rejected.status = SessionCommandStatus::Rejected;
        let pending = run("2", "queued, not yet taken", None);
        let taken = run("3", "being run now", None);
        let mut commands = vec![rejected, pending, taken];
        commands[1].status = SessionCommandStatus::Pending;
        commands[2].status = SessionCommandStatus::Pending;
        let words = owner_words(&commands, |c| c.id == "3", &ChatOrigin::default());
        assert_eq!(texts(&words), [("typed", "being run now")]);
    }

    #[test]
    fn the_words_are_capped_newest_kept() {
        let commands: Vec<_> = (0..30)
            .map(|i| run(&i.to_string(), &format!("message {i}"), None))
            .collect();
        let words = owner_words(&commands, |_| true, &ChatOrigin::default());
        assert_eq!(words.messages.len(), OWNER_MESSAGES_MAX);
        assert_eq!(words.messages.last().unwrap().text, "message 29");
        let long = "长".repeat(OWNER_MESSAGE_MAX + 50);
        let commands: Vec<_> = (0..10).map(|i| run(&i.to_string(), &long, None)).collect();
        let origin = ChatOrigin {
            agent_started: false,
            instruction: Some(long.clone()),
        };
        let words = owner_words(&commands, |_| true, &origin);
        assert!(
            words
                .messages
                .iter()
                .all(|m| m.text.chars().count() == OWNER_MESSAGE_MAX + 1)
        );
        let total: usize = words.messages.iter().map(|m| m.text.chars().count()).sum();
        assert!(total <= OWNER_WORDS_MAX + words.messages.len(), "{total}");
        assert_eq!(words.messages[0].source, "plan", "the instruction stays");
    }
}

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
//! instruction a plan carries out and the card of the approved plan, from
//! its record. Each word says its kind ([`source`]): a message the owner
//! typed, a short one answering the agent message right before it (a
//! `reply`, with that message from the transcript), the owner's answer to
//! an agent's question (an `answer`, with the question; never a permission
//! dialog's), the plan's instruction or card
//! (docs/design/permission-control.md §7.3.4).
//!
//! The words reach Pi in its environment, which the agent can't change.

use cypher_doc::{
    MessagePart, MessageRole, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry, join_continuation_entries,
};
use cypher_proto::BotWork;
use serde::Serialize;

use super::DocHost;

/// How many of the owner's messages a run carries, newest last (typed
/// messages, replies and answers together; the plan's instruction and card
/// take their places from these).
pub(crate) const OWNER_MESSAGES_MAX: usize = 10;
/// The most of one message a run carries.
pub(crate) const OWNER_MESSAGE_MAX: usize = 2_000;
/// The most of an approved plan's card a run carries.
pub(crate) const OWNER_CARD_MAX: usize = 3_000;
/// The most of the agent message a reply answers, and of a question an
/// answer answers: their latest part, which holds what was proposed or
/// asked last.
pub(crate) const OWNER_CONTEXT_MAX: usize = 1_200;
/// The most a run carries in all, every field counted (it rides an
/// environment variable).
pub(crate) const OWNER_WORDS_MAX: usize = 12_000;
/// A message no longer than this (letters and digits, a CJK character
/// counting twice, punctuation and spaces not at all) that directly follows
/// an agent message is a reply to it: "做", "按照建议", "yes, the second
/// one". A longer one stands on its own words. The bound is language
/// independent and says nothing about what the reply means; whether it
/// agrees is the reviewing model's call.
pub(crate) const REPLY_MAX_WEIGHT: usize = 60;

/// Where an owner word comes from (`OwnerWord::source`).
pub(crate) mod source {
    /// A message the owner typed in this chat.
    pub const TYPED: &str = "typed";
    /// The owner's instruction a plan carries out, from the plan's record.
    pub const PLAN: &str = "plan";
    /// The owner's answer to a question an agent asked (ask_user).
    pub const ANSWER: &str = "answer";
    /// An approved plan's card, from the plan's record.
    pub const CARD: &str = "card";
    /// A short message answering the agent message right before it.
    pub const REPLY: &str = "reply";
}

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
    /// The card of the approved plan the chat belongs to ([`plan_card`]):
    /// what the owner approved, from the work's record.
    pub card: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnerWords {
    /// Oldest first (the plan's instruction and card ahead of the rest).
    pub messages: Vec<OwnerWord>,
    /// Some of the chat's messages were written by an agent.
    pub agent_written: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnerWord {
    /// What the owner said: a typed message, a plan's instruction, the
    /// option(s) or words an answer gave, an approved card.
    pub text: String,
    /// One of [`source`].
    pub source: &'static str,
    /// `answer`: the question as it was shown (header, question, options).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// `reply`: the agent message it answers (its latest part).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// Epoch millis the owner said it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

impl OwnerWord {
    fn new(text: String, source: &'static str) -> Self {
        Self {
            text,
            source,
            question: None,
            proposal: None,
            at: None,
        }
    }

    fn size(&self) -> usize {
        [
            Some(&self.text),
            self.question.as_ref(),
            self.proposal.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|t| t.chars().count())
        .sum()
    }
}

/// The least a quote of the owner's words says ([`weight`]): a word or two,
/// never a lone "ok" that any message contains.
pub(crate) const QUOTE_MIN_WEIGHT: usize = 4;

impl OwnerWords {
    pub(crate) fn to_env(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Whether `quote` is the owner's own: word for word (case and spacing
    /// aside) in what they typed, replied or answered, never in a plan's
    /// instruction or card, which only restate earlier words. Whether the
    /// words mean what an agent cites them for is the owner's to confirm
    /// (a plan's card shows the quote with the level it asks for).
    pub(crate) fn quotes(&self, quote: &str) -> bool {
        let quote = normalized(quote);
        if weight(&quote) < QUOTE_MIN_WEIGHT {
            return false;
        }
        self.messages
            .iter()
            .filter(|word| matches!(word.source, source::TYPED | source::REPLY | source::ANSWER))
            .any(|word| normalized(&word.text).contains(&quote))
    }
}

/// `text` lowercased, every run of whitespace one space.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl DocHost {
    /// The owner's own words in `chat_id`, as `CYPHER_OWNER_WORDS` carries
    /// them; `None` when its ledger can't be read (the run then gets none,
    /// and no danger runs on an explicit request).
    pub(crate) fn owner_words_env(&self, chat_id: &str, origin: &ChatOrigin) -> Option<String> {
        Some(self.owner_words_of(chat_id, origin)?.to_env())
    }

    /// Whether `quote` is in the owner's own words in `chat_id`
    /// ([`OwnerWords::quotes`]); `false` when its ledger can't be read.
    pub(crate) fn owner_quoted(&self, chat_id: &str, quote: &str) -> bool {
        self.owner_words_of(chat_id, &ChatOrigin::default())
            .is_some_and(|words| words.quotes(quote))
    }

    fn owner_words_of(&self, chat_id: &str, origin: &ChatOrigin) -> Option<OwnerWords> {
        let handle = self.open(chat_id).ok()?;
        if handle.is_ephemeral() {
            return None;
        }
        let commands = handle.doc.read_commands().ok()?;
        // The transcript only adds context (what a reply answers, what a
        // question asked): unreadable, the words stand without it.
        let entries = join_continuation_entries(handle.doc.read_entries().unwrap_or_default());
        let ran = |command: &SessionCommandEntry| {
            self.inner.store.is_processed(&command.id).unwrap_or(false)
        };
        Some(owner_words(&commands, &entries, ran, origin))
    }
}

/// An approved plan's card as the owner saw it when they approved it:
/// title, summary, and each step with its details, from the work's record
/// (never from a session's brief, which an agent wrote). `None` for a plan
/// nobody approved.
pub(crate) fn plan_card(work: &BotWork) -> Option<String> {
    if !work
        .decision
        .as_ref()
        .is_some_and(|decision| decision.approve)
    {
        return None;
    }
    let mut card = work.title.trim().to_string();
    if let Some(summary) = work
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        card.push('\n');
        card.push_str(summary);
    }
    for (index, step) in work.steps.iter().enumerate() {
        card.push_str(&format!("\n{}. {}", index + 1, step.title.trim()));
        if let Some(detail) = step
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            card.push('\n');
            card.push_str(detail);
        }
    }
    let card = card.trim();
    (!card.is_empty()).then(|| clip(card, OWNER_CARD_MAX))
}

/// Message ids the engine's agents minted before commands said who wrote
/// them: a late answer from another workspace, a peer's question, a plan's
/// brief.
fn legacy_agent_id(message_id: &str) -> bool {
    message_id.starts_with("bot-answer-")
        || message_id.starts_with("peer-")
        || message_id.ends_with("-brief")
}

/// The start of `text`, at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// The end of `text`, at most `max` characters: what an agent proposed or
/// asked last.
fn clip_tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(count - max));
    out
}

/// How much a message says: its letters and digits, a CJK character
/// counting twice; spaces, punctuation and symbols not at all.
fn weight(text: &str) -> usize {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| {
            let wide = matches!(c as u32,
                0x1100..=0x11FF | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
                | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF);
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// The text an agent message shows: its text parts, in order.
fn shown_text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The agent message a message answers: the transcript entry right before
/// the message's own, when that is the agent's and shows text. Only that
/// one binds, never an earlier one.
fn proposal_before(entries: &[SessionMessageEntry], message_id: &str) -> Option<String> {
    let at = entries.iter().position(|entry| entry.id == message_id)?;
    let before = entries.get(at.checked_sub(1)?)?;
    if before.role != MessageRole::Assistant {
        return None;
    }
    let text = shown_text(before);
    (!text.is_empty()).then(|| clip_tail(&text, OWNER_CONTEXT_MAX))
}

/// The owner's answers in a `RespondInput`: each question an agent asked
/// (never a permission prompt: "Run once" consents to nothing else) with
/// what they picked or wrote.
fn answers(
    entries: &[SessionMessageEntry],
    request_id: &str,
    given: &[cypher_proto::UserInputAnswer],
    at: i64,
) -> Vec<OwnerWord> {
    let questions = entries
        .iter()
        .rev()
        .flat_map(|entry| &entry.parts)
        .find_map(|part| match part {
            MessagePart::Input {
                request_id: id,
                questions,
                ..
            } if id == request_id => Some(questions),
            _ => None,
        });
    let Some(questions) = questions else {
        return Vec::new();
    };
    given
        .iter()
        .filter_map(|answer| {
            let question = questions.iter().find(|q| q.id == answer.question_id)?;
            if question.permission.is_some() {
                return None;
            }
            let picked: Vec<&str> = answer
                .labels
                .iter()
                .map(|label| label.trim())
                .filter(|label| !label.is_empty())
                .collect();
            if picked.is_empty() {
                return None;
            }
            let mut shown = [question.header.trim(), question.question.trim()]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !question.options.is_empty() {
                shown.push_str("\nOptions: ");
                shown.push_str(&question.options.join(" | "));
            }
            Some(OwnerWord {
                question: Some(clip_tail(&shown, OWNER_CONTEXT_MAX)),
                at: Some(at),
                ..OwnerWord::new(clip(&picked.join("; "), OWNER_MESSAGE_MAX), source::ANSWER)
            })
        })
        .collect()
}

/// The owner's words in a chat's commands: the messages a person sent that
/// the host ran (or is running) and the answers they gave, oldest first,
/// with the plan's instruction and approved card ahead of them. A short
/// message right after an agent message is a `reply` carrying that message
/// (`entries`, the transcript, continuations joined). `ran` says whether a
/// pending command was taken for execution (the processed ledger).
pub(crate) fn owner_words(
    commands: &[SessionCommandEntry],
    entries: &[SessionMessageEntry],
    ran: impl Fn(&SessionCommandEntry) -> bool,
    origin: &ChatOrigin,
) -> OwnerWords {
    let mut agent_written = origin.agent_started;
    // The first message's id: in a chat an agent started it is the agent's,
    // even when written before commands said so (its retries share the id).
    let mut first: Option<&str> = None;
    let mut seen = std::collections::HashSet::new();
    let mut said: Vec<OwnerWord> = Vec::new();
    for command in commands {
        let taken = match command.status {
            SessionCommandStatus::Applied => true,
            SessionCommandStatus::Pending => ran(command),
            SessionCommandStatus::Rejected
            | SessionCommandStatus::Expired
            | SessionCommandStatus::Superseded => false,
        };
        let (text, message_id) = match &command.payload {
            SessionCommandPayload::Run {
                request,
                message_id,
                ..
            } => (request.prompt.as_str(), Some(message_id.as_str())),
            SessionCommandPayload::Steer {
                prompt, message_id, ..
            } => (prompt.as_str(), message_id.as_deref()),
            SessionCommandPayload::RespondInput {
                request_id,
                answers: given,
            } => {
                if command.author.is_none() && taken {
                    said.extend(answers(entries, request_id, given, command.issued_at));
                }
                continue;
            }
            SessionCommandPayload::Interrupt {} => continue,
        };
        let first_id = *first.get_or_insert(message_id.unwrap_or(command.id.as_str()));
        let agents = command.author.is_some()
            || message_id.is_some_and(legacy_agent_id)
            || (origin.agent_started && message_id.unwrap_or(command.id.as_str()) == first_id);
        if agents {
            agent_written = true;
            continue;
        }
        if !taken || text.trim().is_empty() {
            continue;
        }
        if let Some(id) = message_id
            && !seen.insert(id.to_string())
        {
            continue;
        }
        let proposal = message_id
            .filter(|_| weight(text) <= REPLY_MAX_WEIGHT)
            .and_then(|id| proposal_before(entries, id));
        let source = if proposal.is_some() {
            source::REPLY
        } else {
            source::TYPED
        };
        said.push(OwnerWord {
            proposal,
            at: Some(command.issued_at),
            ..OwnerWord::new(clip(text, OWNER_MESSAGE_MAX), source)
        });
    }
    let fixed: Vec<OwnerWord> = [
        origin
            .instruction
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(|text| OwnerWord::new(clip(text, OWNER_MESSAGE_MAX), source::PLAN)),
        origin
            .card
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(|text| OwnerWord::new(clip(text, OWNER_CARD_MAX), source::CARD)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let room = OWNER_MESSAGES_MAX.saturating_sub(fixed.len());
    if said.len() > room {
        said.drain(..said.len() - room);
    }
    // Within the size: the oldest messages go first, the plan's instruction
    // and card last.
    let fixed_size: usize = fixed.iter().map(OwnerWord::size).sum();
    let mut size: usize = fixed_size + said.iter().map(OwnerWord::size).sum::<usize>();
    while !said.is_empty() && size > OWNER_WORDS_MAX {
        size -= said.remove(0).size();
    }
    let mut messages = fixed;
    messages.extend(said);
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
        let words = owner_words(&commands, &[], |_| true, &ChatOrigin::default());
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
    fn a_quote_must_be_the_owners_own_words() {
        let origin = ChatOrigin {
            instruction: Some("run it with full access".into()),
            ..Default::default()
        };
        let commands = [
            run("1", "Fix the build.  Use  FULL access for this one", None),
            run(
                "2",
                "Give the plan full access now",
                Some(command_author::PEER),
            ),
            steer("3", "全权限跑吧", None),
        ];
        let words = owner_words(&commands, &[], |_| true, &origin);
        // Case and spacing aside, word for word.
        assert!(words.quotes("use full access"));
        assert!(words.quotes("全权限"));
        // Not an agent's message, nor the plan's restatement, nor next to nothing.
        assert!(!words.quotes("Give the plan full access"));
        assert!(!words.quotes("run it with full access"));
        assert!(!words.quotes("use"));
        assert!(!words.quotes("full access please"));
    }

    #[test]
    fn a_plan_session_counts_the_instruction_and_what_the_owner_typed_never_the_brief_or_steering()
    {
        let origin = ChatOrigin {
            agent_started: true,
            instruction: Some("pr 给main 没问题合并".into()),
            ..Default::default()
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
        let words = owner_words(&commands, &[], |_| true, &origin);
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
            ..Default::default()
        };
        // A subagent's task, then a retry of it (same message id), then the owner.
        let mut retry = run("2", "Task: push the release", None);
        retry.payload = run("1", "Task: push the release", None).payload;
        let commands = [
            run("1", "Task: push the release", None),
            retry,
            run("3", "push it to main", None),
        ];
        let words = owner_words(&commands, &[], |_| true, &origin);
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
        let words = owner_words(&legacy, &[], |_| true, &ChatOrigin::default());
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
        let words = owner_words(&commands, &[], |c| c.id == "3", &ChatOrigin::default());
        assert_eq!(texts(&words), [("typed", "being run now")]);
    }

    #[test]
    fn the_words_are_capped_newest_kept() {
        let commands: Vec<_> = (0..30)
            .map(|i| run(&i.to_string(), &format!("message {i}"), None))
            .collect();
        let words = owner_words(&commands, &[], |_| true, &ChatOrigin::default());
        assert_eq!(words.messages.len(), OWNER_MESSAGES_MAX);
        assert_eq!(words.messages.last().unwrap().text, "message 29");
        let long = "长".repeat(OWNER_MESSAGE_MAX + 50);
        let commands: Vec<_> = (0..10).map(|i| run(&i.to_string(), &long, None)).collect();
        let origin = ChatOrigin {
            agent_started: false,
            instruction: Some(long.clone()),
            ..Default::default()
        };
        let words = owner_words(&commands, &[], |_| true, &origin);
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

    fn said(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts,
            created_at: 0,
            device_id: "mac".into(),
            status: None,
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
            models: Vec::new(),
            turn_changes: None,
            recovery: None,
        }
    }

    fn text(text: &str) -> MessagePart {
        MessagePart::Text {
            id: "t0".into(),
            text: text.into(),
            agent_text: None,
        }
    }

    fn question(id: &str, options: &[&str], permission: bool) -> cypher_proto::UserInputQuestion {
        cypher_proto::UserInputQuestion {
            id: id.into(),
            header: "Repository".into(),
            question: "How should the new repository be created?".into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi_select: false,
            permission: permission.then_some(cypher_proto::PermissionPrompt {
                kind: cypher_proto::PermissionPromptKind::Escalate,
                tool_call_id: None,
            }),
            asked_by: None,
        }
    }

    fn respond(id: &str, request_id: &str, answers: &[(&str, &str)]) -> SessionCommandEntry {
        entry(
            id,
            SessionCommandPayload::RespondInput {
                request_id: request_id.into(),
                answers: answers
                    .iter()
                    .map(|(q, label)| cypher_proto::UserInputAnswer {
                        question_id: q.to_string(),
                        labels: vec![label.to_string()],
                    })
                    .collect(),
            },
            None,
        )
    }

    #[test]
    fn a_short_message_right_after_an_agent_message_is_a_reply_to_it() {
        let commands = [
            run("1", "look into the flaky test", None),
            run("2", "做", None),
            run("3", "ok", None),
            run(
                "4",
                &"a long message that says what to do on its own ".repeat(4),
                None,
            ),
        ];
        let entries = [
            said(
                "m-1",
                MessageRole::User,
                vec![text("look into the flaky test")],
            ),
            said(
                "a1",
                MessageRole::Assistant,
                vec![
                    text("Earlier idea."),
                    text("I propose: open a PR to main and merge it."),
                ],
            ),
            said("m-2", MessageRole::User, vec![text("做")]),
            // Two owner messages in a row: the second answers no agent message.
            said("m-3", MessageRole::User, vec![text("ok")]),
            said(
                "a2",
                MessageRole::Assistant,
                vec![text("Shall I delete the branch?")],
            ),
            said("m-4", MessageRole::User, vec![text("long")]),
        ];
        let words = owner_words(&commands, &entries, |_| true, &ChatOrigin::default());
        let sources: Vec<_> = words.messages.iter().map(|m| m.source).collect();
        assert_eq!(sources, ["typed", "reply", "typed", "typed"]);
        let reply = &words.messages[1];
        assert_eq!(reply.text, "做");
        assert_eq!(
            reply.proposal.as_deref(),
            Some("Earlier idea.\n\nI propose: open a PR to main and merge it.")
        );
        assert!(
            words.messages[3].proposal.is_none(),
            "a long message stands on its own words"
        );
        // A long proposal keeps its latest part.
        let long = format!("{}the last proposal", "x".repeat(OWNER_CONTEXT_MAX * 2));
        let entries = [
            said("a", MessageRole::Assistant, vec![text(&long)]),
            said("m-2", MessageRole::User, vec![text("做")]),
        ];
        let words = owner_words(&commands[1..2], &entries, |_| true, &ChatOrigin::default());
        let proposal = words.messages[0].proposal.as_deref().unwrap();
        assert!(proposal.ends_with("the last proposal"), "{proposal}");
        assert_eq!(proposal.chars().count(), OWNER_CONTEXT_MAX + 1);
    }

    #[test]
    fn the_owners_answers_to_questions_count_with_the_question_never_a_permission_dialogs() {
        let entries = [
            said("m-1", MessageRole::User, vec![text("set up the repo")]),
            said(
                "a1",
                MessageRole::Assistant,
                vec![MessagePart::Input {
                    id: "i0".into(),
                    request_id: "r1".into(),
                    questions: vec![question(
                        "q1",
                        &["Create a private repo", "Create a public repo"],
                        false,
                    )],
                    resolved: true,
                }],
            ),
            said(
                "a2",
                MessageRole::Assistant,
                vec![MessagePart::Input {
                    id: "i1".into(),
                    request_id: "r2".into(),
                    questions: vec![question("q1", &["Run once", "Deny"], true)],
                    resolved: true,
                }],
            ),
        ];
        let mut forged = respond("5", "r1", &[("q1", "Create a public repo")]);
        forged.author = Some(cypher_doc::command_author::PLAN.into());
        let commands = [
            run("1", "set up the repo", None),
            respond("2", "r1", &[("q1", "Create a private repo")]),
            respond("3", "r2", &[("q1", "Run once")]),
            respond("4", "missing", &[("q1", "Create a public repo")]),
            forged,
        ];
        let words = owner_words(&commands, &entries, |_| true, &ChatOrigin::default());
        assert_eq!(
            texts(&words),
            [
                ("typed", "set up the repo"),
                ("answer", "Create a private repo")
            ]
        );
        let question = words.messages[1].question.as_deref().unwrap();
        assert!(
            question.contains("How should the new repository be created?"),
            "{question}"
        );
        assert!(
            question.contains("Create a private repo | Create a public repo"),
            "{question}"
        );
        let json = words.to_env();
        assert!(
            json.contains(r#""source":"answer""#) && json.contains(r#""question":"#),
            "{json}"
        );
    }

    #[test]
    fn an_approved_plans_card_counts_from_its_record() {
        let mut work: BotWork = serde_json::from_value(serde_json::json!({
            "id": "w1", "botId": "b", "title": "Ship the fix", "instruction": "fix and ship",
            "summary": "Fix the badge and open a PR", "state": "running", "chatId": "c",
            "createdAt": 0,
            "steps": [{"id": "s1", "title": "Open a PR into main", "detail": "then merge it"}]
        }))
        .unwrap();
        assert_eq!(plan_card(&work), None, "nobody approved it");
        work.decision = Some(cypher_proto::PlanDecision {
            approve: true,
            at: 1,
        });
        let card = plan_card(&work).unwrap();
        assert_eq!(
            card,
            "Ship the fix\nFix the badge and open a PR\n1. Open a PR into main\nthen merge it"
        );
        let origin = ChatOrigin {
            agent_started: true,
            instruction: Some("fix and ship".into()),
            card: Some(card.clone()),
        };
        let words = owner_words(
            &[run("1", "brief", Some(cypher_doc::command_author::PLAN))],
            &[],
            |_| true,
            &origin,
        );
        assert_eq!(
            texts(&words),
            [("plan", "fix and ship"), ("card", card.as_str())]
        );
    }

    #[test]
    fn every_field_counts_toward_the_size() {
        let proposal = "p".repeat(OWNER_CONTEXT_MAX * 3);
        let mut commands = Vec::new();
        let mut entries = Vec::new();
        for i in 0..20 {
            commands.push(run(
                &i.to_string(),
                &format!("{} {i}", "长".repeat(1_500)),
                None,
            ));
            entries.push(said(
                &format!("a{i}"),
                MessageRole::Assistant,
                vec![text(&proposal)],
            ));
            entries.push(said(&format!("m-{i}"), MessageRole::User, vec![text("x")]));
        }
        let words = owner_words(&commands, &entries, |_| true, &ChatOrigin::default());
        let total: usize = words.messages.iter().map(OwnerWord::size).sum();
        assert!(total <= OWNER_WORDS_MAX, "{total}");
        assert!(
            words.messages.last().unwrap().text.ends_with(" 19"),
            "newest kept"
        );
        // Short ones are replies and carry their proposals.
        let commands: Vec<_> = (0..20).map(|i| run(&i.to_string(), "好", None)).collect();
        let words = owner_words(&commands, &entries, |_| true, &ChatOrigin::default());
        assert!(words.messages.iter().all(|m| m.source == "reply"));
        let total: usize = words.messages.iter().map(OwnerWord::size).sum();
        assert!(total <= OWNER_WORDS_MAX, "{total}");
    }
}

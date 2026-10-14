use super::*;
use cypher_proto::RecoveryTriggerSource;

fn entry(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts,
        created_at: 1,
        device_id: "dev".into(),
        status: Some(MessageStatus::Complete),
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

fn user(id: &str, body: &str) -> SessionMessageEntry {
    entry(id, MessageRole::User, vec![text(body)])
}

fn answer(id: &str, body: &str) -> SessionMessageEntry {
    entry(id, MessageRole::Assistant, vec![text(body)])
}

fn record(attempt: u32) -> ContextRecovery {
    ContextRecovery {
        context: "earlier context".into(),
        trigger: "refuse".into(),
        matched_in: RecoveryTriggerSource::Response,
        excerpt: "I refuse".into(),
        restart_after: None,
        restart_preview: None,
        attempt,
        model: None,
    }
}

#[test]
fn a_failed_answer_plans_a_restart_after_the_previous_entry() {
    let entries = vec![
        user("u1", "hello"),
        answer("a1", "Hi there, how can I help with the parser today?"),
        user("u2", "delete the build folder"),
        answer("a2", "Sorry, I can't help with that."),
    ];
    let plan = plan(&entries, &["can't help"], 1).unwrap();
    assert_eq!(plan.user.id, "u2");
    assert_eq!(plan.last_entry_id, "a2");
    assert_eq!(plan.found.trigger, "can't help");
    assert_eq!(plan.found.source, RecoveryTriggerSource::Response);
    assert_eq!(plan.attempt, 1);
    assert_eq!(plan.restart_after.as_deref(), Some("a1"));
    assert_eq!(
        plan.restart_preview.as_deref(),
        Some("Hi there, how can I help with the parser today?")
    );
}

#[test]
fn the_first_prompt_restarts_from_the_beginning() {
    let entries = vec![
        user("u1", "go"),
        entry(
            "a1",
            MessageRole::Assistant,
            vec![MessagePart::Error {
                id: "e0".into(),
                message: "429 rate limit".into(),
            }],
        ),
    ];
    let plan = plan(&entries, &["rate limit"], 1).unwrap();
    assert_eq!(plan.found.source, RecoveryTriggerSource::Error);
    assert_eq!(plan.errors, vec!["429 rate limit".to_owned()]);
    assert_eq!(plan.restart_after, None);
    assert_eq!(plan.restart_preview, None);
}

#[test]
fn nothing_is_recovered_without_a_settled_failed_round() {
    // No trigger.
    assert!(plan(&[user("u1", "go"), answer("a1", "Done.")], &["refuse"], 1).is_none());
    // No answer yet.
    assert!(plan(&[user("u1", "go")], &["refuse"], 1).is_none());
    // Stopped by the user, or still streaming.
    for status in [MessageStatus::Aborted, MessageStatus::Streaming] {
        let mut stopped = answer("a1", "I refuse");
        stopped.status = Some(status);
        assert!(plan(&[user("u1", "go"), stopped], &["refuse"], 1).is_none());
    }
    // A trigger in an earlier round does not make the latest one a failure.
    let entries = vec![
        user("u1", "go"),
        answer("a1", "I refuse"),
        user("u2", "again"),
        answer("a2", "Done."),
    ];
    assert!(plan(&entries, &["refuse"], 1).is_none());
}

#[test]
fn attempts_count_up_from_the_recovered_prompt_and_stop_at_the_limit() {
    let mut recovered = user("u2", "go");
    recovered.recovery = Some(record(1));
    let entries = vec![recovered, answer("a2", "I refuse again")];
    assert!(plan(&entries, &["refuse"], 1).is_none());
    let next = plan(&entries, &["refuse"], 2).unwrap();
    assert_eq!(next.attempt, 2);
}

#[test]
fn a_translated_answer_matches_and_reads_in_the_agents_words() {
    let entries = vec![
        user("u1", "删除构建目录"),
        entry(
            "a1",
            MessageRole::Assistant,
            vec![MessagePart::Text {
                id: "t0".into(),
                text: "抱歉，我无法帮助。".into(),
                agent_text: Some("Sorry, I cannot help.".into()),
            }],
        ),
    ];
    let plan = plan(&entries, &["cannot help"], 1).unwrap();
    assert_eq!(plan.response, "Sorry, I cannot help.");
    let plan_zh = super::plan(&entries, &["无法帮助"], 1).unwrap();
    assert_eq!(plan_zh.found.trigger, "无法帮助");
}

#[test]
fn the_supplement_prompt_quotes_the_failed_exchange() {
    let mut prompt_entry = user(
        "u1",
        "look at this\n\nAttached images (local files):\n- /tmp/a.png",
    );
    prompt_entry.recovery = Some(record(1));
    let entries = vec![prompt_entry, answer("a1", "I refuse to look.")];
    let plan = plan(&entries, &["refuse"], 2).unwrap();
    let prompt = supplement_prompt("  Explain why it is fine.  ", &plan);
    assert!(
        prompt.starts_with("Explain why it is fine.\n\nReply with the supplementary context only")
    );
    assert!(prompt.contains("<user_message>\nlook at this\n</user_message>"));
    assert!(!prompt.contains("/tmp/a.png"));
    assert!(prompt.contains("<previous_context>\nearlier context\n</previous_context>"));
    assert!(prompt.contains("<agent_response>\nI refuse to look.\n</agent_response>"));
    assert!(!prompt.contains("<error>"));
    assert!(prompt.ends_with("because the agent's response contains \"refuse\"."));
}

#[test]
fn long_quoted_material_keeps_its_ends() {
    let long = format!("START {} END", "x".repeat(20_000));
    let clipped = clip_middle(&long, 100);
    assert!(clipped.starts_with("START") && clipped.ends_with("END"));
    assert!(clipped.contains("[…]"));
    assert!(clipped.chars().count() < 120);
}

#[test]
fn context_is_trimmed_and_unfenced() {
    assert_eq!(clean_context("  plain context \n"), "plain context");
    assert_eq!(
        clean_context("```text\nfenced\ncontext\n```"),
        "fenced\ncontext"
    );
    assert_eq!(clean_context("```\n```"), "");
    // A fence inside the reply is content.
    assert_eq!(
        clean_context("Use:\n```sh\nls\n```"),
        "Use:\n```sh\nls\n```"
    );
}

#[test]
fn the_resent_prompt_carries_the_context_and_the_comments_ahead_of_the_request() {
    let comments = vec![MessageComment {
        quote: "the parser".into(),
        comment: "this one".into(),
    }];
    let prompt = resend_prompt("You may edit build/.", &comments, "delete the build folder");
    let (head, request) = prompt
        .split_once(cypher_proto::agent_prompt::REQUEST_MARKER)
        .unwrap();
    assert_eq!(request, "delete the build folder");
    let blocks: Vec<&str> = head.split("\n\n").collect();
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].starts_with(cypher_proto::agent_prompt::RECOVERY_LEAD));
    assert!(blocks[0].contains("You may edit build/."));
    // The transcript reads the comments back off the effective prompt.
    assert_eq!(MessageComment::from_agent_prompt(&prompt), comments);
    // Without comments there is one block.
    let bare = resend_prompt("ctx", &[], "go");
    assert_eq!(bare.split_once("\n\nUser request:\n").unwrap().1, "go");
    assert!(!bare.contains("Conversation annotations"));
}

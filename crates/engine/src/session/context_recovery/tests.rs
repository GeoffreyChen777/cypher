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
        already_done: None,
        failed_attempts: Vec::new(),
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
    assert!(prompt.contains("every attempt failed. Rewrite the context"));
    assert!(prompt.contains(
        "<failed_attempt number=\"1\">\n<context>\nearlier context\n</context>\n\
         <failure>\nI refuse to look.\n</failure>\n</failed_attempt>"
    ));
    assert!(prompt.contains("<agent_response>\nI refuse to look.\n</agent_response>"));
    assert!(!prompt.contains("<error>"));
    assert!(prompt.ends_with("because the agent's response contains \"refuse\"."));
}

/// A third attempt shows the model both failed contexts, oldest first, and
/// its record carries them on.
#[test]
fn every_failed_attempt_reaches_the_next_one() {
    let mut second = record(2);
    second.context = "second context".into();
    second.failed_attempts = vec![FailedRecoveryAttempt {
        attempt: 1,
        context: "first context".into(),
        excerpt: "I refuse, first".into(),
    }];
    let mut prompt_entry = user("u1", "do it");
    prompt_entry.recovery = Some(second);
    let entries = vec![prompt_entry, answer("a1", "I refuse, second")];
    let plan = plan(&entries, &["refuse"], 3).unwrap();
    assert_eq!(plan.attempt, 3);

    let attempts = failed_attempts(&plan);
    assert_eq!(
        attempts
            .iter()
            .map(|a| (a.attempt, a.context.as_str(), a.excerpt.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "first context", "I refuse, first"),
            (2, "second context", "I refuse, second"),
        ]
    );
    let prompt = supplement_prompt("Explain.", &plan);
    let first = prompt.find("<failed_attempt number=\"1\">").unwrap();
    let second = prompt.find("<failed_attempt number=\"2\">").unwrap();
    assert!(first < second);
    assert!(
        prompt.contains(
            "<context>\nfirst context\n</context>\n<failure>\nI refuse, first\n</failure>"
        )
    );
    assert!(prompt.contains(
        "<context>\nsecond context\n</context>\n<failure>\nI refuse, second\n</failure>"
    ));
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
    let prompt = resend_prompt(
        "You may edit build/.",
        None,
        &comments,
        "delete the build folder",
    );
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
    let bare = resend_prompt("ctx", None, &[], "go");
    assert_eq!(bare.split_once("\n\nUser request:\n").unwrap().1, "go");
    assert!(!bare.contains("Conversation annotations"));
}

/// The briefing rides its own one-line block between the context and the
/// comments: a multi-line summary is carried as JSON, so the request marker
/// is still the one that opens the request.
#[test]
fn the_resent_prompt_carries_the_already_done_briefing_as_one_block() {
    let summary = "您已经完成：\n- 编辑 src/config.rs\n\n请继续。";
    let prompt = resend_prompt("ctx", Some(summary), &[], "继续修复");
    let (head, request) = prompt
        .split_once(cypher_proto::agent_prompt::REQUEST_MARKER)
        .unwrap();
    assert_eq!(request, "继续修复");
    let blocks: Vec<&str> = head.split("\n\n").collect();
    assert_eq!(blocks.len(), 2, "one line each: {blocks:?}");
    assert!(blocks[1].starts_with(cypher_proto::agent_prompt::ALREADY_DONE_LEAD));
    assert!(blocks[1].contains("\\n- 编辑 src/config.rs"));
    assert!(!blocks[1].contains('\n'));
}

fn tool_part(id: &str, call: cypher_proto::ToolCall, resolved: bool) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call,
        is_error: false,
        resolved,
        output: Some("2 passed".into()),
        progress: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        permission: None,
    }
}

/// A round that ran tools is planned with them in order, and the summary
/// request quotes each one with how it ended and asks for the user's
/// language.
#[test]
fn a_failed_round_plans_the_tool_work_it_got_through() {
    let entries = vec![
        user("u1", "修复配置解析"),
        entry(
            "a1",
            MessageRole::Assistant,
            vec![
                tool_part(
                    "t1",
                    cypher_proto::ToolCall::EditFile {
                        path: "src/config.rs".into(),
                        old_string: None,
                        new_string: None,
                    },
                    true,
                ),
                tool_part(
                    "t2",
                    cypher_proto::ToolCall::Exec {
                        command: "cargo test".into(),
                    },
                    false,
                ),
                text("I refuse to continue."),
            ],
        ),
    ];
    let plan = plan(&entries, &["refuse"], 1).unwrap();
    assert_eq!(
        plan.tool_steps
            .iter()
            .map(|step| (step.label, step.detail.as_str(), step.resolved))
            .collect::<Vec<_>>(),
        vec![
            ("Edit", "src/config.rs", true),
            ("Run", "cargo test", false),
        ]
    );

    let prompt = already_done_prompt(&plan);
    assert!(prompt.contains("You have already done:"));
    assert!(prompt.contains("SAME LANGUAGE"));
    assert!(prompt.contains("<user_message>\n修复配置解析\n</user_message>"));
    assert!(
        prompt.contains("<step number=\"1\" tool=\"Edit\" outcome=\"succeeded\">\nsrc/config.rs")
    );
    assert!(prompt.contains(
        "<step number=\"2\" tool=\"Run\" outcome=\"never finished — it may have half-happened\">"
    ));
    assert!(prompt.contains("<output>\n2 passed\n</output>"));
    // The context writer is told the work stands, so it writes for an agent
    // carrying on.
    assert!(supplement_prompt("Explain.", &plan).contains("already carried out 2 tool calls"));
}

/// Under a translation, the briefing and the context are written from the
/// prompt the agent received: a translation extension passes both blocks to
/// the agent untranslated, so they must already be in its language.
#[test]
fn under_a_translation_the_recovery_model_reads_the_agents_prompt() {
    let entries = vec![
        entry(
            "u1",
            MessageRole::User,
            vec![MessagePart::Text {
                id: "t0".into(),
                text: "修复配置解析".into(),
                agent_text: Some("Fix the config parsing".into()),
            }],
        ),
        entry(
            "a1",
            MessageRole::Assistant,
            vec![
                tool_part(
                    "t1",
                    cypher_proto::ToolCall::EditFile {
                        path: "src/config.rs".into(),
                        old_string: None,
                        new_string: None,
                    },
                    true,
                ),
                text("I refuse to continue."),
            ],
        ),
    ];
    let plan = plan(&entries, &["refuse"], 1).unwrap();
    for prompt in [
        already_done_prompt(&plan),
        supplement_prompt("Explain.", &plan),
    ] {
        assert!(prompt.contains("<user_message>\nFix the config parsing\n</user_message>"));
        assert!(!prompt.contains("修复配置解析"));
    }
    // The restart point is named as the transcript shows it.
    assert_eq!(plan.restart_preview, None);
    assert_eq!(preview_of(&entries[0]).as_deref(), Some("修复配置解析"));
}

/// A later attempt's briefing request carries the briefing the failed
/// attempt was itself given: work from attempt 1 is not lost when attempt 2
/// fails, even though the rewind took its round out of the transcript.
#[test]
fn a_later_briefing_carries_the_one_the_failed_attempt_was_given() {
    let mut earlier = record(1);
    earlier.already_done =
        Some("You have already done:\n- Read src/main.rs\n\nPlease continue.".into());
    let mut prompt_entry = user("u1", "add the flag");
    prompt_entry.recovery = Some(earlier);
    let entries = vec![
        prompt_entry,
        entry(
            "a1",
            MessageRole::Assistant,
            vec![
                tool_part(
                    "t1",
                    cypher_proto::ToolCall::Exec {
                        command: "cargo build".into(),
                    },
                    true,
                ),
                text("I refuse."),
            ],
        ),
    ];
    let plan = plan(&entries, &["refuse"], 2).unwrap();
    let prompt = already_done_prompt(&plan);
    assert!(prompt.contains(
        "<previous_briefing>\nYou have already done:\n- Read src/main.rs\n\nPlease continue.\n</previous_briefing>"
    ));
    assert!(prompt.contains("carry it into the new briefing"));
    assert!(prompt.contains("tool=\"Run\" outcome=\"succeeded\">\ncargo build"));
}

/// A round with no tool work asks for no briefing at all.
#[test]
fn a_round_with_no_tool_work_has_nothing_to_carry_over() {
    let entries = vec![user("u1", "do it"), answer("a1", "I refuse.")];
    let plan = plan(&entries, &["refuse"], 1).unwrap();
    assert!(plan.tool_steps.is_empty());
    assert_eq!(fallback_already_done(&plan), None);
    assert!(!supplement_prompt("Explain.", &plan).contains("already carried out"));
}

/// The engine's own briefing, for a model that writes nothing usable: every
/// step, with the ones that failed or never finished called out.
#[test]
fn the_fallback_briefing_lists_every_step() {
    let mut running = tool_part(
        "t2",
        cypher_proto::ToolCall::Exec {
            command: "npm install".into(),
        },
        false,
    );
    if let MessagePart::Tool { output, .. } = &mut running {
        *output = None;
    }
    let entries = vec![
        user("u1", "add the flag"),
        entry(
            "a1",
            MessageRole::Assistant,
            vec![
                tool_part(
                    "t1",
                    cypher_proto::ToolCall::ReadFile {
                        path: "src/main.rs".into(),
                    },
                    true,
                ),
                running,
                text("I refuse."),
            ],
        ),
    ];
    let plan = plan(&entries, &["refuse"], 1).unwrap();
    assert_eq!(
        fallback_already_done(&plan).unwrap(),
        "You have already done:\n\
         - Read src/main.rs\n\
         - Run npm install — never finished; check it before relying on it\n\n\
         Please continue."
    );
}

use super::*;
use cypher_doc::MessageRole;

fn entry(id: &str, text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.to_string(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: text.to_string(),
            agent_text: None,
        }],
        created_at: 0,
        device_id: "dev".into(),
        status: None,
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    }
}

#[test]
fn transcript_context_tails_through_anchor() {
    let entries = vec![entry("a", "one"), entry("b", "two"), entry("c", "three")];
    assert_eq!(
        bounded_transcript_context(&entries, Some("b")),
        Some("user: one\n\nuser: two".to_string())
    );
}

#[test]
fn transcript_context_missing_anchor_tails() {
    let entries = vec![entry("a", "one"), entry("b", "two"), entry("c", "three")];
    assert_eq!(
        bounded_transcript_context(&entries, Some("nope")),
        Some("user: one\n\nuser: two\n\nuser: three".to_string())
    );
    assert_eq!(
        bounded_transcript_context(&entries, None),
        Some("user: one\n\nuser: two\n\nuser: three".to_string())
    );
}

#[test]
fn transcript_context_empty_is_none() {
    assert_eq!(bounded_transcript_context(&[], None), None);
    assert_eq!(bounded_transcript_context(&[entry("a", "  ")], None), None);
}

#[test]
fn transcript_context_caps_at_8_newest_messages() {
    // the parent context window is the NEWEST whole
    // messages through the anchor, capped at 8 — older messages never
    // leak into the first send.
    let entries: Vec<_> = (0..12)
        .map(|i| entry(&format!("m{i}"), &format!("msg {i}")))
        .collect();
    let out = bounded_transcript_context(&entries, None).unwrap();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 8);
    assert_eq!(lines[0], "user: msg 4"); // the 5th of 12 = the first of the newest 8
    assert_eq!(lines[7], "user: msg 11");
    // Anchored: through the anchor, still capped at 8.
    let anchored = bounded_transcript_context(&entries, Some("m10")).unwrap();
    assert_eq!(anchored.lines().filter(|l| !l.is_empty()).count(), 8);
    assert_eq!(
        anchored.lines().rfind(|l| !l.is_empty()),
        Some("user: msg 10")
    );
}

#[test]
fn transcript_context_keeps_whole_newest_messages_within_budget() {
    // with a tight budget the NEWEST whole messages are
    // kept and OLDER ones are dropped — never a mid-message truncation
    // of a newer entry. 3 messages of 30 KiB each can't all fit 48 KiB,
    // so only the newest (c) survives whole.
    let big = "x".repeat(30 * 1024);
    let entries = vec![entry("a", &big), entry("b", &big), entry("c", &big)];
    let out = bounded_transcript_context(&entries, None).unwrap();
    assert!(out.chars().count() <= 48 * 1024);
    assert!(out.starts_with("user: xxx"));
    // Newest message kept whole; the OLDER ones were dropped whole.
    assert_eq!(out.matches("user: ").count(), 1);
    // A single message larger than the whole budget contributes its
    // head (the one allowed truncation) — NEVER an empty window.
    let huge = "y".repeat(100 * 1024);
    let out = bounded_transcript_context(&[entry("a", &huge)], None).unwrap();
    assert!(
        !out.is_empty(),
        "oversized newest message still yields context"
    );
    assert_eq!(
        out.chars().count(),
        48 * 1024,
        "head capped to the full budget"
    );
    assert!(
        out.starts_with("user: y"),
        "head keeps role prefix + content"
    );
    // A huge NEWEST message alongside a smaller OLDER one: the older
    // whole message is dropped and the newest contributes its head —
    // never empty, never over budget, content from the newest.
    let out =
        bounded_transcript_context(&[entry("old", &"x".repeat(100)), entry("new", &huge)], None)
            .unwrap();
    assert!(!out.is_empty());
    assert!(out.starts_with("user: y"), "newest content wins: {out}");
    assert!(out.chars().count() <= 48 * 1024);
    // Separators count against the budget: two 24 KiB messages fit the
    // cap by character count alone, but the "\n\n" separator between
    // them would push the joined window over — the oldest is dropped so
    // the result stays <= the cap.
    let a = "a".repeat(24 * 1024 - 6);
    let b = "b".repeat(24 * 1024 - 6);
    let out = bounded_transcript_context(&[entry("a", &a), entry("b", &b)], None).unwrap();
    assert!(out.chars().count() <= 48 * 1024, "stays within the budget");
    assert!(out.starts_with("user: b"), "newest kept: {out}");
    assert_eq!(
        out.matches("user: ").count(),
        1,
        "oldest dropped over the separator edge"
    );
}

/// A translated part contributes the agent's own words: the side chat's
/// agent reads what its parent's agent wrote and was sent.
#[test]
fn transcript_context_uses_the_agents_words_under_a_translation() {
    let mut prompt = entry("u1", "你好");
    prompt.parts[0] = MessagePart::Text {
        id: "t0".into(),
        text: "你好".into(),
        agent_text: Some("Hello".into()),
    };
    let mut answer = entry("a1", "你好！");
    answer.role = MessageRole::Assistant;
    answer.parts[0] = MessagePart::Text {
        id: "t0".into(),
        text: "你好！".into(),
        agent_text: Some("Hi!".into()),
    };
    assert_eq!(
        bounded_transcript_context(&[prompt, answer], None).as_deref(),
        Some("user: Hello\n\nassistant: Hi!")
    );
}

#[test]
fn transcript_context_serializes_safe_visible_content_only() {
    // Role prefixes + safe summaries; hidden reasoning, tool output bytes
    // and command ledger never enter the window.
    let entries = vec![
        entry("u1", "my question"),
        SessionMessageEntry {
            id: "a1".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "t1".into(),
                    text: "visible answer".into(),
                    agent_text: None,
                },
                MessagePart::Tool {
                    id: "t2".into(),
                    call: cypher_proto::ToolCall::Exec {
                        command: "ls".into(),
                    },
                    is_error: false,
                    resolved: true,
                    output: Some("huge raw output that must never appear".into()),
                    progress: None,
                    diff: None,
                    output_ref: None,
                    output_bytes: Some(999_999),
                    diff_ref: None,
                    diff_stats: None,
                },
                MessagePart::Error {
                    id: "t3".into(),
                    message: "boom".into(),
                },
                MessagePart::Input {
                    id: "t4".into(),
                    request_id: "r1".into(),
                    questions: vec![cypher_proto::UserInputQuestion {
                        id: "q1".into(),
                        header: String::new(),
                        question: "which one?".into(),
                        options: vec![],
                        multi_select: false,
                    }],
                    resolved: true,
                },
            ],
            created_at: 1,
            device_id: "dev".into(),
            status: None,
            continuation_of: None,
            completed_at: None,
            comments: Vec::new(),
            models: Vec::new(),
        },
    ];
    let out = bounded_transcript_context(&entries, None).unwrap();
    assert!(out.starts_with("user: my question"));
    assert!(out.contains("assistant: visible answer"));
    assert!(out.contains("[tool: exec]"));
    assert!(
        !out.contains("huge raw output"),
        "raw output never enters context"
    );
    assert!(
        !out.contains("999_999") && !out.contains("999999"),
        "byte counts never enter context"
    );
    assert!(out.contains("[error: boom]"));
    assert!(out.contains("[question: which one?]"));
}

#[test]
fn title_from_selected_quote_is_deterministic() {
    assert_eq!(
        side_chat_title("Fix the flaky network test in CI"),
        "Fix the flaky network test"
    );
    assert_eq!(side_chat_title("   "), "Side chat");
    // Capped in length with an ellipsis.
    let long = side_chat_title(&"w".repeat(200));
    assert!(long.chars().count() <= 49);
    assert!(long.ends_with('…'));
}

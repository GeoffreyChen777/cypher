use super::*;
use cypher_doc::MessageStatus;

fn user(id: &str, text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: format!("{id}-p"),
            text: text.into(),
            agent_text: None,
        }],
        created_at: 0,
        device_id: "dev".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    }
}

fn assistant(id: &str, text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: format!("{id}-p"),
            text: text.into(),
            agent_text: None,
        }],
        created_at: 0,
        device_id: "dev".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    }
}

fn streaming_assistant(id: &str) -> SessionMessageEntry {
    let mut e = assistant(id, "live");
    e.status = Some(MessageStatus::Streaming);
    e
}

fn joined() -> Vec<SessionMessageEntry> {
    vec![
        user("u1", "first"),
        assistant("a1", "reply one"),
        user("u2", "second"),
        assistant("a2", "reply two"),
    ]
}

#[test]
fn user_anchor_forks_before_and_prefills() {
    let plan = compute_boundary(&joined(), "u2").unwrap();
    assert_eq!(plan.mode, SessionForkMode::EditUser);
    assert_eq!(plan.prefix_end, 2); // u1 + a1
    assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1));
    assert_eq!(plan.composer_text.as_deref(), Some("second"));
    let joined = joined();
    let copied = plan.to_copy(&joined, &joined);
    assert_eq!(
        copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["u1", "a1"]
    );
}

#[test]
fn assistant_with_later_user_includes_the_clicked_reply() {
    let plan = compute_boundary(&joined(), "a1").unwrap();
    assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
    assert_eq!(plan.prefix_end, 2); // u1 + a1
    assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1)); // next user = u2
    assert_eq!(plan.composer_text, None);
    let joined = joined();
    let copied = plan.to_copy(&joined, &joined);
    assert_eq!(
        copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["u1", "a1"]
    );
}

#[test]
fn last_assistant_clones_at_leaf() {
    let plan = compute_boundary(&joined(), "a2").unwrap();
    assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
    assert_eq!(plan.prefix_end, 4);
    assert_eq!(plan.pi_boundary, PiForkBoundary::CloneLeaf);
}

#[test]
fn mid_assistant_with_no_later_user_is_unavailable() {
    // u1, a1(clicked), a2 — no later user and a2 sits past the clicked
    // boundary: CloneLeaf would over-include, so it must be refused.
    let transcript = vec![
        user("u1", "first"),
        assistant("a1", "one"),
        assistant("a2", "two"),
    ];
    let err = compute_boundary(&transcript, "a1").unwrap_err();
    assert_eq!(err, SessionForkUnavailableReason::BoundaryUnavailable);
}

#[test]
fn assistant_intervened_by_assistant_before_user_is_unavailable() {
    // u1, a1(clicked), a2, u2 — the next USER is NOT the immediately next
    // joined entry (an assistant intervenes). `fork before u2` would copy
    // pi context through a2 while the Cypher prefix omits it: refuse.
    let transcript = vec![
        user("u1", "first"),
        assistant("a1", "one"),
        assistant("a2", "two"),
        user("u2", "second"),
    ];
    assert_eq!(
        compute_boundary(&transcript, "a1").unwrap_err(),
        SessionForkUnavailableReason::BoundaryUnavailable
    );
}

#[test]
fn assistant_intervened_by_system_before_user_is_unavailable() {
    // u1, a1(clicked), sys, u2 — a system entry sits between the clicked
    // assistant and the next user: same over-inclusion drift, refused.
    let mut sys = user("sys", "note");
    sys.role = MessageRole::System;
    let transcript = vec![
        user("u1", "first"),
        assistant("a1", "one"),
        sys,
        user("u2", "second"),
    ];
    assert_eq!(
        compute_boundary(&transcript, "a1").unwrap_err(),
        SessionForkUnavailableReason::BoundaryUnavailable
    );
}

#[test]
fn assistant_directly_followed_by_user_still_forks() {
    // The immediately-next-entry user case remains valid: u1, a1(clicked),
    // u2 — prefix through a1, fork before u2.
    let plan = compute_boundary(&joined(), "a1").unwrap();
    assert_eq!(plan.mode, SessionForkMode::ContinueAfterAssistant);
    assert_eq!(plan.prefix_end, 2);
    assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(1));
}

#[test]
fn streaming_and_missing_anchors_are_unavailable() {
    assert_eq!(
        compute_boundary(&joined(), "nope").unwrap_err(),
        SessionForkUnavailableReason::BoundaryUnavailable
    );
    let live = vec![user("u1", "first"), streaming_assistant("a1")];
    assert_eq!(
        compute_boundary(&live, "a1").unwrap_err(),
        SessionForkUnavailableReason::BoundaryUnavailable
    );
}

#[test]
fn continuation_entries_copied_with_their_roots() {
    let mut a1b = assistant("a1b", "continued");
    a1b.continuation_of = Some("a1".into());
    let raw = vec![
        user("u1", "first"),
        assistant("a1", "one"),
        a1b.clone(),
        user("u2", "second"),
    ];
    let joined = cypher_doc::join_continuation_entries(raw.clone());
    let plan = compute_boundary(&joined, "u2").unwrap(); // prefix u1+a1(+cont)
    let copied = plan.to_copy(&joined, &raw);
    assert_eq!(
        copied.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["u1", "a1", "a1b"]
    );
    assert_eq!(copied[2].continuation_of.as_deref(), Some("a1"));
}

#[test]
fn visible_text_strips_attachment_trailers() {
    let mut e = user("u1", "prompt");
    e.parts[0] = MessagePart::Text {
        id: "p".into(),
        text: "prompt\n\nAttached images (local files — open them to view):\n- /a.png".into(),
        agent_text: None,
    };
    assert_eq!(visible_text_of(&e), "prompt");
}

#[test]
fn canonical_cleanup_guard_refuses_escapes_and_accepts_managed_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("agent-sessions");
    std::fs::create_dir_all(&root).unwrap();
    let root_c = std::fs::canonicalize(&root).unwrap();

    // A non-existent file under the root resolves (the delete may run
    // before pi persists it).
    let inside = canonicalize_under_root(&root.join("fork-1.jsonl"), &root).unwrap();
    assert_eq!(inside, root_c.join("fork-1.jsonl"));
    // An existing file under the root resolves too.
    std::fs::write(root.join("fork-2.jsonl"), b"{}").unwrap();
    let inside = canonicalize_under_root(&root.join("fork-2.jsonl"), &root).unwrap();
    assert_eq!(inside, root_c.join("fork-2.jsonl"));
    // A path OUTSIDE the root (lexically and canonically) is refused.
    assert!(canonicalize_under_root(&dir.path().join("x.jsonl"), &root).is_none());
    assert!(canonicalize_under_root(&root.join("..").join("x.jsonl"), &root).is_none());

    // A symlinked ancestor escaping the root is refused even though the
    // lexical path starts with the root.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path(), root.join("escape")).unwrap();
        assert!(
            canonicalize_under_root(&root.join("escape").join("secret.jsonl"), &root).is_none()
        );
    }
}

#[test]
fn fork_title_appends_and_bounds() {
    let mut chat = Chat {
        pinned: false,
        id: "c".into(),
        device_id: "d".into(),
        title: Some("My chat".into()),
        archived: false,
        cwd: None,
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: None,
        last_message_at: None,
        created_at: chrono::Utc::now(),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
        child: None,
    };
    assert_eq!(fork_title(&chat), "My chat — Fork");
    chat.title = None;
    assert_eq!(fork_title(&chat), "New session — Fork");
    chat.title = Some("x".repeat(300));
    assert!(fork_title(&chat).chars().count() <= MAX_FORK_TITLE_CHARS);
}

#[test]
fn first_user_fork_has_empty_prefix() {
    let plan = compute_boundary(&joined(), "u1").unwrap();
    assert_eq!(plan.prefix_end, 0);
    assert_eq!(plan.pi_boundary, PiForkBoundary::BeforeUser(0));
    assert!(plan.to_copy(&joined(), &joined()).is_empty());
}

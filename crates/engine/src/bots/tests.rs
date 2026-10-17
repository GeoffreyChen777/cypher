//! Rendering and paging of the bot tools' answers. Scope enforcement runs
//! against a real engine in `tests/agent_bots.rs`.

use chrono::{DateTime, Utc};

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::{Bot, BotScope, Chat, ProjectGrant, Space, ToolCall};

use super::tools::{View, list_projects, list_sessions, list_sessions_args, render_session};

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap()
}

fn chat(id: &str, space: &str, last: i64) -> Chat {
    Chat {
        id: id.into(),
        device_id: "dev".into(),
        title: Some(format!("{id} title")),
        archived: false,
        pinned: false,
        cwd: None,
        branch: None,
        checkout_id: None,
        config: None,
        last_message_preview: Some("latest   words\nhere".into()),
        last_message_at: Some(at(last)),
        created_at: at(0),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: Some(space.into()),
        last_seen_at: None,
        room_gen: None,
        child: None,
        bot: None,
    }
}

fn space(id: &str, path: &str) -> Space {
    Space {
        id: id.into(),
        device_id: "dev".into(),
        path: path.into(),
        name: None,
        git_detected: true,
        git_checked_at: None,
        checkout_id: None,
        created_at: at(0),
        pinned: false,
        icon: None,
        color: None,
    }
}

fn view(scope: BotScope) -> View {
    View::build(
        Bot {
            id: "bot".into(),
            name: "Bot".into(),
            icon: None,
            color: None,
            host_device_id: "dev".into(),
            chat_id: "bot-chat".into(),
            instructions: None,
            scope,
            created_at: at(0),
        },
        vec![space("s1", "/code/billing"), space("s2", "/code/secret")],
        vec![
            chat("old", "s1", 1_000),
            chat("new", "s1", 5_000),
            chat("hidden", "s2", 9_000),
        ],
        vec![],
        vec![cypher_proto::Device {
            id: "dev".into(),
            name: "Studio".into(),
            platform: "macos".into(),
            last_seen_at: None,
            created_at: None,
            version: None,
        }],
        at(10_000),
    )
}

fn billing() -> BotScope {
    BotScope {
        projects: ProjectGrant::Selected {
            space_ids: vec!["s1".into()],
        },
        ..Default::default()
    }
}

fn message(index: usize, text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: format!("m{index}"),
        role: if index.is_multiple_of(2) {
            MessageRole::User
        } else {
            MessageRole::Assistant
        },
        parts: vec![
            MessagePart::Text {
                id: "t".into(),
                text: text.into(),
                agent_text: None,
            },
            MessagePart::Tool {
                id: "x".into(),
                call: ToolCall::Exec {
                    command: "cargo   test\n-p engine".into(),
                },
                is_error: true,
                resolved: true,
                output: None,
                progress: None,
                diff: None,
                output_ref: None,
                output_bytes: None,
                diff_ref: None,
                diff_stats: None,
                permission: None,
            },
            MessagePart::Reasoning {
                id: "r".into(),
                text: "private thoughts".into(),
            },
        ],
        created_at: 2_000,
        device_id: "dev".into(),
        status: None,
        continuation_of: None,
        completed_at: None,
        comments: vec![],
        models: vec![],
        turn_changes: None,
        recovery: None,
    }
}

#[test]
fn listings_show_the_scope_newest_first() {
    let view = view(billing());
    let projects = list_projects(&view);
    assert!(projects.contains("billing (id s1) on Studio"), "{projects}");
    assert!(!projects.contains("secret"), "{projects}");

    let sessions = list_sessions(&view, list_sessions_args(serde_json::json!({}))).unwrap();
    let new_at = sessions.find("- new").unwrap();
    let old_at = sessions.find("- old").unwrap();
    assert!(new_at < old_at, "newest first: {sessions}");
    assert!(!sessions.contains("hidden"));
    assert!(
        sessions.contains("latest: latest words here"),
        "previews fold to one line"
    );

    // 0.002 h = 7.2 s: "new" moved 5 s ago, "old" 9 s ago.
    let recent = list_sessions(
        &view,
        list_sessions_args(serde_json::json!({"since_hours": 0.002})),
    )
    .unwrap();
    assert!(
        recent.contains("- new") && !recent.contains("- old"),
        "{recent}"
    );
    let by_name = list_sessions(
        &view,
        list_sessions_args(serde_json::json!({"project": "BILLING"})),
    )
    .unwrap();
    assert!(by_name.contains("- new"));
    assert!(
        list_sessions(
            &view,
            list_sessions_args(serde_json::json!({"project": "secret"}))
        )
        .is_err()
    );
}

#[test]
fn reading_pages_from_the_tail_and_summarizes_tools() {
    let view = view(billing());
    let chat = view.chats.iter().find(|c| c.id == "new").unwrap().clone();
    let entries: Vec<SessionMessageEntry> = (0..30)
        .map(|i| message(i, &format!("message {i}")))
        .collect();

    let tail = render_session(&view, &chat, &entries, None, Some(5));
    assert!(tail.contains("showing #25–#29"), "{tail}");
    assert!(tail.contains("Earlier messages: read_session with from=20."));
    assert!(tail.contains("[#29] agent"));
    assert!(
        tail.contains("↳ ran `cargo test -p engine` (failed)"),
        "{tail}"
    );
    assert!(!tail.contains("private thoughts"), "reasoning stays out");
    assert!(!tail.contains("[#24]"));

    let head = render_session(&view, &chat, &entries, Some(0), Some(3));
    assert!(head.contains("[#0] user") && head.contains("[#2]"));
    assert!(head.contains("More messages: read_session with from=3."));

    let empty = render_session(&view, &chat, &[], None, None);
    assert!(empty.contains("0 message(s)"), "{empty}");
}

#[test]
fn a_long_message_is_clipped() {
    let view = view(billing());
    let chat = view.chats[0].clone();
    let long = "x".repeat(10_000);
    let page = render_session(&view, &chat, &[message(0, &long)], None, None);
    assert!(page.contains("more characters]"), "clipped");
    assert!(page.chars().count() < 8_000);
}

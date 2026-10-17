//! Rendering and paging of the bot tools' answers. Scope enforcement runs
//! against a real engine in `tests/agent_bots.rs`.

use chrono::{DateTime, Utc};

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::{
    Bot, BotScope, Chat, DiffFileSummary, GitHistoryCommit, GitHistoryPage, GitHistoryRef,
    GitHistoryRefKind, ProjectGrant, Session, SessionStatus, Space, ToolCall,
};

use super::git::{
    Commit, DiffStats, GitFacts, Upstream, facts_from_history, render, resolve_space,
};
use super::tools::{
    View, list_projects, list_sessions, list_sessions_args, render_session, session_activity,
    session_activity_args,
};

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
    view_with(scope, vec![])
}

fn view_with(scope: BotScope, sessions: Vec<Session>) -> View {
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
        sessions,
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

fn session(chat: &str, status: SessionStatus, updated: i64) -> Session {
    Session {
        chat_id: chat.into(),
        device_id: "dev".into(),
        status,
        started_at: None,
        updated_at: at(updated),
        subagents: vec![],
        context_usage: None,
        throughput: None,
        recovery: None,
    }
}

#[test]
fn activity_groups_the_window_by_project() {
    let view = view_with(
        billing(),
        vec![
            session("new", SessionStatus::Idle, 6_000),
            session("hidden", SessionStatus::Working, 9_500),
        ],
    );
    let day = session_activity(&view, session_activity_args(serde_json::json!({}))).unwrap();
    assert!(day.contains("Activity in the last 24 hours"), "{day}");
    assert!(day.contains("billing (id s1) on Studio:"), "{day}");
    let new_at = day.find("- new").unwrap();
    assert!(new_at < day.find("- old").unwrap(), "newest first: {day}");
    assert!(day.contains("new, started 1970-01-01 00:00 UTC"), "{day}");
    assert!(day.contains("latest: latest words here"), "{day}");
    assert!(day.contains("run finished 1970-01-01 00:00 UTC"), "{day}");
    assert!(!day.contains("hidden"), "{day}");

    // 0.002 h = 7.2 s: only "new" moved, and nothing was created.
    let recent = session_activity(
        &view,
        session_activity_args(serde_json::json!({"since_hours": 0.002})),
    )
    .unwrap();
    assert!(
        recent.contains("- new") && !recent.contains("- old"),
        "{recent}"
    );
    assert!(!recent.contains("new, started"), "{recent}");

    // A working session counts even without new messages.
    let working = view_with(
        billing(),
        vec![session("old", SessionStatus::Working, 9_900)],
    );
    let now = session_activity(
        &working,
        session_activity_args(serde_json::json!({"since_hours": 0.001})),
    )
    .unwrap();
    assert!(
        now.contains("- old") && now.contains("· working ·"),
        "{now}"
    );

    let empty = view_with(billing(), vec![]);
    let quiet = session_activity(
        &empty,
        session_activity_args(serde_json::json!({"since_hours": 0.0001})),
    )
    .unwrap();
    assert!(quiet.starts_with("Nothing changed"), "{quiet}");

    // Out of scope reads exactly like unknown.
    let hidden = session_activity(
        &view,
        session_activity_args(serde_json::json!({"project": "secret"})),
    )
    .unwrap_err()
    .to_string();
    let unknown = session_activity(
        &view,
        session_activity_args(serde_json::json!({"project": "nope"})),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(hidden.replace("secret", "X"), unknown.replace("nope", "X"));
}

#[test]
fn git_state_reads_as_one_answer() {
    let view = view(billing());
    let space = resolve_space(&view, "billing").unwrap().clone();
    let facts = GitFacts {
        branch: Some("main".into()),
        head: Some("abc1234".into()),
        upstream: Some(Upstream {
            name: "origin/main".into(),
            ahead: 2,
            behind: 1,
            estimated: false,
        }),
        commits: vec![Commit {
            sha: "abc1234".into(),
            subject: "Fix the invoice totals".into(),
            author: "Ada".into(),
            at: Some(at(4_000)),
        }],
        diff: Ok(DiffStats {
            files: vec![
                DiffFileSummary {
                    path: "src/a.rs".into(),
                    old_path: None,
                    status: "modified".into(),
                    additions: 10,
                    deletions: 2,
                    binary: false,
                },
                DiffFileSummary {
                    path: "notes.md".into(),
                    old_path: None,
                    status: "added".into(),
                    additions: 3,
                    deletions: 0,
                    binary: false,
                },
            ],
            additions: 13,
            deletions: 2,
            truncated: false,
        }),
    };
    let text = render(&view, &space, &facts);
    assert!(
        text.starts_with("Git state of billing (id s1) on Studio · /code/billing"),
        "{text}"
    );
    assert!(
        text.contains("Branch: main · 2 ahead, 1 behind origin/main"),
        "{text}"
    );
    assert!(
        text.contains("Working tree: 2 file(s) changed, +13 −2 (1 new):"),
        "{text}"
    );
    assert!(text.contains("  modified src/a.rs +10 −2"), "{text}");
    assert!(text.contains("Recent commits on main:"), "{text}");
    assert!(
        text.contains("- abc1234 Fix the invoice totals — Ada, 1970-01-01 00:00 UTC"),
        "{text}"
    );

    let fresh = GitFacts {
        branch: Some("main".into()),
        head: None,
        upstream: None,
        commits: vec![],
        diff: Ok(DiffStats {
            files: vec![],
            additions: 0,
            deletions: 0,
            truncated: false,
        }),
    };
    let text = render(&view, &space, &fresh);
    assert!(text.contains("Branch: main · no commits yet"), "{text}");
    assert!(text.contains("Working tree: clean."), "{text}");

    // Out of scope reads exactly like unknown; a missing name is asked for.
    let hidden = resolve_space(&view, "secret").unwrap_err().to_string();
    let unknown = resolve_space(&view, "nope").unwrap_err().to_string();
    assert_eq!(hidden.replace("secret", "X"), unknown.replace("nope", "X"));
    assert!(resolve_space(&view, " ").is_err());
}

#[test]
fn a_remote_history_page_estimates_ahead_and_behind() {
    let commit = |sha: &str, parents: &[&str], refs: Vec<GitHistoryRef>| GitHistoryCommit {
        sha: format!("{sha}000000000"),
        parent_shas: parents.iter().map(|p| format!("{p}000000000")).collect(),
        subject: format!("commit {sha}"),
        author_name: "Ada".into(),
        author_email: "ada@example.com".into(),
        authored_at: "2026-10-14T09:12:00+02:00".into(),
        refs,
    };
    let remote = |label: &str| GitHistoryRef {
        kind: GitHistoryRefKind::Remote,
        label: label.into(),
    };
    // main: d → c → a; origin/main: b → a; an unrelated branch x.
    let page = GitHistoryPage {
        commits: vec![
            commit("x", &[], vec![]),
            commit("d", &["c"], vec![]),
            commit("c", &["a"], vec![]),
            commit(
                "b",
                &["a"],
                vec![remote("fork/main"), remote("origin/main")],
            ),
            commit("a", &[], vec![]),
        ],
        head_sha: Some("d000000000".into()),
        next_cursor: None,
        total_count: Some(5),
        head_commit_count: Some(3),
    };
    let facts = facts_from_history(Some("main".into()), &page, Err("offline diff".into()));
    let up = facts.upstream.as_ref().unwrap();
    assert_eq!(
        (up.name.as_str(), up.ahead, up.behind),
        ("origin/main", 2, 1)
    );
    assert!(up.estimated);
    let shas: Vec<&str> = facts.commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(
        shas,
        ["d000000", "c000000", "a000000"],
        "HEAD's own commits"
    );
    assert_eq!(
        facts.commits[0].at.unwrap().to_rfc3339(),
        "2026-10-14T07:12:00+00:00",
        "author times read as UTC"
    );
    let text = render(
        &view(billing()),
        &resolve_space(&view(billing()), "s1").unwrap().clone(),
        &facts,
    );
    assert!(
        text.contains("2 ahead, 1 behind origin/main (estimated"),
        "{text}"
    );
    assert!(
        text.contains("Working tree: couldn't be read (offline diff)."),
        "{text}"
    );
}

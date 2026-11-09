//! Rendering and paging of the bot tools' answers. Scope enforcement runs
//! against a real engine in `tests/agent_bots.rs`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_proto::{
    Bot, BotScope, Chat, DiffFileSummary, GitHistoryCommit, GitHistoryPage, GitHistoryRef,
    GitHistoryRefKind, ProjectGrant, Session, SessionStatus, Space, ToolCall,
};

use super::git::{
    Commit, DiffStats, GitFacts, Upstream, facts_from_history, render, resolve_space,
};
use super::index::{Hit, Lookup, Pass, SearchIndex, match_query, targets};
use super::tools::{
    SEARCH_SCAN_MAX, View, find, list_projects, list_sessions, list_sessions_args, render_session,
    session_activity, session_activity_args,
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
        memory: None,
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
            capabilities: Default::default(),
            paused: false,
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
        progress: None,
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

// ── the search index ────────────────────────────────────────────────────────

fn transcript(texts: &[&str]) -> Vec<SessionMessageEntry> {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| message(i, text))
        .collect()
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

fn chats_hit(lookup: &Lookup) -> Vec<String> {
    let mut chats: Vec<String> = lookup.hits.iter().map(|h| h.chat_id.clone()).collect();
    chats.dedup();
    chats
}

#[test]
fn the_index_finds_every_word_as_a_substring_within_the_asked_scope() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::new(dir.path().join("bots-index.sqlite"));
    let chats = [chat("a", "s1", 2_000), chat("b", "s1", 1_000)];
    let texts: HashMap<&str, Vec<SessionMessageEntry>> = HashMap::from([
        ("a", transcript(&["Migrate the invoice tables", "done"])),
        ("b", transcript(&["Rotate the invoice signing key"])),
    ]);
    let pass = index
        .sync(&chats, &HashMap::new(), |id| Ok(texts.get(id).cloned()), 10)
        .unwrap();
    assert_eq!(
        pass,
        Pass {
            indexed: 2,
            removed: 0,
            remaining: 0
        }
    );

    let both = index.search(&words("INVO"), &ids(&["a", "b"])).unwrap();
    assert_eq!(both.hits.len(), 2, "substring, any case: {both:?}");
    assert_eq!(
        both.indexed,
        HashMap::from([("a".into(), true), ("b".into(), true)])
    );
    let hit = both.hits.iter().find(|h| h.chat_id == "a").unwrap();
    assert_eq!(
        (hit.index, hit.role.as_str(), hit.text.as_str()),
        (0, "user", "Migrate the invoice tables")
    );

    let all_words = index
        .search(&words("invoice tab"), &ids(&["a", "b"]))
        .unwrap();
    assert_eq!(chats_hit(&all_words), ["a"]);
    // Scope is the caller's: b's match is invisible to a query over a only.
    let scoped = index.search(&words("signing"), &ids(&["a"])).unwrap();
    assert!(scoped.hits.is_empty() && !scoped.indexed.contains_key("b"));
    // Substrings match, as they did in the scan: inside identifiers too.
    let inside = index.search(&words("igning"), &ids(&["a", "b"])).unwrap();
    assert_eq!(chats_hit(&inside), ["b"]);
    // Words too short for trigrams filter instead.
    let short = index
        .search(&words("ke invoice"), &ids(&["a", "b"]))
        .unwrap();
    assert_eq!(chats_hit(&short), ["b"]);
    let only_short = index.search(&words("bl"), &ids(&["a", "b"])).unwrap();
    assert_eq!(chats_hit(&only_short), ["a"]);
    // Quotes, LIKE wildcards and bare punctuation can't break the query.
    assert_eq!(
        match_query(&words("- \"key a_ _%")),
        Some(super::index::MatchQuery {
            fts: Some("\"\"\"key\"".into()),
            short: vec!["%a\\_%".into()],
        })
    );
    assert!(
        index
            .search(&words("%"), &ids(&["b"]))
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(index.search(&words("\"key"), &ids(&["b"])).is_ok());
    assert!(
        index
            .search(&words("--"), &ids(&["b"]))
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn the_index_rereads_only_moved_chats_and_drops_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::new(dir.path().join("bots-index.sqlite"));
    let mut chats = vec![chat("a", "s1", 2_000), chat("b", "s1", 1_000)];
    let mut texts: HashMap<String, Vec<SessionMessageEntry>> = HashMap::from([
        ("a".to_string(), transcript(&["first draft"])),
        ("b".to_string(), transcript(&["release notes"])),
    ]);
    let mut saved = HashMap::from([("b".to_string(), 50)]);
    let reads = std::cell::RefCell::new(Vec::<String>::new());
    let sync = |chats: &[Chat],
                saved: &HashMap<String, i64>,
                texts: &HashMap<String, Vec<SessionMessageEntry>>,
                budget| {
        reads.borrow_mut().clear();
        index
            .sync(
                chats,
                saved,
                |id| {
                    reads.borrow_mut().push(id.to_string());
                    Ok(texts.get(id).cloned())
                },
                budget,
            )
            .unwrap()
    };
    // A bounded pass leaves the rest for the next one.
    assert_eq!(sync(&chats, &saved, &texts, 1).remaining, 1);
    assert_eq!(*reads.borrow(), ["a"], "newest first");
    assert_eq!(sync(&chats, &saved, &texts, 10).indexed, 1);
    assert_eq!(sync(&chats, &saved, &texts, 10).indexed, 0, "nothing moved");

    // New activity re-reads that chat; its old text is replaced.
    texts.insert("a".into(), transcript(&["final copy"]));
    chats[0].last_message_at = Some(at(3_000));
    sync(&chats, &saved, &texts, 10);
    assert_eq!(*reads.borrow(), ["a"]);
    assert!(
        index
            .search(&words("draft"), &ids(&["a"]))
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        chats_hit(&index.search(&words("final"), &ids(&["a"])).unwrap()),
        ["a"]
    );

    // A fresher stored snapshot re-reads the chat even with no row change.
    saved.insert("b".into(), 60);
    sync(&chats, &saved, &texts, 10);
    assert_eq!(*reads.borrow(), ["b"]);

    // A chat that left every scope (or was deleted) is dropped.
    let pass = sync(&chats[..1], &saved, &texts, 10);
    assert_eq!(pass.removed, 1);
    let gone = index.search(&words("release"), &ids(&["a", "b"])).unwrap();
    assert!(gone.hits.is_empty() && !gone.indexed.contains_key("b"));

    // No local copy: indexed as such, matched by title only.
    texts.remove("a");
    chats[0].last_message_at = Some(at(4_000));
    sync(&chats[..1], &saved, &texts, 10);
    assert!(!index.search(&words("final"), &ids(&["a"])).unwrap().indexed["a"]);
}

#[test]
fn index_targets_are_the_union_of_this_devices_reading_bots() {
    let bot = |id: &str, host: &str, scope: BotScope| Bot {
        id: id.into(),
        name: id.into(),
        icon: None,
        color: None,
        host_device_id: host.into(),
        chat_id: format!("{id}-chat"),
        instructions: None,
        scope,
        capabilities: Default::default(),
        paused: false,
        created_at: at(0),
    };
    let grant = |space: &str| BotScope {
        projects: ProjectGrant::Selected {
            space_ids: vec![space.into()],
        },
        ..Default::default()
    };
    let mut bot_chat = chat("b1-chat", "s1", 9_000);
    bot_chat.bot = Some(cypher_proto::BotChat::owner("b1"));
    let chats = vec![
        chat("one", "s1", 1_000),
        chat("two", "s2", 2_000),
        chat("three", "s3", 3_000),
        chat("four", "s4", 4_000),
        bot_chat,
    ];
    let bots = vec![
        bot("b1", "dev", grant("s1")),
        bot("b2", "dev", grant("s2")),
        bot("b3", "other", grant("s3")),
        bot(
            "b4",
            "dev",
            BotScope {
                transcripts: false,
                ..grant("s4")
            },
        ),
    ];
    let ids: Vec<String> = targets("dev", &bots, &chats)
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(
        ids,
        ["two", "one"],
        "newest first; no bot chats, other hosts or title-only bots"
    );
}

#[test]
fn a_corrupt_index_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bots-index.sqlite");
    std::fs::write(
        &path,
        b"this is not a database at all, not even close......",
    )
    .unwrap();
    let index = SearchIndex::new(path);
    let chats = [chat("a", "s1", 1_000)];
    index
        .sync(
            &chats,
            &HashMap::new(),
            |_| Ok(Some(transcript(&["hello world"]))),
            10,
        )
        .unwrap();
    assert_eq!(
        chats_hit(&index.search(&words("hello"), &ids(&["a"])).unwrap()),
        ["a"]
    );
}

#[test]
fn search_falls_back_to_scanning_unindexed_chats() {
    let candidates = vec![
        chat("indexed", "s1", 5_000),
        chat("fresh", "s1", 4_000),
        chat("uncopied", "s1", 3_000),
        chat("titled", "s1", 2_000),
    ];
    let mut candidates = candidates;
    candidates[3].title = Some("Invoice cleanup".into());
    let lookup = |_: &[String], _: &[String]| {
        Ok(Lookup {
            hits: vec![Hit {
                chat_id: "indexed".into(),
                index: 4,
                role: "agent".into(),
                text: "the invoice job is green".into(),
                score: -2.0,
            }],
            indexed: HashMap::from([("indexed".into(), true), ("uncopied".into(), false)]),
        })
    };
    let read = |id: &str| {
        Ok(match id {
            "fresh" => Some(transcript(&["no match", "invoice export broke"])),
            "titled" => Some(transcript(&["unrelated"])),
            other => panic!("{other} is indexed and must not be scanned"),
        })
    };
    let found = find(&candidates, &words("invoice"), true, 20, lookup, read);
    let order: Vec<&str> = found.hits.iter().map(|(c, _)| c.id.as_str()).collect();
    assert_eq!(
        order,
        ["indexed", "fresh", "titled"],
        "ranked, then scanned, then title-only"
    );
    assert_eq!(found.hits[0].1, ["#4 agent: the invoice job is green"]);
    assert_eq!(found.hits[1].1, ["#1 agent: invoice export broke"]);
    assert_eq!((found.unstored, found.not_searched), (1, 0));

    // With no index every chat is scanned, up to the bound.
    let many: Vec<Chat> = (0..SEARCH_SCAN_MAX + 2)
        .map(|i| chat(&format!("c{i}"), "s1", 10_000 - i as i64))
        .collect();
    let found = find(
        &many,
        &words("invoice"),
        true,
        20,
        |_, _| Err(crate::EngineError::Other("no index".into())),
        |_| Ok(None),
    );
    assert_eq!((found.unstored, found.not_searched), (SEARCH_SCAN_MAX, 2));
    assert!(found.hits.is_empty());
}

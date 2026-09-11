use super::*;
use cypher_proto::{AgentEvent, ToolCall};

fn text(value: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: value.into() }
}

#[test]
fn source_append_is_sparse_and_exact_overlap_is_idempotent() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let changes = j.db.total_changes();
    assert_eq!(
        j.append_execution_events(&permit, 1, &[text("a"), text("b")])
            .unwrap(),
        2
    );
    assert_eq!(j.db.total_changes() - changes, 2);
    let changes = j.db.total_changes();
    j.append_execution_events(&permit, 1, &[text("a"), text("b")])
        .unwrap();
    assert_eq!(j.db.total_changes(), changes);
    assert_eq!(
        j.append_execution_events(&permit, 2, &[text("b"), text("c")])
            .unwrap(),
        3
    );
    assert_eq!(j.db.total_changes() - changes, 1);
    assert!(
        j.pending().unwrap().is_empty(),
        "raw events never enter the wire outbox"
    );
    let page = j.execution_events("command", 0, 2).unwrap();
    assert_eq!((page.through, page.next, page.events.len()), (3, 2, 2));
    assert!(!page.events[0].after_completion);
    assert!(matches!(&page.events[0].event, AgentEvent::TextDelta { text } if text == "a"));
    let last = j.execution_events("command", page.next, 2).unwrap();
    assert_eq!((last.through, last.next, last.events.len()), (3, 3, 1));
    assert!(
        j.execution_events("command", 3, 2)
            .unwrap()
            .events
            .is_empty()
    );
}

#[test]
fn source_batch_failure_is_atomic_and_does_not_advance_ordinal() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    j.db.execute_batch(
        "CREATE TEMP TRIGGER fail_source BEFORE INSERT ON sync3_execution_source_events
        WHEN NEW.seq=2 BEGIN SELECT RAISE(ABORT,'test failure'); END;",
    )
    .unwrap();
    assert!(
        j.append_execution_events(&permit, 1, &[text("a"), text("b")])
            .is_err()
    );
    assert_eq!(j.execution_events("command", 0, 32).unwrap().through, 0);
    j.db.execute_batch("DROP TRIGGER fail_source").unwrap();
    j.append_execution_events(&permit, 1, &[text("a"), text("b")])
        .unwrap();
    assert_eq!(j.execution_events("command", 0, 32).unwrap().through, 2);
}

#[test]
fn source_rejects_gaps_changed_retries_and_ahead_cursors_without_reseeding() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    assert!(j.append_execution_events(&permit, 0, &[text("a")]).is_err());
    assert!(j.append_execution_events(&permit, 2, &[text("a")]).is_err());
    j.append_execution_events(&permit, 1, &[text("a"), text("b"), text("c")])
        .unwrap();
    assert!(
        j.append_execution_events(&permit, 1, &[text("changed")])
            .is_err()
    );
    assert!(j.execution_events("command", 4, 1).is_err());
    assert!(j.execution_events("command", 0, 0).is_err());
    assert!(j.execution_events("command", 0, 33).is_err());
    j.db.execute("DELETE FROM sync3_execution_source_events WHERE seq=2", [])
        .unwrap();
    assert!(j.execution_events("command", 0, 32).is_err());
    assert!(j.append_execution_events(&permit, 2, &[text("b")]).is_err());
    assert_eq!(
        j.db.query_row(
            "SELECT COUNT(*) FROM sync3_execution_source_events",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn late_observations_survive_scope_fence_loss_without_publishing() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 2,
        head: 3,
    })
    .unwrap();
    j.append_execution_events(&permit, 1, &[text("late retained output")])
        .unwrap();
    assert!(
        j.complete_execution(
            &permit,
            Some(Outcome::Completed),
            SessionCommandStatus::Applied,
            None
        )
        .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    assert_eq!(j.execution_events("command", 0, 32).unwrap().through, 1);
    let mut foreign = Journal::open(Path::new(":memory:"), "other", "room", "host").unwrap();
    assert!(
        foreign
            .append_execution_events(&permit, 1, &[text("private")])
            .is_err()
    );
    assert!(foreign.execution_events("command", 0, 32).is_err());
    assert_eq!(
        foreign
            .db
            .query_row(
                "SELECT COUNT(*) FROM sync3_execution_source_events",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn completion_marks_only_new_observations_late_and_restart_grants_no_permit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.sqlite");
    let mut j = host(&path);
    let permit = claim_run(&mut j);
    j.append_execution_events(&permit, 1, &[text("original")])
        .unwrap();
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    let pending = j.pending().unwrap();
    j.append_execution_events(&permit, 1, &[text("original"), text("late")])
        .unwrap();
    assert_eq!(j.pending().unwrap(), pending);
    drop(j);
    drop(permit);
    let mut j = Journal::open(&path, "account", "room", "host").unwrap();
    let page = j.execution_events("command", 0, 32).unwrap();
    assert_eq!(page.events.len(), 2);
    assert!(!page.events[0].after_completion);
    assert!(page.events[1].after_completion);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Settled
    ));
}

#[test]
fn oversized_private_event_is_retained_intact_and_paged_alone() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let content = "私密🙂".repeat(200_000);
    let event = AgentEvent::ToolCall {
        id: "tool".into(),
        call: ToolCall::WriteFile {
            path: "private.txt".into(),
            content: Some(content.clone()),
        },
    };
    assert!(
        j.append_execution_events(&permit, 1, &[event.clone(), text("tail")])
            .is_err()
    );
    assert_eq!(j.execution_events("command", 0, 32).unwrap().through, 0);
    j.append_execution_events(&permit, 1, &[event]).unwrap();
    j.append_execution_events(&permit, 2, &[text("tail")])
        .unwrap();
    let first = j.execution_events("command", 0, 32).unwrap();
    assert_eq!((first.next, first.events.len()), (1, 1));
    assert!(matches!(&first.events[0].event, AgentEvent::ToolCall {
        call: ToolCall::WriteFile { content: Some(stored), .. }, ..
    } if stored == &content));
    let second = j.execution_events("command", first.next, 32).unwrap();
    assert_eq!((second.next, second.events.len()), (2, 1));
    assert!(j.pending().unwrap().is_empty());
}

#[test]
fn source_corruption_or_unknown_fields_fail_without_silently_dropping_data() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    j.append_execution_events(&permit, 1, &[text("original")])
        .unwrap();
    j.db.execute("UPDATE sync3_execution_source_events SET event='{}'", [])
        .unwrap();
    assert!(j.execution_events("command", 0, 1).is_err());
    let mut event = serde_json::to_value(text("original")).unwrap();
    event["futureField"] = serde_json::json!("must not be lost");
    let body = serde_json::to_string(&event).unwrap();
    let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
    j.db.execute(
        "UPDATE sync3_execution_source_events SET event=?,digest=?",
        params![body, digest.as_slice()],
    )
    .unwrap();
    assert!(j.execution_events("command", 0, 1).is_err());
    assert_eq!(
        j.db.query_row("SELECT event FROM sync3_execution_source_events", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        body
    );
}

fn entry() -> cypher_proto::SessionMessageEntry {
    cypher_proto::SessionMessageEntry {
        id: "reply".into(),
        role: cypher_proto::MessageRole::Assistant,
        device_id: "host".into(),
        created_at: 1,
        parts: vec![],
        status: Some(cypher_proto::MessageStatus::Streaming),
        continuation_of: None,
    }
}

#[test]
fn execution_publication_requires_durable_source_and_exact_run_context() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let parts = [cypher_proto::MessagePart::Text {
        id: "text".into(),
        text: "retained".into(),
    }];
    let mut writer = j.new_execution_writer(&permit, &entry()).unwrap();
    assert!(
        writer
            .sync(&parts, |frame| j.enqueue_execution_frame(&permit, 1, frame))
            .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    assert!(j.load_writer("reply").unwrap().is_none());
    j.append_execution_events(&permit, 1, &[text("retained")])
        .unwrap();
    let mut wrong = j.new_writer(1, Some("wrong-run".into()), &entry()).unwrap();
    assert!(
        wrong
            .sync(&parts, |frame| j.enqueue_execution_frame(&permit, 1, frame))
            .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    writer
        .sync(&parts, |frame| j.enqueue_execution_frame(&permit, 1, frame))
        .unwrap();
    assert!(!j.pending().unwrap().is_empty());
    assert!(j.load_writer("reply").unwrap().is_some());
}

#[test]
fn publication_fence_loss_preserves_raw_results_without_advancing_producer() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let mut writer = j.new_execution_writer(&permit, &entry()).unwrap();
    let parts = [cypher_proto::MessagePart::Text {
        id: "text".into(),
        text: "late".into(),
    }];
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 2,
        head: 3,
    })
    .unwrap();
    j.append_execution_events(&permit, 1, &[text("late")])
        .unwrap();
    assert!(j.new_execution_writer(&permit, &entry()).is_err());
    assert!(
        writer
            .sync(&parts, |frame| j.enqueue_execution_frame(&permit, 1, frame))
            .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    assert!(j.load_writer("reply").unwrap().is_none());
    assert_eq!(j.execution_events("command", 0, 1).unwrap().events.len(), 1);
}

#[test]
fn settled_execution_cannot_publish_again_even_with_a_new_raw_observation() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let mut writer = j.new_execution_writer(&permit, &entry()).unwrap();
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    let before = j.pending().unwrap();
    j.append_execution_events(&permit, 1, &[text("late")])
        .unwrap();
    let parts = [cypher_proto::MessagePart::Text {
        id: "text".into(),
        text: "late".into(),
    }];
    assert!(
        writer
            .sync(&parts, |frame| j.enqueue_execution_frame(&permit, 1, frame))
            .is_err()
    );
    assert!(j.new_execution_writer(&permit, &entry()).is_err());
    assert_eq!(j.pending().unwrap(), before);
    assert!(j.execution_events("command", 0, 1).unwrap().events[0].after_completion);
}

#[cfg(unix)]
#[test]
fn source_crash_helper() {
    let Some(dir) = std::env::var_os("CYPHER_SYNC3_SOURCE_CRASH_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let mut j = host(&dir.join("journal.sqlite"));
    let permit = claim_run(&mut j);
    std::fs::write(dir.join("effect"), b"executed once").unwrap();
    j.append_execution_events(
        &permit,
        1,
        &[
            AgentEvent::ToolCall {
                id: "tool".into(),
                call: ToolCall::WriteFile {
                    path: "effect".into(),
                    content: Some("executed once".into()),
                },
            },
            AgentEvent::ToolResult {
                id: "tool".into(),
                is_error: false,
                output: Some("retained result".into()),
                diff: None,
            },
        ],
    )
    .unwrap();
    std::fs::write(dir.join("ready"), b"committed").unwrap();
    loop {
        std::thread::park();
    } // parent kills without dropping SQLite
}

#[cfg(unix)]
#[test]
fn sigkill_after_source_commit_retains_wal_and_never_reissues_dispatch() {
    use std::process::{Child, Command, Stdio};
    struct KillOnDrop(Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sync3::execution::tests::source::source_crash_helper",
                "--nocapture",
            ])
            .env("CYPHER_SYNC3_SOURCE_CRASH_DIR", dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dir.path().join("ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before commit"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "child did not reach commit boundary"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.0.kill().unwrap();
    let status = child.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
    let mut j = Journal::open(
        &dir.path().join("journal.sqlite"),
        "account",
        "room",
        "host",
    )
    .unwrap();
    let page = j.execution_events("command", 0, 32).unwrap();
    assert_eq!((page.through, page.events.len()), (2, 2));
    assert!(
        matches!(&page.events[1].event, AgentEvent::ToolResult { output: Some(output), .. } if output == "retained result")
    );
    for _ in 0..2 {
        assert!(matches!(
            j.advance_execution("command").unwrap(),
            Progress::RecoveryRequired
        ));
    }
    assert!(j.pending().unwrap().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("effect")).unwrap(),
        b"executed once"
    );
}

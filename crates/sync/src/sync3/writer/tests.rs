use super::*;
use crate::sync3::Journal;
use cypher_proto::{MessageRole, ToolCall, UserInputQuestion};
use rusqlite::params;
use std::path::Path;

fn entry() -> SessionMessageEntry {
    SessionMessageEntry {
        id: "root".into(),
        role: MessageRole::Assistant,
        parts: vec![],
        created_at: 1000,
        device_id: "host".into(),
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    }
}
fn writer() -> TranscriptWriter {
    TranscriptWriter::new("host".into(), 1, None, &entry()).unwrap()
}
fn text(id: &str, value: &str) -> MessagePart {
    MessagePart::Text {
        id: id.into(),
        text: value.into(),
    }
}
fn tool() -> MessagePart {
    MessagePart::Tool {
        id: "tool".into(),
        call: ToolCall::WriteFile {
            path: "file".into(),
            content: Some("private-secret".repeat(100_000)),
        },
        is_error: false,
        resolved: false,
        output: None,
        progress: Some("working".into()),
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
    }
}
fn input(id: &str) -> MessagePart {
    MessagePart::Input {
        id: id.into(),
        request_id: format!("request-{id}"),
        questions: vec![UserInputQuestion {
            id: "question one".into(),
            header: "Select".into(),
            question: "Continue?".into(),
            options: vec!["Yes".into(), "No".into()],
            multi_select: false,
        }],
        resolved: false,
    }
}
fn sync_all(w: &mut TranscriptWriter, j: &mut Journal, parts: &[MessagePart]) {
    for _ in 0..10_000 {
        if !w
            .sync(parts, |frame| j.enqueue_writer_frame(frame))
            .unwrap()
            .more
        {
            return;
        }
    }
    panic!("producer failed to make progress");
}
fn finish_all(w: &mut TranscriptWriter, j: &mut Journal, parts: &[MessagePart]) {
    for _ in 0..10_000 {
        if !w
            .finish(parts, Some(MessageStatus::Complete), |frame| {
                j.enqueue_writer_frame(frame)
            })
            .unwrap()
            .more
        {
            return;
        }
    }
    panic!("finalization failed to make progress");
}
fn apply_outbox(j: &mut Journal) {
    let count: u64 =
        j.db.query_row("SELECT count(*) FROM sync3_outbox", [], |r| r.get(0))
            .unwrap();
    let head = j.cursor().unwrap() + count;
    j.accept_state(&wire::Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 1,
        head,
    })
    .unwrap();
    loop {
        let pending = j.pending().unwrap();
        if pending.is_empty() {
            break;
        }
        let base = j.cursor().unwrap();
        let next = base + pending.len() as u64;
        let rows = pending
            .into_iter()
            .enumerate()
            .map(|(i, operation)| wire::Row {
                seq: base + i as u64 + 1,
                operation,
            })
            .collect();
        j.apply_page(&wire::Reply::Page {
            version: 3,
            epoch: 1,
            through: head,
            next,
            done: next == head,
            rows,
        })
        .unwrap();
    }
}
fn ordered_parts(j: &Journal) -> Vec<MessagePart> {
    let mut messages = j
        .projection()
        .unwrap()
        .messages
        .into_values()
        .collect::<Vec<_>>();
    messages.sort_by_key(|m| m.created_seq);
    messages.into_iter().flat_map(|m| m.entry.parts).collect()
}
fn joined_text(parts: &[MessagePart]) -> String {
    parts
        .iter()
        .filter_map(|p| {
            if let MessagePart::Text { text, .. } = p {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn escaped_unicode_rolls_over_and_old_tool_and_input_can_resolve() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    let body = "你好🙂\n\"\\\0".repeat(100_000);
    let mut parts = vec![
        text("before", "before"),
        tool(),
        input("input"),
        text("long", &body),
    ];
    sync_all(&mut w, &mut j, &parts);
    let raw: String =
        j.db.query_row(
            "SELECT group_concat(operation) FROM sync3_outbox",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("private-secret"));
    // The resolved part remains in its original message after many rollovers.
    if let MessagePart::Tool {
        resolved,
        output,
        progress,
        output_ref,
        output_bytes,
        ..
    } = &mut parts[1]
    {
        *resolved = true;
        *output = Some("Done".into());
        *progress = None;
        *output_ref = Some("chat/tool".into());
        *output_bytes = Some(1234);
    }
    if let MessagePart::Input { resolved, .. } = &mut parts[2] {
        *resolved = true;
    }
    finish_all(&mut w, &mut j, &parts);
    apply_outbox(&mut j);
    let projected = ordered_parts(&j);
    assert_eq!(joined_text(&projected), format!("before{body}"));
    assert!(projected.contains(&render_parts(&parts[1..2])[0]));
    assert!(projected.contains(&parts[2]));
    let projection = j.projection().unwrap();
    assert!(projection.messages.len() > 4);
    for (id, message) in &projection.messages {
        assert_eq!(message.entry.status, Some(MessageStatus::Complete));
        if id != "root" {
            assert_eq!(message.entry.continuation_of.as_deref(), Some("root"));
        }
        assert!(serde_json::to_vec(message).unwrap().len() <= wire::MAX_MESSAGE_BYTES);
    }
    let last: String =
        j.db.query_row(
            "SELECT operation FROM sync3_events ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        matches!(serde_json::from_str::<Operation>(&last).unwrap().event, Event::MessageFinished { message_id, .. } if message_id == "root")
    );
    assert!(
        w.sync(&parts, |_| panic!("closed writer reached sink"))
            .is_err()
    );
}

#[test]
fn committed_but_lost_sink_result_retries_exact_bytes_then_catches_up() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    let mut sent = Vec::new();
    assert!(
        w.sync(&[text("text", "old")], |frame| {
            sent = frame.operations.clone();
            j.enqueue_writer_frame(frame)?;
            Err(invalid("lost_sink_result"))
        })
        .is_err()
    );
    let changes = j.db.total_changes();
    let progress = w
        .sync(&[text("text", "old new")], |frame| {
            assert_eq!(frame.operations, sent);
            j.enqueue_writer_frame(frame)
        })
        .unwrap();
    assert!(progress.more);
    assert_eq!(j.db.total_changes(), changes);
    finish_all(&mut w, &mut j, &[text("text", "old new")]);
    apply_outbox(&mut j);
    assert_eq!(joined_text(&ordered_parts(&j)), "old new");
}

#[test]
fn restart_restores_progress_without_reserializing_full_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("writer.sqlite");
    let parts = vec![text("text", &"🙂\0".repeat(150_000))];
    let mut j = Journal::open(&path, "account", "room", "host").unwrap();
    let mut w = writer();
    assert!(
        w.sync(&parts, |frame| j.enqueue_writer_frame(frame))
            .unwrap()
            .more
    );
    drop(w);
    drop(j);
    let mut j = Journal::open(&path, "account", "room", "host").unwrap();
    let mut w = j.load_writer("root").unwrap().unwrap();
    let Memo::Text { bytes, .. } = w.state.slots[0].memo else {
        panic!("text slot")
    };
    let MessagePart::Text { text: source, .. } = &parts[0] else {
        panic!("text source")
    };
    assert!(
        w.sync(&[text("text", &source[..bytes])], |_| panic!(
            "unwritten suffix was silently discarded"
        ))
        .is_err()
    );
    finish_all(&mut w, &mut j, &parts);
    let metadata_bytes: usize =
        j.db.query_row(
            "SELECT sum(length(body)) FROM sync3_writer_items",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(metadata_bytes < 10_000, "{metadata_bytes}");
    apply_outbox(&mut j);
    assert_eq!(joined_text(&ordered_parts(&j)), joined_text(&parts));
    drop(w);
    drop(j);
    let j = Journal::open(&path, "account", "room", "host").unwrap();
    let mut w = j.load_writer("root").unwrap().unwrap();
    let result = w
        .finish(&parts, Some(MessageStatus::Complete), |_| {
            panic!("finished writer wrote again")
        })
        .unwrap();
    assert_eq!(
        result,
        Progress {
            operations: 0,
            more: false
        }
    );
    assert!(
        w.finish(&parts, Some(MessageStatus::Aborted), |_| Ok(()))
            .is_err()
    );
}

#[test]
fn interrupted_multi_frame_finalization_resumes_without_reopening_children() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    let parts = (0..70)
        .map(|i| MessagePart::Error {
            id: format!("error-{i}"),
            message: "retained".into(),
        })
        .collect::<Vec<_>>();
    sync_all(&mut w, &mut j, &parts);
    assert!(
        w.finish(&parts, Some(MessageStatus::Complete), |frame| {
            assert_eq!(frame.operations.len(), 64);
            j.enqueue_writer_frame(frame)?;
            Err(invalid("lost_finalization_result"))
        })
        .is_err()
    );
    drop(w);
    let mut w = j.load_writer("root").unwrap().unwrap();
    finish_all(&mut w, &mut j, &parts);
    apply_outbox(&mut j);
    assert_eq!(ordered_parts(&j), parts);
    assert_eq!(j.projection().unwrap().messages.len(), 70);
    assert!(
        j.projection()
            .unwrap()
            .messages
            .values()
            .all(|m| m.entry.status == Some(MessageStatus::Complete))
    );
}

#[test]
fn malformed_checkpoint_offsets_and_mismatched_revision_fail_closed() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    sync_all(&mut w, &mut j, &[text("text", "value")]);
    j.db.execute("UPDATE sync3_writers SET revision=revision+1", [])
        .unwrap();
    assert!(j.load_writer("root").is_err());
    j.db.execute("UPDATE sync3_writers SET revision=revision-1", [])
        .unwrap();
    let body: String =
        j.db.query_row(
            "SELECT body FROM sync3_writer_items WHERE kind='slot'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
    value["memo"]["Text"]["tail_bytes"] = serde_json::json!(wire::MAX_MESSAGE_BYTES + 1);
    j.db.execute(
        "UPDATE sync3_writer_items SET body=? WHERE kind='slot'",
        [serde_json::to_string(&value).unwrap()],
    )
    .unwrap();
    assert!(j.load_writer("root").is_err());
    assert_eq!(j.pending().unwrap().len(), 3);
}

#[test]
fn empty_finish_persists_terminal_producer_without_a_fake_message() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    assert_eq!(
        w.finish(&[], None, |frame| j.enqueue_writer_frame(frame))
            .unwrap(),
        Progress {
            operations: 0,
            more: false
        }
    );
    assert!(j.pending().unwrap().is_empty());
    assert!(j.projection().unwrap().messages.is_empty());
    let mut restored = j.load_writer("root").unwrap().unwrap();
    assert_eq!(
        restored
            .finish(&[], None, |_| panic!("empty finish repeated"))
            .unwrap(),
        Progress {
            operations: 0,
            more: false
        }
    );
}

#[test]
fn invalid_fold_is_not_partially_enqueued_or_silently_rewritten() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    assert!(
        w.sync(&[text("text", "first"), text("bad/id", "bad")], |_| panic!(
            "invalid frame reached sink"
        ))
        .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    sync_all(&mut w, &mut j, &[text("text", "first")]);
    for bad in [vec![text("text", "other")], vec![], vec![input("text")]] {
        assert!(
            w.sync(&bad, |_| panic!("invalid change reached sink"))
                .is_err()
        );
    }
    let oversized = MessagePart::Error {
        id: "error".into(),
        message: "x".repeat(65 * 1024),
    };
    assert!(
        w.sync(&[text("text", "first"), oversized], |_| panic!(
            "oversized field reached sink"
        ))
        .is_err()
    );
    finish_all(&mut w, &mut j, &[text("text", "first")]);
    apply_outbox(&mut j);
    assert_eq!(joined_text(&ordered_parts(&j)), "first");
}

#[test]
fn part_count_and_repeated_ids_roll_over_and_token_checkpoint_writes_stay_sparse() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    let mut parts = (0..400)
        .map(|i| input(&format!("input-{i}")))
        .collect::<Vec<_>>();
    parts.push(input("input-399")); // same ID, different physical message
    parts.push(text("tail", "a"));
    sync_all(&mut w, &mut j, &parts);
    let changes = j.db.total_changes();
    parts.last_mut().map(|p| *p = text("tail", "ab"));
    let progress = w
        .sync(&parts, |frame| {
            assert_eq!(frame.operations.len(), 1);
            assert_eq!(frame.updates.len(), 2); // one chunk + one slot, not all 402 slots
            j.enqueue_writer_frame(frame)
        })
        .unwrap();
    assert!(!progress.more);
    assert_eq!(j.db.total_changes() - changes, 4); // operation, header, chunk, slot
    finish_all(&mut w, &mut j, &parts);
    apply_outbox(&mut j);
    assert_eq!(ordered_parts(&j), parts);
}

#[test]
fn batch_conflict_rolls_back_outbox_and_writer_metadata() {
    let mut j = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    let mut w = writer();
    assert!(
        w.sync(&[text("text", "value")], |frame| {
            // Poison a later ID deliberately; earlier inserts must roll back.
            let mut conflict = frame.operations.last().unwrap().clone();
            if let Event::TextAppended { text, .. } = &mut conflict.event {
                *text = "different".into();
            }
            j.enqueue(&conflict)?;
            j.enqueue_writer_frame(frame)
        })
        .is_err()
    );
    assert_eq!(j.pending().unwrap().len(), 1);
    assert!(j.load_writer("root").unwrap().is_none());
    assert_eq!(
        j.db.query_row("SELECT count(*) FROM sync3_writer_items", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    j.db.execute("DELETE FROM sync3_outbox", []).unwrap();
    sync_all(&mut w, &mut j, &[text("text", "value")]);
    let mut stale = writer();
    assert!(
        stale
            .sync(&[text("text", "value")], |frame| j
                .enqueue_writer_frame(frame))
            .is_ok()
    ); // exact frame retry
    // Once progress has advanced, another producer at the old revision cannot
    // replace it with a divergent frame.
    sync_all(&mut w, &mut j, &[text("text", "value more")]);
    assert!(
        stale
            .sync(&[text("text", "value other")], |frame| j
                .enqueue_writer_frame(frame))
            .is_err()
    );
    let rows: u64 =
        j.db.query_row(
            "SELECT count(*) FROM sync3_writer_items WHERE writer_id=?",
            params!["root"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2);
}

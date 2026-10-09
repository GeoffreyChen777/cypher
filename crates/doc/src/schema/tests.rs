use super::*;
use crate::parts::fold_event_into_parts;
use cypher_proto::{AgentEvent, ToolCall};

fn user_entry(id: &str, text: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: text.into(),
            agent_text: None,
        }],
        created_at: 1,
        device_id: "dev-a".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    }
}

#[test]
fn round_trips_message_entries() {
    let doc = SessionDoc::init("chat-1").unwrap();
    doc.push_message(&user_entry("m1", "hello")).unwrap();
    let entries = doc.read_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "m1");
    assert_eq!(
        entries[0].parts,
        vec![MessagePart::Text {
            id: "t0".into(),
            text: "hello".into(),
            agent_text: None,
        }]
    );
    assert_eq!(doc.chat_id().as_deref(), Some("chat-1"));
}

#[test]
fn round_trips_message_comments() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut commented = user_entry("m1", "");
    commented.comments = vec![MessageComment {
        quote: "a \"quoted\"\nline".into(),
        comment: "why?".into(),
    }];
    doc.push_message(&commented).unwrap();
    doc.push_message(&user_entry("m2", "plain")).unwrap();
    let other = LoroDoc::new();
    other.import(&doc.export_snapshot().unwrap()).unwrap();
    let entries = SessionDoc::from_doc(other).read_entries().unwrap();
    assert_eq!(entries[0].comments, commented.comments);
    assert!(entries[1].comments.is_empty());
}

#[test]
fn remove_messages_truncates_and_leaves_the_prefix_intact() {
    let doc = SessionDoc::init("chat-1").unwrap();
    for id in ["m1", "m2", "m3", "m4"] {
        doc.push_message(&user_entry(id, id)).unwrap();
    }
    let cut: std::collections::HashSet<String> =
        ["m2".to_string(), "m4".to_string()].into_iter().collect();
    assert_eq!(doc.remove_messages(&cut).unwrap(), 2);
    let left: Vec<String> = doc
        .read_entries()
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(left, vec!["m1".to_string(), "m3".to_string()]);
    // Idempotent: the ids are already gone.
    assert_eq!(doc.remove_messages(&cut).unwrap(), 0);
    // An empty set never touches the doc.
    assert_eq!(
        doc.remove_messages(&std::collections::HashSet::new())
            .unwrap(),
        0
    );
    assert_eq!(doc.read_entries().unwrap().len(), 2);
}

#[test]
fn resolve_input_stamps_the_part_in_place() {
    let doc = SessionDoc::init("chat-1").unwrap();
    doc.push_message(&SessionMessageEntry {
        id: "m1".into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Input {
            id: "r1".into(),
            request_id: "r1".into(),
            questions: vec![],
            resolved: false,
        }],
        created_at: 1,
        device_id: "dev-a".into(),
        // The orphan case: the run died and recovery stamped the entry.
        status: Some(MessageStatus::Aborted),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    })
    .unwrap();
    assert!(!doc.resolve_input("nope").unwrap());
    assert!(doc.resolve_input("r1").unwrap());
    let entries = doc.read_entries().unwrap();
    assert!(matches!(
        &entries[0].parts[0],
        MessagePart::Input { resolved: true, .. }
    ));
}

#[test]
fn snapshot_round_trips_between_docs() {
    let doc = SessionDoc::init("chat-1").unwrap();
    doc.push_message(&user_entry("m1", "hello")).unwrap();
    let bytes = doc.export_snapshot().unwrap();

    let other = LoroDoc::new();
    other.import(&bytes).unwrap();
    let restored = SessionDoc::from_doc(other);
    assert_eq!(
        restored.read_entries().unwrap(),
        doc.read_entries().unwrap()
    );
}

#[test]
fn two_peers_converge_on_concurrent_inserts() {
    let a = SessionDoc::init("chat-1").unwrap();
    let b = SessionDoc::from_doc({
        let d = LoroDoc::new();
        d.import(&a.export_snapshot().unwrap()).unwrap();
        d
    });
    a.push_message(&user_entry("m-a", "from a")).unwrap();
    b.push_message(&user_entry("m-b", "from b")).unwrap();

    // Cross-import updates.
    let a_update = a
        .doc()
        .export(ExportMode::updates(&b.doc().oplog_vv()))
        .unwrap();
    let b_update = b
        .doc()
        .export(ExportMode::updates(&a.doc().oplog_vv()))
        .unwrap();
    b.doc().import(&a_update).unwrap();
    a.doc().import(&b_update).unwrap();

    let ea = a.read_entries().unwrap();
    let eb = b.read_entries().unwrap();
    assert_eq!(ea, eb);
    assert_eq!(ea.len(), 2); // one insert from each peer, converged in the same order
}

#[test]
fn segment_writer_streams_text_incrementally() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

    let mut folded = Vec::new();
    fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "Hel".into() });
    writer.sync(&folded).unwrap();
    fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "lo".into() });
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolCall {
            id: "tool-1".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolResult {
            id: "tool-1".into(),
            is_error: false,
            output: None,
            diff: None,
        },
    );
    writer.sync(&folded).unwrap();
    writer.finish(&folded, MessageStatus::Complete).unwrap();

    let entries = doc.read_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, Some(MessageStatus::Complete));
    assert_eq!(entries[0].parts.len(), 2);
    match &entries[0].parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "Hello"),
        other => panic!("unexpected {other:?}"),
    }
    match &entries[0].parts[1] {
        MessagePart::Tool {
            resolved, is_error, ..
        } => {
            assert!(*resolved);
            assert!(!*is_error);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn segment_writer_streams_reasoning_under_its_own_key() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
    let mut folded = Vec::new();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ReasoningDelta {
            text: "Weigh ".into(),
        },
    );
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ReasoningDelta {
            text: "the options.".into(),
        },
    );
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::TextDelta {
            text: "Done.".into(),
        },
    );
    writer.finish(&folded, MessageStatus::Complete).unwrap();

    assert_eq!(
        doc.read_entries().unwrap()[0].parts,
        [
            MessagePart::Reasoning {
                id: "r0".into(),
                text: "Weigh the options.".into(),
            },
            MessagePart::Text {
                id: "t1".into(),
                text: "Done.".into(),
                agent_text: None,
            },
        ]
    );
    // Readers older than the kind decode an unknown kind as a text part
    // from `text`. With none they get an empty one, which they skip —
    // never the thinking shown as the answer.
    let raw = doc
        .doc()
        .get_list("messages")
        .get_deep_value()
        .to_json_value();
    let part = &raw[0]["parts"][0];
    assert_eq!(part["kind"], "reasoning");
    assert_eq!(part["reasoning"], "Weigh the options.");
    assert!(part.get("text").is_none());
}

#[test]
fn only_a_commit_that_grew_reasoning_alone_is_deferrable() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let marks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let _sub = doc.doc().subscribe_local_update(Box::new({
        let marks = marks.clone();
        move |_| {
            marks.lock().unwrap().push(local_commit_is_deferrable());
            true
        }
    }));
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
    let mut folded = Vec::new();
    for event in [
        AgentEvent::ReasoningDelta { text: "a".into() }, // new thought
        AgentEvent::ReasoningDelta { text: "b".into() }, // growth
        AgentEvent::TextDelta { text: "x".into() },      // answer text
        AgentEvent::ReasoningDelta { text: "c".into() }, // a later thought
    ] {
        fold_event_into_parts(&mut folded, &event);
        writer.sync(&folded).unwrap();
    }
    writer.finish(&folded, MessageStatus::Complete).unwrap();
    // begin, a, b, x, c, finish (status + completedAt).
    assert_eq!(
        *marks.lock().unwrap(),
        [false, true, true, false, true, false]
    );
    assert!(
        !local_commit_is_deferrable(),
        "the mark never outlives its commit"
    );
}

fn agent_text_of(doc: &SessionDoc) -> (String, Option<String>) {
    match &doc.read_entries().unwrap()[0].parts[0] {
        MessagePart::Text {
            text, agent_text, ..
        } => (text.clone(), agent_text.clone()),
        other => panic!("unexpected {other:?}"),
    }
}

/// A replace-mode translation rewrites the text wholesale; the answer the
/// model wrote survives in Loro as the part's agent version.
#[test]
fn segment_writer_keeps_the_original_under_a_translation() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
    let mut folded = Vec::new();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::TextDelta {
            text: "The answer.".into(),
        },
    );
    writer.sync(&folded).unwrap();
    for frame in ["The answer.", "答", "答案。"] {
        fold_event_into_parts(&mut folded, &AgentEvent::Translation { text: frame.into() });
        writer.sync(&folded).unwrap();
    }
    writer.finish(&folded, MessageStatus::Complete).unwrap();
    assert_eq!(
        agent_text_of(&doc),
        ("答案。".to_string(), Some("The answer.".to_string()))
    );
}

/// Append mode grows the text with the original as its prefix — the
/// writer's append path must still stamp the agent version.
#[test]
fn segment_writer_stamps_the_original_on_an_append_mode_growth() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
    let mut folded = Vec::new();
    fold_event_into_parts(&mut folded, &AgentEvent::TextDelta { text: "Hi.".into() });
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::Translation {
            text: "Hi.\n\n---\n\n你好。".into(),
        },
    );
    writer.sync(&folded).unwrap();
    assert_eq!(
        agent_text_of(&doc),
        ("Hi.\n\n---\n\n你好。".to_string(), Some("Hi.".to_string()))
    );
}

/// A translation that fails puts the original back — nothing is
/// translated any more, so the agent version must be DELETED in Loro, not
/// just dropped from the fold's mirror.
#[test]
fn a_restored_original_clears_the_agent_version() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();
    let mut folded = Vec::new();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::TextDelta {
            text: "Answer".into(),
        },
    );
    writer.sync(&folded).unwrap();
    fold_event_into_parts(&mut folded, &AgentEvent::Translation { text: "答".into() });
    writer.sync(&folded).unwrap();
    assert_eq!(agent_text_of(&doc).1.as_deref(), Some("Answer"));
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::Translation {
            text: "Answer".into(),
        },
    );
    writer.finish(&folded, MessageStatus::Complete).unwrap();
    assert_eq!(agent_text_of(&doc), ("Answer".to_string(), None));
}

#[test]
fn user_agent_text_stamps_the_newest_matching_unstamped_prompt() {
    let doc = SessionDoc::init("chat-1").unwrap();
    doc.push_message(&user_entry("m1", "你好")).unwrap();
    doc.push_message(&user_entry("m2", "别的")).unwrap();
    doc.push_message(&user_entry("m3", "你好")).unwrap();
    // Outer whitespace on either side is not a difference.
    assert!(doc.stamp_user_agent_text(" 你好\n", "Hello").unwrap());
    assert!(doc.stamp_user_agent_text("你好", "Hi").unwrap());
    // Every copy is stamped; nothing else matches.
    assert!(!doc.stamp_user_agent_text("你好", "Hey").unwrap());
    assert!(!doc.stamp_user_agent_text("missing", "x").unwrap());
    // An "unchanged" translation is no translation.
    assert!(!doc.stamp_user_agent_text("别的", "别的").unwrap());
    let stamped: Vec<Option<String>> = doc
        .read_entries()
        .unwrap()
        .into_iter()
        .map(|e| match &e.parts[0] {
            MessagePart::Text { agent_text, .. } => agent_text.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(stamped, vec![Some("Hi".into()), None, Some("Hello".into())]);
}

/// Live progress is transient column state: a ToolCall creates the part
/// without it, ToolProgress writes the tail into Loro, and ToolResult
/// CLEARS it (the delete must actually land in Loro — not just in the
/// fold's in-memory mirror — or a settled chip keeps a stale live tail).
#[test]
fn segment_writer_persists_progress_and_clears_on_resolve() {
    let doc = SessionDoc::init("chat-4").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

    let mut folded = Vec::new();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolCall {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    writer.sync(&folded).unwrap();
    let entries = doc.read_entries().unwrap();
    assert!(matches!(
        &entries[0].parts[0],
        MessagePart::Tool {
            progress: None,
            resolved: false,
            ..
        }
    ));

    // A progress tick writes the transient tail into Loro.
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolProgress {
            id: "t1".into(),
            output: "compiling\nlinking".into(),
        },
    );
    writer.sync(&folded).unwrap();
    let entries = doc.read_entries().unwrap();
    match &entries[0].parts[0] {
        MessagePart::Tool {
            progress,
            resolved: false,
            ..
        } => assert_eq!(progress.as_deref(), Some("compiling\nlinking")),
        other => panic!("unexpected {other:?}"),
    }

    // Resolve clears the column in Loro.
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolResult {
            id: "t1".into(),
            is_error: false,
            output: None,
            diff: None,
        },
    );
    writer.sync(&folded).unwrap();
    writer.finish(&folded, MessageStatus::Complete).unwrap();
    let entries = doc.read_entries().unwrap();
    match &entries[0].parts[0] {
        MessagePart::Tool {
            resolved: true,
            progress: None,
            ..
        } => {}
        other => panic!("unexpected {other:?}"),
    }
}

/// The ToolResult resolution path goes through `update_part_fields` —
/// the stripped output summary, sidecar refs, and diff stats must survive
/// the doc round trip (regression: output/diff were silently dropped
/// there while `to_doc_part` carried them).
#[test]
fn segment_writer_round_trips_stripped_tool_fields() {
    let doc = SessionDoc::init("chat-2").unwrap();
    let mut writer = SegmentWriter::begin(&doc, "a1", "dev-a", 5).unwrap();

    let mut folded = Vec::new();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolCall {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
        },
    );
    writer.sync(&folded).unwrap();
    fold_event_into_parts(
        &mut folded,
        &AgentEvent::ToolResult {
            id: "t1".into(),
            is_error: false,
            output: Some("total 0\nmore lines".into()),
            diff: Some(cypher_proto::ToolDiff {
                path: "/w/a.rs".into(),
                old_text: Some("old\n".into()),
                new_text: "new\n".into(),
            }),
        },
    );
    crate::parts::apply_sidecar_refs("chat-2", &mut folded);
    writer.sync(&folded).unwrap();
    writer.finish(&folded, MessageStatus::Complete).unwrap();

    let entries = doc.read_entries().unwrap();
    match &entries[0].parts[0] {
        MessagePart::Tool {
            output,
            output_ref,
            output_bytes,
            diff,
            diff_ref,
            diff_stats,
            ..
        } => {
            // The bounded output summary is retained for the expandable
            // chip body. Full-output sidecar refs remain absent while
            // sidecar storage is disabled; diff stats still get their ref.
            assert_eq!(output.as_deref(), Some("total 0\nmore lines"));
            assert_eq!(output_ref.as_deref(), None);
            assert_eq!(*output_bytes, None);
            assert!(diff.is_none(), "no inline diff text in the doc");
            assert_eq!(diff_ref.as_deref(), Some("chat-2/t1.diff"));
            let stats = diff_stats.as_ref().expect("stats survive");
            assert_eq!(stats[0].path, "/w/a.rs");
            assert_eq!((stats[0].additions, stats[0].deletions), (1, 1));
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Old pre-strip docs carry inline `output`/`diff` — they must still read
/// back (schema changes are serde-additive ONLY; old readers, old docs).
#[test]
fn pre_strip_doc_parts_still_round_trip() {
    let doc = SessionDoc::init("chat-3").unwrap();
    doc.push_message(&SessionMessageEntry {
        id: "m1".into(),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Tool {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "ls".into(),
            },
            is_error: false,
            resolved: true,
            output: Some("full inline output\nline 2".into()),
            progress: None,
            diff: Some(cypher_proto::ToolDiff {
                path: "/w/a.rs".into(),
                old_text: Some("old".into()),
                new_text: "new".into(),
            }),
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
        }],
        created_at: 1,
        device_id: "dev-a".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
        models: Vec::new(),
    })
    .unwrap();
    let entries = doc.read_entries().unwrap();
    match &entries[0].parts[0] {
        MessagePart::Tool { output, diff, .. } => {
            assert_eq!(output.as_deref(), Some("full inline output\nline 2"));
            assert_eq!(diff.as_ref().unwrap().new_text, "new");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn set_message_status_stamps_existing_entry() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let mut entry = user_entry("m1", "hello");
    entry.role = MessageRole::Assistant;
    entry.status = Some(MessageStatus::Streaming);
    doc.push_message(&entry).unwrap();

    assert!(
        doc.set_message_status("m1", MessageStatus::Aborted)
            .unwrap()
    );
    assert!(
        !doc.set_message_status("nope", MessageStatus::Aborted)
            .unwrap()
    );
    let entries = doc.read_entries().unwrap();
    assert_eq!(entries[0].status, Some(MessageStatus::Aborted));
    // Crash recovery closes the entry here: it still gets a span.
    assert!(
        entries[0]
            .completed_at
            .is_some_and(|at| at >= entry.created_at)
    );
}

/// A finished segment carries its own span: `createdAt` → `completedAt`.
/// That pair is the only durable record of how long a settled turn took
/// (the transcript's "Worked for …" rule), so it must survive a reload of
/// the doc, not just the live session.
#[test]
fn finish_stamps_the_completion_instant() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let started = chrono::Utc::now().timestamp_millis();
    let writer = SegmentWriter::begin(&doc, "m1", "dev", started).unwrap();
    let folded = vec![MessagePart::Text {
        id: "t0".into(),
        text: "hello".into(),
        agent_text: None,
    }];
    // Streaming entries carry no completion — the turn is still open.
    assert_eq!(doc.read_entries().unwrap()[0].completed_at, None);
    writer.finish(&folded, MessageStatus::Complete).unwrap();

    let reopened = LoroDoc::new();
    reopened.import(&doc.export_snapshot().unwrap()).unwrap();
    let entry = SessionDoc::from_doc(reopened)
        .read_entries()
        .unwrap()
        .remove(0);
    assert_eq!(entry.status, Some(MessageStatus::Complete));
    assert!(entry.completed_at.is_some_and(|at| at >= started));
}

/// The join spans the root's start to the LAST segment's finish — and a
/// continuation still streaming leaves the joined entry unstamped.
#[test]
fn continuation_join_takes_the_last_segments_completion() {
    let mut root = user_entry("m1", "a");
    root.role = MessageRole::Assistant;
    root.created_at = 1_000;
    root.completed_at = Some(2_000);
    let mut tail = user_entry("m1#c1", "b");
    tail.role = MessageRole::Assistant;
    tail.continuation_of = Some("m1".into());
    tail.completed_at = Some(9_000);

    let joined = join_continuation_entries(vec![root.clone(), tail.clone()]);
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].created_at, 1_000);
    assert_eq!(joined[0].completed_at, Some(9_000));

    let mut live_tail = tail;
    live_tail.completed_at = None;
    let joined = join_continuation_entries(vec![root, live_tail]);
    assert_eq!(joined[0].completed_at, None, "still being written");
}

fn answered(model: &str, requested: Option<&str>) -> AnsweredModel {
    AnsweredModel {
        model: model.into(),
        requested: requested.map(str::to_owned),
    }
}

/// The models a segment's writer records survive a reload of the doc,
/// and an entry without any carries none.
#[test]
fn segment_models_round_trip() {
    let doc = SessionDoc::init("chat-1").unwrap();
    let folded = vec![MessagePart::Text {
        id: "t0".into(),
        text: "hello".into(),
        agent_text: None,
    }];
    let mut writer = SegmentWriter::begin(&doc, "m1", "dev", 1_000).unwrap();
    writer.sync(&folded).unwrap();
    let models = vec![
        answered("gpt-5.4", Some("gpt-6-astra")),
        answered("gpt-6-astra", None),
    ];
    writer.set_models(&models).unwrap();
    // A models change alone still lands, with no new parts to sync.
    writer.sync(&folded).unwrap();
    assert_eq!(doc.read_entries().unwrap()[0].models, models);
    writer.finish(&folded, MessageStatus::Complete).unwrap();
    SegmentWriter::begin(&doc, "m2", "dev", 2_000)
        .unwrap()
        .finish(&folded, MessageStatus::Complete)
        .unwrap();

    let reopened = LoroDoc::new();
    reopened.import(&doc.export_snapshot().unwrap()).unwrap();
    let entries = SessionDoc::from_doc(reopened).read_entries().unwrap();
    assert_eq!(entries[0].models, models);
    assert!(entries[1].models.is_empty());
}

/// A joined entry carries every model that answered any of its
/// segments, once each, in order.
#[test]
fn continuation_join_merges_the_segments_models() {
    let mut root = user_entry("m1", "a");
    root.role = MessageRole::Assistant;
    root.models = vec![answered("gpt-6-astra", None)];
    let mut tail = user_entry("m1#c1", "b");
    tail.role = MessageRole::Assistant;
    tail.continuation_of = Some("m1".into());
    tail.models = vec![
        answered("gpt-6-astra", None),
        answered("gpt-5.4", Some("gpt-6-astra")),
    ];
    let joined = join_continuation_entries(vec![root, tail]);
    assert_eq!(
        joined[0].models,
        vec![
            answered("gpt-6-astra", None),
            answered("gpt-5.4", Some("gpt-6-astra")),
        ]
    );
}

#[test]
fn command_queue_and_outcome_round_trip() {
    use crate::commands::{SessionCommandPayload, SessionCommandStatus};
    let doc = SessionDoc::init("chat-1").unwrap();
    let entry = SessionCommandEntry {
        id: "c1".into(),
        payload: SessionCommandPayload::Steer {
            prompt: "focus".into(),
            message_id: None,
            agent_prompt: None,
        },
        issued_by: "dev-b".into(),
        issued_at: 10,
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
        sent_at: None,
    };
    doc.queue_command(&entry).unwrap();
    doc.set_command_status("c1", SessionCommandStatus::Applied, None)
        .unwrap();
    let commands = doc.read_commands().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].status, SessionCommandStatus::Applied);
    assert_eq!(commands[0].payload, entry.payload);
}

#[test]
fn run_request_attachments_survive_command_round_trip() {
    use crate::commands::SessionCommandPayload;
    let doc = SessionDoc::init("chat-1").unwrap();
    let request = cypher_proto::RunRequest {
        prompt: "p".into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: vec!["/tmp/a.png".into()],
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    };
    doc.queue_command(&SessionCommandEntry {
        id: "c1".into(),
        payload: SessionCommandPayload::Run {
            request,
            message_id: "m1".into(),
            agent_prompt: None,
        },
        issued_by: "d".into(),
        issued_at: 1,
        based_on: None,
        expires_at: None,
        status: SessionCommandStatus::Pending,
        resolution: None,
        sent_at: None,
    })
    .unwrap();
    match &doc.read_commands().unwrap()[0].payload {
        SessionCommandPayload::Run { request, .. } => {
            assert_eq!(request.attachments, vec!["/tmp/a.png".to_string()]);
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

#[test]
fn sealed_attachments_round_trip_and_survive_snapshot() {
    // A fresh doc (no container) reads empty — the container is additive.
    let doc = SessionDoc::init("chat-1").unwrap();
    assert!(doc.sealed_attachment("up-1").unwrap().is_none());
    assert!(doc.sealed_attachments().unwrap().is_empty());

    doc.seal_attachment("up-1", "/up/1-a.png", "a.png").unwrap();
    doc.seal_attachment("up-2", "/up/2-b.png", "b.png").unwrap();
    assert_eq!(
        doc.sealed_attachment("up-1").unwrap(),
        Some(("/up/1-a.png".into(), "a.png".into()))
    );
    let mut sealed = doc.sealed_attachments().unwrap();
    sealed.sort();
    assert_eq!(
        sealed,
        vec![
            ("up-1".into(), "/up/1-a.png".into(), "a.png".into()),
            ("up-2".into(), "/up/2-b.png".into(), "b.png".into()),
        ]
    );
    // Re-sealing the same id is idempotent (overwrites in place).
    doc.seal_attachment("up-1", "/up/1-c.png", "c.png").unwrap();
    assert_eq!(
        doc.sealed_attachment("up-1").unwrap(),
        Some(("/up/1-c.png".into(), "c.png".into()))
    );
    assert_eq!(doc.sealed_attachments().unwrap().len(), 2);

    // The container crosses an export/import snapshot intact.
    let bytes = doc.export_snapshot().unwrap();
    let restored = SessionDoc::from_doc({
        let d = loro::LoroDoc::new();
        d.import(&bytes).unwrap();
        d
    });
    assert_eq!(
        restored.sealed_attachment("up-1").unwrap(),
        Some(("/up/1-c.png".into(), "c.png".into()))
    );
    assert_eq!(restored.sealed_attachments().unwrap().len(), 2);
}

/// Regression guard: entries/parts missing strict fields must salvage
/// field-by-field — a fresh reader importing a room's merged doc must
/// never render a BLANK transcript because some writer (old app version,
/// other-platform client, mangled export) omitted metadata.
#[test]
fn malformed_entries_salvage_instead_of_vanishing() {
    // Entry missing `id` + `deviceId`; one part missing `kind` but
    // carrying text; one part contentless (dropped).
    let v = serde_json::json!({
        "role": "assistant",
        "createdAt": 123,
        "parts": [
            { "id": "p1", "text": "still readable" },
            { "opaque": true },
            { "id": "p3", "kind": "text", "text": "well-formed" }
        ]
    });
    let entry = entry_from_json(v.clone()).expect("salvaged");
    assert!(
        entry.id.starts_with("recovered-"),
        "deterministic stand-in id"
    );
    let again = entry_from_json(v).expect("salvaged again");
    assert_eq!(entry.id, again.id, "recovered id is stable across reads");
    assert_eq!(entry.role, MessageRole::Assistant);
    assert_eq!(entry.created_at, 123);
    assert_eq!(
        entry.parts.len(),
        2,
        "text parts survive, contentless part dropped"
    );
    match &entry.parts[0] {
        MessagePart::Text { text, .. } => assert_eq!(text, "still readable"),
        other => panic!("unexpected {other:?}"),
    }

    // Tool part missing `kind` but with a parseable call salvages as Tool.
    let v = serde_json::json!({
        "role": "assistant",
        "createdAt": 1,
        "parts": [ { "id": "t1", "call": { "kind": "exec", "command": "ls" }, "output": "x" } ]
    });
    let entry = entry_from_json(v).expect("salvaged");
    assert!(matches!(
        &entry.parts[0],
        MessagePart::Tool { resolved: true, .. }
    ));

    // Only non-objects are truly unsalvageable.
    assert!(entry_from_json(serde_json::json!("garbage")).is_err());
    assert!(entry_from_json(serde_json::json!(42)).is_err());

    // Well-formed entries take the strict path unchanged.
    let v = serde_json::json!({
        "id": "m1", "role": "user", "createdAt": 5, "deviceId": "d",
        "parts": [ { "id": "p", "kind": "text", "text": "hi" } ]
    });
    let entry = entry_from_json(v).expect("strict");
    assert_eq!(entry.id, "m1");
}

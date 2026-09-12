//! Experimental v3 dispatch integration, not the normal SessionStore cutover.
//! Calls the real MockHarness API, folds its stream, journals raw events and
//! publishes bounded render frames only after the durable dispatch gate.
use cypher_harness::{Harness, RunControls, mock::MockHarness};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, MessagePart, MessageRole, MessageStatus,
    SessionCommandPayload, SessionCommandStatus, SessionMessageEntry, ToolCall,
    parts::fold_event_into_parts,
    sync3::{Operation, Outcome, Reply, Row},
};
use cypher_sync::sync3::{
    Journal,
    execution::{Plan, Progress},
};
use futures::StreamExt;

fn commit(journal: &mut Journal, ops: Vec<Operation>) {
    let base = journal.cursor().unwrap();
    let head = base + ops.len() as u64;
    journal
        .accept_state(&Reply::State {
            version: 3,
            epoch: 1,
            owner: "host".into(),
            owner_epoch: 1,
            head,
        })
        .unwrap();
    journal
        .apply_page(&Reply::Page {
            version: 3,
            epoch: 1,
            through: head,
            next: head,
            done: true,
            rows: ops
                .into_iter()
                .enumerate()
                .map(|(i, operation)| Row {
                    seq: base + i as u64 + 1,
                    operation,
                })
                .collect(),
        })
        .unwrap();
}
fn deliver(j: &mut Journal) {
    let ops = j.pending().unwrap();
    commit(j, ops);
}

#[tokio::test]
async fn durable_claim_drives_mock_stream_and_never_reissues_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replica.sqlite");
    let mut journal = Journal::open(&path, "account", "room", "host").unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
    let mut queued: Operation = serde_json::from_value(fixture["operations"][0].clone()).unwrap();
    if let cypher_proto::sync3::Event::CommandQueued { command, .. } = &mut queued.event {
        command.expires_at = None;
        command.based_on = None;
        command.issued_at = chrono::Utc::now().timestamp_millis();
        command.sent_at = Some(command.issued_at);
        if let SessionCommandPayload::Run { request, .. } = &mut command.payload {
            request.harness = Some(HarnessId::Mock);
            request.model = Some("mock-1".into());
            request.reasoning = Some(cypher_proto::ReasoningLevel::Medium);
            request.model_options.clear();
            request.cwd = dir.path().to_string_lossy().into();
            request.attachments.clear();
            request.pending_attachments.clear();
            request.worktree = None;
        }
    }
    commit(&mut journal, vec![queued]);
    journal.prepare_execution("command", Plan::Run).unwrap();
    deliver(&mut journal);
    assert!(matches!(
        journal.advance_execution("command").unwrap(),
        Progress::WaitingForRun
    ));
    deliver(&mut journal);
    let Progress::Dispatch(permit) = journal.advance_execution("command").unwrap() else {
        panic!("no permit")
    };
    let SessionCommandPayload::Run { request, .. } = &permit.command().payload else {
        panic!("wrong payload")
    };
    assert_eq!(request.harness, Some(HarnessId::Mock));

    let long = "你好🙂\n".repeat(30_000);
    let harness = MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "Before tool.".into(),
            },
            AgentEvent::ToolCall {
                id: "write".into(),
                call: ToolCall::WriteFile {
                    path: "mock-only.txt".into(),
                    content: Some("retained-private-content".into()),
                },
            },
            AgentEvent::TextDelta { text: long.clone() },
            AgentEvent::ToolResult {
                id: "write".into(),
                is_error: false,
                output: Some("Done".into()),
                diff: None,
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("mock-process".into()),
            },
        ],
    };
    let (_steer, steering) = tokio::sync::mpsc::channel(1);
    let mut stream = harness
        .run(
            request.clone(),
            RunControls {
                steering,
                interrupt: Default::default(),
                host: Default::default(),
                request_input: Box::new(|_| panic!("this script does not ask questions")),
            },
        )
        .await
        .unwrap();
    let entry = SessionMessageEntry {
        id: "reply".into(),
        role: MessageRole::Assistant,
        device_id: "host".into(),
        created_at: 1000,
        parts: vec![],
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    };
    let mut writer = journal.new_execution_writer(&permit, &entry).unwrap();
    let mut parts = vec![];
    let mut events = 0;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        events += 1;
        journal
            .append_execution_events(&permit, events, std::slice::from_ref(&event))
            .unwrap();
        fold_event_into_parts(&mut parts, &event);
        while writer
            .sync(&parts, |frame| {
                journal.enqueue_execution_frame(&permit, events, frame)
            })
            .unwrap()
            .more
        {}
        // Only restore producer metadata; never invoke the harness again.
        if events == 1 {
            drop(writer);
            writer = journal.load_writer("reply").unwrap().unwrap();
        }
    }
    assert_eq!(events, 5);
    while writer
        .finish(&parts, Some(MessageStatus::Complete), |frame| {
            journal.enqueue_execution_frame(&permit, events, frame)
        })
        .unwrap()
        .more
    {}
    journal
        .complete_execution(
            &permit,
            Some(Outcome::Completed),
            SessionCommandStatus::Applied,
            None,
        )
        .unwrap();
    while !journal.pending().unwrap().is_empty() {
        deliver(&mut journal);
    }
    journal
        .append_execution_events(
            &permit,
            events + 1,
            &[AgentEvent::TextDelta {
                text: "late private observation".into(),
            }],
        )
        .unwrap();
    assert!(
        journal.pending().unwrap().is_empty(),
        "late observations are not published"
    );
    let run_id = permit.run_id().to_owned();
    drop(permit);
    drop(writer);
    drop(journal);

    let mut journal = Journal::open(&path, "account", "room", "host").unwrap();
    assert!(matches!(
        journal.advance_execution("command").unwrap(),
        Progress::Settled
    ));
    let projection = journal.projection().unwrap();
    assert_eq!(projection.runs[&run_id].outcome, Some(Outcome::Completed));
    assert_eq!(
        projection.commands["command"].command.status,
        SessionCommandStatus::Applied
    );
    let mut messages = projection.messages.into_values().collect::<Vec<_>>();
    messages.sort_by_key(|m| m.created_seq);
    assert!(messages.len() > 1);
    let rendered = messages
        .into_iter()
        .flat_map(|m| m.entry.parts)
        .collect::<Vec<_>>();
    let text = rendered
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, format!("Before tool.{long}"));
    assert!(rendered.iter().any(|p| matches!(p, MessagePart::Tool { resolved: true, output: Some(output), .. } if output == "Done")));
    assert!(
        !serde_json::to_string(&rendered)
            .unwrap()
            .contains("retained-private-content")
    );
    let mut replay = Vec::new();
    let mut recovered_parts = Vec::new();
    let mut cursor = 0;
    loop {
        let page = journal.execution_events("command", cursor, 2).unwrap();
        for record in page.events {
            // Recovery is pure folding, never a harness call. Observations
            // after completion are retained but cannot reopen the run.
            if !record.after_completion {
                fold_event_into_parts(&mut recovered_parts, &record.event);
            }
            replay.push(record.event);
        }
        cursor = page.next;
        if cursor == page.through {
            break;
        }
    }
    assert_eq!(replay.len(), 6);
    assert_eq!(
        recovered_parts, parts,
        "raw-event replay retains the complete source fold"
    );
    assert!(
        serde_json::to_string(&replay)
            .unwrap()
            .contains("retained-private-content")
    );
    assert!(
        !dir.path().join("mock-only.txt").exists(),
        "MockHarness must not execute the scripted file tool"
    );
}

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
use std::sync::Arc;

#[tokio::test]
async fn normal_host_v3_queue_preserves_original_command_identity() {
    use cypher_engine::{EngineCore, HarnessRegistry};
    let dir = tempfile::tempdir().unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![
            AgentEvent::TextDelta {
                text: "normal v3 path".into(),
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ],
    }));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None).unwrap();
    let command_id = core
        .doc_host
        .queue_command(
            "normal-v3",
            SessionCommandPayload::Run {
                request: cypher_proto::RunRequest {
                    prompt: "run on v3".into(),
                    harness: Some(HarnessId::Mock),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: dir.path().to_string_lossy().into(),
                    sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    resume: None,
                    attachments: vec![],
                    pending_attachments: vec![],
                    worktree: None,
                },
                message_id: "original-user-id".into(),
                agent_prompt: None,
            },
        )
        .unwrap();
    let replica = core.sessions.session_replica("normal-v3").await.unwrap();
    let mut status = replica.watch();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            status.borrow_and_update();
            let p = replica.read(|j| j.projection()).unwrap();
            if p.commands
                .get(&command_id)
                .is_some_and(|c| c.command.status == SessionCommandStatus::Applied)
            {
                assert_eq!(p.commands.len(), 1, "no shadow/inner command identity");
                assert_eq!(p.messages.len(), 2);
                assert!(p.messages.contains_key("original-user-id"));
                assert!(p.commands[&command_id].accepted_op_id.is_some());
                assert!(
                    p.runs
                        .values()
                        .all(|r| r.outcome == Some(Outcome::Completed))
                );
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    core.shutdown().await;
}

#[tokio::test]
async fn v3_waiting_upload_expires_without_another_message_or_heartbeat_write() {
    use cypher_engine::{EngineCore, HarnessRegistry};
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(
        dir.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let handle = core.doc_host.open("waiting").unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    handle
        .queue_command(&cypher_proto::SessionCommandEntry {
            id: "expiring".into(),
            issued_by: core.device_id.clone(),
            issued_at: now,
            sent_at: None,
            based_on: None,
            expires_at: Some(now + 100),
            status: SessionCommandStatus::Pending,
            resolution: None,
            payload: SessionCommandPayload::Run {
                message_id: "waiting-user".into(),
                agent_prompt: None,
                request: cypher_proto::RunRequest {
                    prompt: "await upload".into(),
                    harness: Some(HarnessId::Mock),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: cypher_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    resume: None,
                    attachments: vec![],
                    worktree: None,
                    pending_attachments: vec![cypher_proto::PendingAttachment {
                        upload_id: "missing".into(),
                        file_name: "image.png".into(),
                    }],
                },
            },
        })
        .unwrap();
    let replica = handle.replica().unwrap();
    let mut status = replica.watch();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            status.borrow_and_update();
            let p = replica.read(|j| j.projection()).unwrap();
            if p.commands["expiring"].command.status == SessionCommandStatus::Expired {
                assert!(p.runs.is_empty());
                assert!(p.messages.is_empty());
                assert_eq!(
                    replica.read(|j| j.cursor()).unwrap(),
                    2,
                    "only enqueue and expiry, no durable heartbeats"
                );
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    core.shutdown().await;
}

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
async fn engine_replica_publishes_real_harness_source_and_retains_late_observations() {
    use cypher_engine::session_replica::{ExecutionPublication, SessionReplica};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine-v3.sqlite");
    let replica = Arc::new(SessionReplica::open_local(&path, "account", "room", "host").unwrap());
    let mut changed = replica.watch();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
    let mut queued: Operation = serde_json::from_value(fixture["operations"][0].clone()).unwrap();
    queued.actor = "host".into();
    if let cypher_proto::sync3::Event::CommandQueued { command, .. } = &mut queued.event {
        command.issued_by = "host".into();
    }
    replica.enqueue(&queued).unwrap();
    changed.changed().await.unwrap();
    assert_eq!(changed.borrow_and_update().cursor, 1);
    replica.prepare("command", Plan::Run).unwrap();
    assert!(matches!(
        replica.advance("command").unwrap(),
        Progress::WaitingForRun
    ));
    let Progress::Dispatch(permit) = replica.advance("command").unwrap() else {
        panic!("no permit");
    };
    let occupancy = replica.occupy(&permit).await.unwrap();
    let SessionCommandPayload::Run { request, .. } = &permit.command().payload else {
        panic!("not a run");
    };
    let request = request.clone();
    let mut publication = ExecutionPublication::new(
        replica.clone(),
        permit,
        &SessionMessageEntry {
            id: "real-reply".into(),
            role: MessageRole::Assistant,
            device_id: "host".into(),
            created_at: 1,
            parts: vec![],
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
        },
    )
    .unwrap();
    let response = "🙂\" bounded output\n".repeat(30_000);
    publication.set_cwd("/tmp").unwrap();
    let harness = MockHarness {
        script: vec![
            AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "mock-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "native-initial".into(),
                assistant_message_id: "real-reply".into(),
            },
            AgentEvent::ToolCall {
                id: "tool".into(),
                call: ToolCall::WriteFile {
                    path: "file".into(),
                    content: Some("private source contents".into()),
                },
            },
            AgentEvent::TextDelta {
                text: response.clone(),
            },
            AgentEvent::ToolResult {
                id: "tool".into(),
                is_error: false,
                output: Some("saved".into()),
                diff: None,
            },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("native-final".into()),
            },
        ],
    };
    let (_, steering) = tokio::sync::mpsc::channel(1);
    let mut stream = harness
        .run(
            request,
            RunControls {
                steering,
                interrupt: Default::default(),
                host: Default::default(),
                request_input: Box::new(|_| panic!("no question in this script")),
            },
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        publication.observe(&event.unwrap()).unwrap();
    }
    publication.finish(Outcome::Completed).unwrap();
    assert!(
        !replica
            .read(|j| Ok(j.projection()?.executions[&occupancy].closed))
            .unwrap(),
        "turn completion cannot release the persistent process"
    );
    let head = replica.read(|j| j.cursor()).unwrap();
    publication
        .observe(&AgentEvent::TextDelta {
            text: "late raw observation".into(),
        })
        .unwrap();
    publication
        .observe(&AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: "/tmp".into(),
            session_id: "late-stale-native-id".into(),
            assistant_message_id: "real-reply".into(),
        })
        .unwrap();
    assert_eq!(
        replica.read(|j| j.cursor()).unwrap(),
        head,
        "late source cannot reopen the run"
    );
    let projection = replica.read(|j| j.projection()).unwrap();
    let public = serde_json::to_string(&projection).unwrap();
    assert!(!public.contains("private source contents"));
    assert!(!public.contains("late raw observation"));
    assert!(
        projection.messages.len() > 1,
        "real publisher must roll over large output"
    );
    let mut messages: Vec<_> = projection.messages.values().collect();
    messages.sort_by_key(|m| m.created_seq);
    let text: String = messages
        .iter()
        .flat_map(|m| m.entry.parts.iter())
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, response);
    replica.release_occupancy(&occupancy).unwrap();
    assert!(
        replica
            .read(|j| Ok(j.projection()?.executions[&occupancy].closed))
            .unwrap()
    );
    drop(publication);
    drop(replica);
    let reopened = SessionReplica::open_local(&path, "account", "room", "host").unwrap();
    assert_eq!(
        reopened
            .read(|j| j.harness_session("/tmp"))
            .unwrap()
            .as_deref(),
        Some("native-final")
    );
    assert_eq!(
        reopened
            .read(|j| j.harness_session("/another/cwd"))
            .unwrap(),
        None
    );
    assert!(matches!(
        reopened.advance("command").unwrap(),
        Progress::Settled
    ));
    assert_eq!(reopened.read(|j| j.cursor()).unwrap(), head + 1);
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

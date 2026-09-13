//! Real Engine replica → workerd → Swift smoke. Loopback and temporary data
//! only; no production accounts or paid harnesses.
use cypher_engine::{
    EdgeConfig,
    session_replica::{ExecutionPublication, SessionReplica},
};
use cypher_harness::{Harness, RunControls, mock::MockHarness};
use cypher_proto::{
    AgentEvent, DoneStatus, MessagePart, MessageRole, MessageStatus, SessionCommandPayload,
    SessionMessageEntry, ToolCall,
    sync3::{Event, Operation, Outcome},
};
use cypher_sync::sync3::{
    Phase,
    execution::{Plan, Progress},
};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    let base = &args[1];
    let url = reqwest::Url::parse(base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"), "smoke is loopback only");
    let room = &args[2];
    let report = &args[3];
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("host.sqlite");
    let replica = Arc::new(
        SessionReplica::open_synced(
            &path,
            r#"["sync3-org","sync3-user"]"#,
            "sync3-org",
            "sync3-user",
            room,
            "host",
            "host",
            EdgeConfig::with_static_token(base, "sync3-user@sync3-org").with_device("host"),
        )
        .await
        .unwrap(),
    );
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/sync3/golden.json")).unwrap();
    let mut op: Operation = serde_json::from_value(fixture["operations"][0].clone()).unwrap();
    op.actor = "host".into();
    if let Event::CommandQueued { command, .. } = &mut op.event {
        command.issued_by = "host".into();
        command.based_on = None;
        command.expires_at = None;
    }
    replica.enqueue(&op).unwrap();
    let mut status = replica.watch();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = status.borrow_and_update().clone();
            assert!(current.error.is_none(), "{current:?}");
            if current.cursor >= 1 && current.phase == Phase::Live {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    replica.prepare("command", Plan::Run).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            // Consume notification before advancing to avoid losing a commit
            // between the durable read and changed(). No client-forged ACK.
            status.borrow_and_update();
            match replica.advance("command").unwrap() {
                Progress::Dispatch(permit) => break permit,
                Progress::WaitingForRun | Progress::WaitingForClaim => {}
                _ => panic!("unexpected execution progress"),
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let occupancy = replica.occupy(&permit).await.unwrap();
    let SessionCommandPayload::Run { request, .. } = &permit.command().payload else {
        panic!("not run");
    };
    let mut request = request.clone();
    request.cwd = temp.path().to_string_lossy().into();
    request.attachments.clear();
    request.pending_attachments.clear();
    request.worktree = None;
    let mut publication = ExecutionPublication::new(
        replica.clone(),
        permit,
        &SessionMessageEntry {
            id: "engine-message".into(),
            role: MessageRole::Assistant,
            device_id: "host".into(),
            created_at: 1,
            parts: vec![],
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
        },
    )
    .unwrap();
    let response = "你好🙂\"\\\n".repeat(90_000);
    let harness = MockHarness {
        script: vec![
            AgentEvent::ToolCall {
                id: "tool".into(),
                call: ToolCall::WriteFile {
                    path: "mock-file".into(),
                    content: Some("private-source-must-not-replicate".into()),
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
                session_id: None,
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
                request_input: Box::new(|_| panic!("no input in this script")),
            },
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        publication.observe(&event.unwrap()).unwrap();
    }
    let run = publication.run_id().to_owned();
    publication.finish(Outcome::Completed).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = status.borrow_and_update().clone();
            assert!(current.error.is_none(), "{current:?}");
            if current.phase == Phase::Live
                && replica
                    .read(|j| Ok(j.projection()?.runs.get(&run).and_then(|r| r.outcome)))
                    .unwrap()
                    == Some(Outcome::Completed)
            {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        !replica
            .read(|j| Ok(j.projection()?.executions[&occupancy].closed))
            .unwrap()
    );
    // The MockHarness stream has ended. Reconcile and close its occupancy
    // separately from the semantic completion.
    replica.release_occupancy(&occupancy).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            status.borrow_and_update();
            if replica
                .read(|j| Ok(j.projection()?.executions[&occupancy].closed))
                .unwrap()
            {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let projection = replica.read(|j| j.projection()).unwrap();
    let encoded = serde_json::to_string(&projection).unwrap();
    assert!(!encoded.contains("private-source-must-not-replicate"));
    let mut messages: Vec<_> = projection.messages.values().collect();
    messages.sort_by_key(|m| m.created_seq);
    let mut parts = Vec::new();
    for message in &messages {
        assert_eq!(message.entry.status, Some(MessageStatus::Complete));
        for part in &message.entry.parts {
            if let MessagePart::Text { id, text } = part {
                if let Some(MessagePart::Text {
                    id: old,
                    text: tail,
                }) = parts.last_mut()
                {
                    if old == id {
                        tail.push_str(text);
                        continue;
                    }
                }
            }
            parts.push(part.clone());
        }
    }
    let text: String = parts
        .iter()
        .filter_map(|p| match p {
            MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, response);
    assert!(messages.len() > 3);
    let mut value = serde_json::to_value(parts).unwrap();
    value.sort_all_objects();
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
    std::fs::write(report, serde_json::to_vec(&serde_json::json!({
        "head": replica.read(|j| j.cursor()).unwrap(), "partsDigest": digest, "messages": messages.len()
    })).unwrap()).unwrap();
    assert_eq!(status.borrow().repairs, 0);
    drop(publication);
    drop(status);
    Arc::try_unwrap(replica).ok().unwrap().shutdown().await;
    let restarted = SessionReplica::open_synced(
        &path,
        r#"["sync3-org","sync3-user"]"#,
        "sync3-org",
        "sync3-user",
        room,
        "host",
        "host",
        EdgeConfig::with_static_token(base, "sync3-user@sync3-org").with_device("host"),
    )
    .await
    .unwrap();
    assert!(matches!(
        restarted.advance("command").unwrap(),
        Progress::Settled
    ));
    restarted.shutdown().await;
    println!(
        "PASS: Engine SessionReplica + MockHarness → workerd; private source, bounded publisher, committed fences, restart, healthy repairs=0"
    );
}

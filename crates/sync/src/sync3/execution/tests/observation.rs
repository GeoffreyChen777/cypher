use super::*;
use cypher_proto::{AgentEvent, MessagePart, MessageRole, MessageStatus, SessionMessageEntry};

#[test]
fn observed_publication_requires_retained_source_and_committed_occupancy_without_redispatch() {
    let mut j = host(Path::new(":memory:"));
    let dispatch = claim_run(&mut j);
    commit(
        &mut j,
        vec![Operation {
            id: "occupy".into(),
            actor: "host".into(),
            owner_epoch: 1,
            event: Event::ExecutionStarted {
                execution_id: "process".into(),
                command_id: "command".into(),
            },
        }],
    );
    j.append_execution_events(
        &dispatch,
        1,
        &[AgentEvent::TextDelta {
            text: "initial".into(),
        }],
    )
    .unwrap();
    j.complete_execution(
        &dispatch,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    deliver(&mut j);
    assert!(
        j.prepare_observation(&dispatch, "process", "observed", 2)
            .is_err(),
        "no invented source"
    );
    assert!(j.open_observations("", 32).unwrap().is_empty());
    j.append_execution_events(
        &dispatch,
        2,
        &[AgentEvent::TextDelta {
            text: "background".into(),
        }],
    )
    .unwrap();
    assert!(
        j.prepare_observation(&dispatch, "missing", "observed", 2)
            .is_err()
    );
    let observed = j
        .prepare_observation(&dispatch, "process", "observed", 2)
        .unwrap();
    let marker = j.pending().unwrap();
    assert_eq!(marker.len(), 1);
    assert!(matches!(marker[0].event, Event::RunObserved { .. }));
    let entry = SessionMessageEntry {
        id: "background".into(),
        role: MessageRole::Assistant,
        device_id: "host".into(),
        created_at: 2,
        parts: vec![],
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    };
    assert!(!j.observation_ready(&observed).unwrap());
    assert!(j.new_observation_writer(&observed, &entry).is_err());
    j.acknowledge(&Reply::Ack {
        version: 3,
        epoch: 1,
        receipts: vec![Receipt {
            id: marker[0].id.clone(),
            seq: j.cursor().unwrap() + 1,
        }],
    })
    .unwrap();
    assert!(
        !j.observation_ready(&observed).unwrap(),
        "ACK is not canonical application"
    );
    commit(&mut j, marker);
    assert!(j.observation_ready(&observed).unwrap());
    j.db.execute(
        "UPDATE sync3_observations SET first_seq=1 WHERE run_id='observed'",
        [],
    )
    .unwrap();
    assert!(
        j.observation_ready(&observed).is_err(),
        "indexed source identity must match its durable marker"
    );
    assert!(
        j.quarantine_observation("observed", "corrupt index")
            .is_err()
    );
    j.db.execute(
        "UPDATE sync3_observations SET first_seq=2 WHERE run_id='observed'",
        [],
    )
    .unwrap();
    assert!(
        j.prepare_observation(&dispatch, "process", "overlap", 2)
            .is_err()
    );
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Settled
    ));
    assert!(
        j.new_execution_writer(&dispatch, &entry).is_err(),
        "original dispatch cannot reopen"
    );
    let parts = vec![MessagePart::Text {
        id: "text".into(),
        text: "background".into(),
    }];
    let mut writer = j.new_observation_writer(&observed, &entry).unwrap();
    assert!(
        writer
            .sync(&parts, |frame| j
                .enqueue_observation_frame(&observed, 1, frame))
            .is_err()
    );
    while writer
        .sync(&parts, |frame| {
            j.enqueue_observation_frame(&observed, 2, frame)
        })
        .unwrap()
        .more
    {}
    assert!(
        j.complete_observation(&observed, 2, Outcome::Completed)
            .is_err(),
        "writer must be finalized"
    );
    while writer
        .finish(&parts, Some(MessageStatus::Complete), |frame| {
            j.enqueue_observation_frame(&observed, 2, frame)
        })
        .unwrap()
        .more
    {}
    j.complete_observation(&observed, 2, Outcome::Completed)
        .unwrap();
    let count = j.pending().unwrap().len();
    j.complete_observation(&observed, 2, Outcome::Completed)
        .unwrap();
    assert_eq!(count, j.pending().unwrap().len());
    assert!(
        j.complete_observation(&observed, 2, Outcome::Failed)
            .is_err()
    );
    deliver(&mut j);
    let p = j.projection().unwrap();
    assert_eq!(p.commands.len(), 1);
    assert_eq!(
        p.commands["command"].command.status,
        SessionCommandStatus::Applied
    );
    assert_eq!(p.runs.len(), 2);
    assert!(!p.executions["process"].closed);

    j.append_execution_events(
        &dispatch,
        3,
        &[AgentEvent::TextDelta {
            text: "another observation".into(),
        }],
    )
    .unwrap();
    let next = j
        .prepare_observation(&dispatch, "process", "another", 3)
        .unwrap();
    deliver(&mut j);
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 2,
        head: j.cursor().unwrap(),
    })
    .unwrap();
    assert!(
        j.new_observation_writer(&next, &entry).is_err(),
        "owner epoch fences observations too"
    );
    assert!(j.quarantine_observation("another", "wrong epoch").is_err());
    j.append_execution_events(
        &dispatch,
        4,
        &[AgentEvent::TextDelta {
            text: "retained after fence loss".into(),
        }],
    )
    .unwrap();
    assert_eq!(
        j.execution_events("command", 3, 32).unwrap().events.len(),
        1
    );
    assert!(j.pending().unwrap().is_empty());
}

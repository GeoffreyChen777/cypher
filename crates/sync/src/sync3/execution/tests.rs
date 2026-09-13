use super::*;
use cypher_proto::sync3::{Receipt, Reply, Row};
use std::path::Path;
mod observation;
mod source;

fn queued() -> Operation {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../../../fixtures/sync3/golden.json")).unwrap();
    serde_json::from_value(fixture["operations"][0].clone()).unwrap()
}
fn host(path: &Path) -> Journal {
    let mut j = Journal::open(path, "account", "room", "host").unwrap();
    commit(&mut j, vec![queued()]);
    j
}
fn commit(j: &mut Journal, operations: Vec<Operation>) {
    let base = j.cursor().unwrap();
    let head = base + operations.len() as u64;
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 1,
        head,
    })
    .unwrap();
    let rows = operations
        .into_iter()
        .enumerate()
        .map(|(i, operation)| Row {
            seq: base + i as u64 + 1,
            operation,
        })
        .collect();
    j.apply_page(&Reply::Page {
        version: 3,
        epoch: 1,
        through: head,
        next: head,
        done: true,
        rows,
    })
    .unwrap();
}
fn deliver(j: &mut Journal) {
    let operations = j.pending().unwrap();
    commit(j, operations);
}
fn claim_run(j: &mut Journal) -> DispatchPermit {
    j.prepare_execution("command", Plan::Run).unwrap();
    deliver(j);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::WaitingForRun
    ));
    deliver(j);
    let Progress::Dispatch(permit) = j.advance_execution("command").unwrap() else {
        panic!("no permit")
    };
    permit
}

#[test]
fn intent_precedes_claim_and_ack_is_not_permission_to_dispatch() {
    let mut j = host(Path::new(":memory:"));
    let first = j.prepare_execution("command", Plan::Run).unwrap();
    assert_eq!(first.state, State::Prepared);
    let ops = j.pending().unwrap();
    assert_eq!(ops.len(), 1);
    assert!(matches!(ops[0].event, Event::CommandClaimAttempted { .. }));
    assert_eq!(j.prepare_execution("command", Plan::Run).unwrap(), first);
    assert_eq!(j.pending().unwrap(), ops);
    j.acknowledge(&Reply::Ack {
        version: 3,
        epoch: 1,
        receipts: vec![Receipt {
            id: ops[0].id.clone(),
            seq: 2,
        }],
    })
    .unwrap();
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::WaitingForClaim
    ));
    commit(&mut j, ops);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::WaitingForRun
    ));
    let starts = j.pending().unwrap();
    assert_eq!(starts.len(), 1);
    assert!(matches!(starts[0].event, Event::RunStarted { .. }));
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::WaitingForRun
    ));
    assert_eq!(j.pending().unwrap(), starts);
    deliver(&mut j);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Dispatch(_)
    ));
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Claimed
    );
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::RecoveryRequired
    ));
}

#[test]
fn crash_before_permit_can_resume_but_lost_permit_can_never_reexecute() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.sqlite");
    let mut j = host(&path);
    let info = j.prepare_execution("command", Plan::Run).unwrap();
    let ops = j.pending().unwrap();
    drop(j);
    let mut j = Journal::open(&path, "account", "room", "host").unwrap();
    assert_eq!(j.prepare_execution("command", Plan::Run).unwrap(), info);
    assert_eq!(j.pending().unwrap(), ops);
    deliver(&mut j);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::WaitingForRun
    ));
    deliver(&mut j);
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Dispatch(_)
    ));
    drop(j); // includes death immediately BEFORE the external call, or after it
    let mut j = Journal::open(&path, "account", "room", "host").unwrap();
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::RecoveryRequired
    ));
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Claimed
    );
    assert!(j.pending().unwrap().is_empty());
}

#[test]
fn competing_local_intents_with_the_same_actor_and_target_have_one_winner() {
    let mut a = host(Path::new(":memory:"));
    let mut b = host(Path::new(":memory:"));
    // A shared live run proves run-ID equality cannot serve as dispatch proof.
    let run = claim_run(&mut a).run_id().to_owned();
    let base =
        a.db.prepare("SELECT operation FROM sync3_events WHERE seq>1 ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect::<Vec<_>>();
    commit(&mut b, base);
    let mut control = queued();
    control.id = "control-queued".into();
    if let Event::CommandQueued {
        command_id,
        command,
    } = &mut control.event
    {
        *command_id = "control".into();
        command.id = "control".into();
        command.payload = cypher_proto::SessionCommandPayload::Interrupt {};
    }
    commit(&mut a, vec![control.clone()]);
    commit(&mut b, vec![control]);
    let plan = Plan::Control { run_id: Some(run) };
    a.prepare_execution("control", plan.clone()).unwrap();
    b.prepare_execution("control", plan).unwrap();
    let win = a.pending().unwrap()[0].clone();
    let lose = b.pending().unwrap()[0].clone();
    assert_ne!(win.id, lose.id);
    assert_eq!(win.event, lose.event);
    commit(&mut a, vec![win.clone(), lose.clone()]);
    commit(&mut b, vec![win, lose]);
    assert!(matches!(
        a.advance_execution("control").unwrap(),
        Progress::Dispatch(_)
    ));
    assert!(matches!(
        b.advance_execution("control").unwrap(),
        Progress::Declined
    ));
    assert_eq!(
        b.execution_info("control").unwrap().unwrap().state,
        State::Declined
    );
    assert!(b.pending().unwrap().is_empty());
}

#[test]
fn cancellation_before_claim_is_durable_decline_not_execution() {
    let mut j = host(Path::new(":memory:"));
    j.prepare_execution("command", Plan::Run).unwrap();
    let claim = j.pending().unwrap()[0].clone();
    commit(
        &mut j,
        vec![
            Operation {
                id: "cancel".into(),
                actor: "phone".into(),
                owner_epoch: 1,
                event: Event::CommandCancelAttempted {
                    command_id: "command".into(),
                },
            },
            claim,
        ],
    );
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Declined
    ));
    assert!(j.pending().unwrap().is_empty());
    assert!(j.projection().unwrap().runs.is_empty());
}

#[test]
fn accepted_history_without_local_intent_is_never_treated_as_a_new_dispatch() {
    let mut j = host(Path::new(":memory:"));
    commit(
        &mut j,
        vec![Operation {
            id: "another-process".into(),
            actor: "host".into(),
            owner_epoch: 1,
            event: Event::CommandClaimAttempted {
                command_id: "command".into(),
                run_id: "run".into(),
            },
        }],
    );
    assert!(j.prepare_execution("command", Plan::Run).is_err());
    assert!(j.execution_info("command").unwrap().is_none());
}

#[test]
fn completion_and_terminal_outbox_are_atomic_and_exactly_retryable() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    let ops = j.pending().unwrap();
    assert_eq!(ops.len(), 2);
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Settled
    );
    let changes = j.db.total_changes();
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    assert_eq!(j.db.total_changes(), changes);
    assert_eq!(j.pending().unwrap(), ops);
    assert!(
        j.complete_execution(
            &permit,
            Some(Outcome::Failed),
            SessionCommandStatus::Applied,
            None
        )
        .is_err()
    );
    deliver(&mut j);
    assert_eq!(
        j.projection().unwrap().runs[permit.run_id()].outcome,
        Some(Outcome::Completed)
    );
    assert!(matches!(
        j.advance_execution("command").unwrap(),
        Progress::Settled
    ));
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    assert!(j.pending().unwrap().is_empty());
}

#[test]
fn terminal_conflict_does_not_partially_finish_or_erase_uncertainty() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let poison = permit.intent.operation(
        "resolve",
        Event::CommandResolved {
            command_id: "command".into(),
            status: SessionCommandStatus::Rejected,
            resolution: Some("other".into()),
        },
    );
    j.enqueue(&poison).unwrap();
    assert!(
        j.complete_execution(
            &permit,
            Some(Outcome::Completed),
            SessionCommandStatus::Applied,
            None
        )
        .is_err()
    );
    assert_eq!(j.pending().unwrap(), vec![poison]);
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Claimed
    );
}

#[test]
fn owner_epoch_changes_and_foreign_scope_results_fail_closed() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let mut foreign = Journal::open(Path::new(":memory:"), "other", "room", "host").unwrap();
    assert!(
        foreign
            .complete_execution(
                &permit,
                Some(Outcome::Completed),
                SessionCommandStatus::Applied,
                None
            )
            .is_err()
    );
    assert!(foreign.pending().unwrap().is_empty());
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 2,
        head: 3,
    })
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
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Claimed
    );
}

#[test]
fn unsynced_and_non_owner_journals_cannot_prepare_execution() {
    let mut empty = Journal::open(Path::new(":memory:"), "account", "room", "host").unwrap();
    assert!(empty.prepare_execution("command", Plan::Run).is_err());
    let mut viewer = Journal::open(Path::new(":memory:"), "account", "room", "phone").unwrap();
    commit(&mut viewer, vec![queued()]);
    assert!(viewer.prepare_execution("command", Plan::Run).is_err());
    assert!(viewer.pending().unwrap().is_empty());
}

#[test]
fn missing_receipts_never_silently_reseed_claim_or_run_start() {
    for after_claim in [false, true] {
        let mut j = host(Path::new(":memory:"));
        j.prepare_execution("command", Plan::Run).unwrap();
        if after_claim {
            deliver(&mut j);
            assert!(matches!(
                j.advance_execution("command").unwrap(),
                Progress::WaitingForRun
            ));
        }
        j.db.execute("DELETE FROM sync3_outbox", []).unwrap();
        assert!(j.advance_execution("command").is_err());
        assert!(j.pending().unwrap().is_empty());
        assert_eq!(
            j.execution_info("command").unwrap().unwrap().state,
            State::Prepared
        );
    }
}

#[test]
fn prepared_intent_cannot_follow_a_changed_owner_epoch() {
    let mut j = host(Path::new(":memory:"));
    j.prepare_execution("command", Plan::Run).unwrap();
    j.accept_state(&Reply::State {
        version: 3,
        epoch: 1,
        owner: "host".into(),
        owner_epoch: 2,
        head: 1,
    })
    .unwrap();
    assert!(j.advance_execution("command").is_err());
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Prepared
    );
}

#[test]
fn prepare_failure_rolls_back_claim_outbox_together_with_intent() {
    let mut j = host(Path::new(":memory:"));
    j.db.execute_batch(
        "CREATE TEMP TRIGGER fail_intent BEFORE INSERT ON sync3_execution_intents
        BEGIN SELECT RAISE(ABORT,'test failure'); END;",
    )
    .unwrap();
    assert!(j.prepare_execution("command", Plan::Run).is_err());
    assert!(j.pending().unwrap().is_empty());
    assert!(j.execution_info("command").unwrap().is_none());
    j.db.execute_batch("DROP TRIGGER fail_intent").unwrap();
    j.prepare_execution("command", Plan::Run).unwrap();
    assert_eq!(j.pending().unwrap().len(), 1);
}

#[test]
fn run_cannot_bypass_run_fence_as_control_but_idle_interrupt_needs_no_run() {
    let mut j = host(Path::new(":memory:"));
    assert!(
        j.prepare_execution("command", Plan::Control { run_id: None })
            .is_err()
    );
    assert!(j.pending().unwrap().is_empty());
    let mut command = queued();
    command.id = "idle-interrupt".into();
    if let Event::CommandQueued {
        command_id,
        command,
    } = &mut command.event
    {
        *command_id = "interrupt".into();
        command.id = "interrupt".into();
        command.payload = cypher_proto::SessionCommandPayload::Interrupt {};
    }
    commit(&mut j, vec![command]);
    j.prepare_execution("interrupt", Plan::Control { run_id: None })
        .unwrap();
    deliver(&mut j);
    let Progress::Dispatch(permit) = j.advance_execution("interrupt").unwrap() else {
        panic!("no permit")
    };
    j.complete_execution(&permit, None, SessionCommandStatus::Applied, None)
        .unwrap();
    assert_eq!(j.pending().unwrap().len(), 1);
    deliver(&mut j);
    assert!(j.projection().unwrap().runs.is_empty());
}

#[test]
fn unfinished_producer_cannot_be_stranded_behind_terminal_run_outbox() {
    let mut j = host(Path::new(":memory:"));
    let permit = claim_run(&mut j);
    let entry = cypher_proto::SessionMessageEntry {
        id: "reply".into(),
        role: cypher_proto::MessageRole::Assistant,
        created_at: 1,
        device_id: "host".into(),
        status: Some(cypher_proto::MessageStatus::Streaming),
        continuation_of: None,
        parts: vec![],
    };
    let parts = [cypher_proto::MessagePart::Text {
        id: "text".into(),
        text: "retained".into(),
    }];
    let mut writer = j
        .new_writer(1, Some(permit.run_id().into()), &entry)
        .unwrap();
    writer
        .sync(&parts, |frame| j.enqueue_writer_frame(frame))
        .unwrap();
    let before = j.pending().unwrap();
    assert!(
        j.complete_execution(
            &permit,
            Some(Outcome::Completed),
            SessionCommandStatus::Applied,
            None
        )
        .is_err()
    );
    assert_eq!(j.pending().unwrap(), before);
    assert_eq!(
        j.execution_info("command").unwrap().unwrap().state,
        State::Claimed
    );
    while writer
        .finish(
            &parts,
            Some(cypher_proto::MessageStatus::Complete),
            |frame| j.enqueue_writer_frame(frame),
        )
        .unwrap()
        .more
    {}
    j.complete_execution(
        &permit,
        Some(Outcome::Completed),
        SessionCommandStatus::Applied,
        None,
    )
    .unwrap();
    deliver(&mut j);
    assert_eq!(
        j.projection().unwrap().runs[permit.run_id()].outcome,
        Some(Outcome::Completed)
    );
}

//! Subagent live projection (pi `cypher.subagents.v1`): `AgentEvent::SubagentStatus`
//! must update the chat's SESSION projection — subagents + updated_at — without
//! touching the transcript doc, the run status, `started_at`, or the parked-session
//! lifecycle. A background subagent finishing must never re-arm a parked session
//! as Working.

mod common;

use std::time::Duration;

use cypher_doc::{MessageRole, SessionMessageEntry};
use cypher_engine::EngineCore;
use cypher_proto::{
    AgentEvent, ContextUsage, DoneStatus, HarnessId, Session, SessionStatus, SubagentRun,
    SubagentRunMode, SubagentRunStatus, Throughput, ToolCall,
};

use common::{FeedRig, run_request, status, wait_for};

const CHAT: &str = "chat-subagents";

fn session_started() -> AgentEvent {
    common::session_started(HarnessId::Mock, "mock-1", "/tmp", "hs-sa", "a-sa")
}

fn subagent_tool_call() -> AgentEvent {
    AgentEvent::ToolCall {
        id: "t1".into(),
        call: ToolCall::Unknown {
            name: "subagent".into(),
            input: Some(serde_json::json!({
                "agent": "planner",
                "task": "Plan the panel",
                "async": true,
            })),
        },
    }
}

fn running_async() -> AgentEvent {
    run_status(SubagentRunStatus::Running, false)
}

/// One async run event with an arbitrary status (settled runs carry ended_at).
fn run_status(status: SubagentRunStatus, settled: bool) -> AgentEvent {
    AgentEvent::SubagentStatus {
        runs: vec![SubagentRun {
            run_id: "run-1".into(),
            tool_call_id: Some("t1".into()),
            agent: "planner".into(),
            model: Some("anthropic/claude-sonnet-4".into()),
            task: "Plan the panel".into(),
            mode: SubagentRunMode::Async,
            status,
            progress: Some("live tail".into()),
            started_at: 1000,
            updated_at: 2000,
            ended_at: if settled { Some(2000) } else { None },
            child_chat_id: None,
        }],
    }
}

fn clear() -> AgentEvent {
    AgentEvent::SubagentStatus { runs: vec![] }
}

fn assemble(main_prompt: &str) -> FeedRig {
    common::feed_rig(main_prompt, false)
}

fn started_at(core: &EngineCore) -> Option<chrono::DateTime<chrono::Utc>> {
    core.sessions
        .session_status(CHAT)
        .and_then(|s| s.started_at)
}

fn subagents(core: &EngineCore) -> Vec<SubagentRun> {
    core.sessions
        .session_status(CHAT)
        .map(|s| s.subagents)
        .unwrap_or_default()
}

fn assistant_entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    common::entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .collect()
}

/// After a parked (Done) session, a SubagentStatus snapshot must update the
/// live projection while leaving EVERYTHING else alone: status stays Idle,
/// `started_at` stays cleared, the transcript gains no entry, and the clear
/// snapshot empties the projection. The workspace registry row mirrors it.
#[tokio::test]
async fn subagent_status_updates_projection_without_polluting_run_state() {
    let rig = assemble("spawn a background planner");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("spawn a background planner"),
            None,
        )
        .await
        .expect("dispatch");

    // A full turn: subagent tool call + progress + resolve, then Done parks.
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(subagent_tool_call()).unwrap();
    rig.feed
        .send(AgentEvent::ToolProgress {
            id: "t1".into(),
            output: "drafting…".into(),
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::ToolResult {
            id: "t1".into(),
            is_error: false,
            output: None,
            diff: None,
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    let before_entries = assistant_entries(&rig.core).len();
    let before_started = started_at(&rig.core);
    assert_eq!(before_started, None, "parked session has no turn base");

    // A live async snapshot arrives (background subagent still running).
    rig.feed.send(running_async()).unwrap();
    wait_for(
        || subagents(&rig.core).len() == 1,
        "projection picks up the running run",
    )
    .await;

    // Status, started_at and transcript are untouched.
    assert_eq!(
        status(&rig.core, CHAT),
        Some(SessionStatus::Idle),
        "SubagentStatus must not re-arm a parked session"
    );
    assert_eq!(
        started_at(&rig.core),
        before_started,
        "started_at must not change"
    );
    assert_eq!(
        assistant_entries(&rig.core).len(),
        before_entries,
        "SubagentStatus must not fold a transcript entry"
    );

    // The projection is visible through the watch channel too (the UI's
    // WatchSessions source).
    let watched = rig
        .core
        .sessions
        .watch_sessions()
        .borrow()
        .iter()
        .find(|s| s.chat_id == CHAT)
        .cloned();
    assert_eq!(watched.map(|s| s.subagents.len()), Some(1));

    // And it mirrored into the workspace registry row.
    let rows = rig.core.workspace.read_sessions().unwrap();
    let row = rows.iter().find(|s| s.chat_id == CHAT).expect("row");
    assert_eq!(row.subagents.len(), 1);
    assert_eq!(row.subagents[0].agent, "planner");
    assert_eq!(row.subagents[0].status, SubagentRunStatus::Running);

    // Clear snapshot empties the projection; still no lifecycle change.
    rig.feed.send(clear()).unwrap();
    wait_for(
        || subagents(&rig.core).is_empty(),
        "clear empties the projection",
    )
    .await;
    assert_eq!(status(&rig.core, CHAT), Some(SessionStatus::Idle));
    assert_eq!(started_at(&rig.core), before_started);
    assert_eq!(assistant_entries(&rig.core).len(), before_entries);
    let rows = rig.core.workspace.read_sessions().unwrap();
    assert!(
        rows.iter()
            .find(|s| s.chat_id == CHAT)
            .unwrap()
            .subagents
            .is_empty(),
        "registry row clears too"
    );

    rig.core.sessions.shutdown().await;
}

/// The context gauge (`AgentEvent::ContextUsage`) is the same kind of live
/// projection: after a parked turn it lands on the session row without
/// re-arming the session, restamping its freshness, or folding a transcript
/// entry — and, the row being settled, it is written to the synced registry
/// so other devices' rings see it.
#[tokio::test]
async fn context_usage_on_a_parked_row_syncs_without_polluting_run_state() {
    let rig = assemble("measure the context");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("measure the context"),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed
        .send(AgentEvent::TextDelta {
            text: "done".into(),
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    let before_entries = assistant_entries(&rig.core).len();
    let before_updated = rig.core.sessions.session_status(CHAT).map(|s| s.updated_at);

    // Claude re-reports the gauge after a compaction, turn already parked.
    rig.feed
        .send(AgentEvent::ContextUsage {
            used: 32_000,
            size: 200_000,
        })
        .unwrap();
    let expected = Some(ContextUsage {
        used: 32_000,
        size: 200_000,
    });
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(CHAT)
                .and_then(|s| s.context_usage)
                == expected
        },
        "gauge lands on the session row",
    )
    .await;

    assert_eq!(status(&rig.core, CHAT), Some(SessionStatus::Idle));
    assert_eq!(started_at(&rig.core), None);
    assert_eq!(
        rig.core.sessions.session_status(CHAT).map(|s| s.updated_at),
        before_updated,
        "the gauge is not a liveness signal"
    );
    assert_eq!(assistant_entries(&rig.core).len(), before_entries);

    let watched = rig
        .core
        .sessions
        .watch_sessions()
        .borrow()
        .iter()
        .find(|s| s.chat_id == CHAT)
        .and_then(|s| s.context_usage);
    assert_eq!(watched, expected, "WatchSessions carries the gauge");

    let rows = rig.core.workspace.read_sessions().unwrap();
    let row = rows.iter().find(|s| s.chat_id == CHAT).expect("row");
    assert_eq!(
        row.context_usage, expected,
        "a settled reading reaches the registry"
    );
    assert_eq!(row.status, SessionStatus::Idle);
    assert_eq!(
        Some(row.updated_at.timestamp_millis()),
        before_updated.map(|at| at.timestamp_millis()),
        "the synced row keeps its freshness too"
    );

    rig.core.sessions.shutdown().await;
}

/// Mid-turn readings stay off the registry (Claude reports one per message
/// delta, Pi one per assistant message); the settle transition's own write
/// carries the latest one to other devices.
#[tokio::test]
async fn context_usage_mid_turn_rides_the_settle_write() {
    let rig = assemble("measure mid turn");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("measure mid turn"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "turn running",
    )
    .await;
    let reading = |used| ContextUsage {
        used,
        size: 200_000,
    };
    for used in [40_000, 41_000] {
        rig.feed
            .send(AgentEvent::ContextUsage {
                used,
                size: 200_000,
            })
            .unwrap();
    }
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(CHAT)
                .and_then(|s| s.context_usage)
                == Some(reading(41_000))
        },
        "latest reading on the local row",
    )
    .await;
    let registry_usage = || {
        rig.core
            .workspace
            .read_sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.chat_id == CHAT)
            .and_then(|s| s.context_usage)
    };
    assert_eq!(registry_usage(), None, "mid-turn readings stay local");

    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    assert_eq!(
        registry_usage(),
        Some(reading(41_000)),
        "the settle write carries the latest reading"
    );

    rig.core.sessions.shutdown().await;
}

/// Throughput (pi `cypher.throughput.v1`) is the working trailer's tok/s:
/// live on this engine's own `WatchSessions`, never in the transcript, on the
/// registry only when the row is next written for its own reasons, and gone
/// everywhere once the turn settles.
#[tokio::test]
async fn throughput_rides_the_next_row_write_and_ends_with_the_turn() {
    let rig = assemble("stream fast");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("stream fast"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "turn running",
    )
    .await;
    let before_entries = assistant_entries(&rig.core).len();
    let reading = Throughput {
        tokens_per_second: Some(52),
        average_tokens_per_second: Some(48),
        output_tokens: 3_400,
        sampled_at: chrono::Utc::now(),
    };
    rig.feed
        .send(AgentEvent::Throughput {
            throughput: Some(reading),
        })
        .unwrap();
    wait_for(
        || {
            rig.core
                .sessions
                .watch_sessions()
                .borrow()
                .iter()
                .any(|s| s.chat_id == CHAT && s.throughput == Some(reading))
        },
        "WatchSessions carries the reading",
    )
    .await;
    assert_eq!(status(&rig.core, CHAT), Some(SessionStatus::Working));
    assert_eq!(assistant_entries(&rig.core).len(), before_entries);
    let registry_throughput = || {
        rig.core
            .workspace
            .read_sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.chat_id == CHAT)
            .and_then(|s| s.throughput)
    };
    assert_eq!(
        registry_throughput(),
        None,
        "a reading alone writes nothing"
    );

    // Any write of the row (here a subagent snapshot) carries the latest one.
    rig.feed.send(running_async()).unwrap();
    wait_for(
        || registry_throughput() == Some(reading),
        "the next row write carries the reading",
    )
    .await;

    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    assert_eq!(
        rig.core
            .sessions
            .session_status(CHAT)
            .and_then(|s| s.throughput),
        None,
        "a settled turn drops its tok/s"
    );
    assert_eq!(registry_throughput(), None, "the settle write clears it");

    rig.core.sessions.shutdown().await;
}

/// During a LIVE turn, SubagentStatus still never folds into the transcript:
/// it updates the projection only, and the run keeps its Working status.
#[tokio::test]
async fn subagent_status_mid_turn_does_not_fold() {
    let rig = assemble("delegate the build");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("delegate the build"),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed
        .send(AgentEvent::TextDelta {
            text: "Delegating…".into(),
        })
        .unwrap();
    rig.feed.send(running_async()).unwrap();
    rig.feed.send(clear()).unwrap();
    wait_for(
        || {
            subagents(&rig.core).is_empty()
                && status(&rig.core, CHAT) == Some(SessionStatus::Working)
        },
        "mid-turn projection updates while Working",
    )
    .await;

    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "turn completes and parks",
    )
    .await;
    // Only the one text delta entry exists — no SubagentStatus got folded.
    let texts: Vec<String> = assistant_entries(&rig.core)
        .iter()
        .flat_map(|e| e.parts.iter())
        .filter_map(|p| match p {
            cypher_doc::MessagePart::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Delegating…".to_string()]);

    rig.core.sessions.shutdown().await;
}

/// Test 1 — while the parked (Done) stream stays OPEN, a running snapshot
/// keeps the projection Running and the main session stays Idle. The legal
/// background subagent is alive on the open stream; no terminalize fires.
#[tokio::test]
async fn parked_stream_open_keeps_running_and_main_idle() {
    let rig = assemble("parked background");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("parked background"),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(running_async()).unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle) && subagents(&rig.core).len() == 1,
        "parked with the running projection",
    )
    .await;
    assert_eq!(
        subagents(&rig.core)[0].status,
        SubagentRunStatus::Running,
        "parked stream open ⇒ the subagent stays Running"
    );
    // A further heartbeat snapshot keeps it Running, still Idle.
    rig.feed.send(running_async()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        subagents(&rig.core)[0].status,
        SubagentRunStatus::Running,
        "heartbeat on the open parked stream keeps Running"
    );
    assert_eq!(status(&rig.core, CHAT), Some(SessionStatus::Idle));
    assert_eq!(
        subagents(&rig.core)[0].ended_at,
        None,
        "running has no ended_at"
    );

    rig.core.sessions.shutdown().await;
}

/// Test 2 — the parked stream ENDS (the harness owner is gone): the residual
/// Running projection terminalizes to Error with ended_at, while the main
/// session stays Idle.
#[tokio::test]
async fn parked_stream_end_terminalizes_running() {
    let rig = assemble("parked end");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("parked end"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(running_async()).unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle) && subagents(&rig.core).len() == 1,
        "parked with the running projection",
    )
    .await;
    assert_eq!(subagents(&rig.core)[0].status, SubagentRunStatus::Running);

    // Drop the feed: the parked stream EOFs and the run's owner ends.
    let FeedRig {
        feed,
        core,
        dir: _dir,
    } = rig;
    drop(feed);
    wait_for(
        || subagents(&core)[0].status == SubagentRunStatus::Error,
        "owner death terminalizes the residual running projection",
    )
    .await;
    assert_eq!(
        status(&core, CHAT),
        Some(SessionStatus::Idle),
        "owner death must not change the main session status"
    );
    let run = &subagents(&core)[0];
    assert_eq!(run.status, SubagentRunStatus::Error);
    assert_eq!(run.ended_at, Some(run.updated_at), "ended_at stamped");

    core.sessions.shutdown().await;
}

/// Test 3 — an ACTIVE (Working) stream ends mid-turn: the residual Running
/// projection terminalizes too, and the session reads Errored.
#[tokio::test]
async fn active_stream_end_terminalizes_running() {
    let rig = assemble("active end");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("active end"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(running_async()).unwrap();
    wait_for(
        || subagents(&rig.core).len() == 1,
        "running projection during the live turn",
    )
    .await;
    assert_eq!(subagents(&rig.core)[0].status, SubagentRunStatus::Running);

    // Mid-turn stream EOF (not parked, not interrupted) = a crash.
    let FeedRig {
        feed,
        core,
        dir: _dir,
    } = rig;
    drop(feed);
    wait_for(
        || subagents(&core)[0].status == SubagentRunStatus::Error,
        "active owner death terminalizes the residual running projection",
    )
    .await;
    assert_eq!(
        status(&core, CHAT),
        Some(SessionStatus::Errored),
        "active stream end is a crash for the main session"
    );
    assert_eq!(
        subagents(&core)[0].ended_at,
        Some(subagents(&core)[0].updated_at)
    );

    core.sessions.shutdown().await;
}

/// Test 4 — owner death only touches Running: settled Done/Error runs survive
/// exactly as they were.
#[tokio::test]
async fn settled_runs_survive_owner_death() {
    let rig = assemble("settled survival");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("settled survival"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed
        .send(run_status(SubagentRunStatus::Done, true))
        .unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("hs-sa".into()),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after settled snapshot",
    )
    .await;
    assert_eq!(subagents(&rig.core)[0].status, SubagentRunStatus::Done);

    let FeedRig {
        feed,
        core,
        dir: _dir,
    } = rig;
    drop(feed);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        subagents(&core)[0].status,
        SubagentRunStatus::Done,
        "settled Done survives owner death"
    );

    core.sessions.shutdown().await;
}

/// Test 5 — boot recovery: THIS device's durable Running rows terminalize to
/// Error; a remote device's rows are left untouched (their owner may be live).
#[tokio::test]
async fn boot_recovery_terminalizes_local_but_not_remote_rows() {
    let rig = assemble("recovery");
    let now = chrono::Utc::now();
    let run = |run_id: &str| SubagentRun {
        run_id: run_id.into(),
        tool_call_id: Some("t1".into()),
        agent: "planner".into(),
        model: None,
        task: "Plan the panel".into(),
        mode: SubagentRunMode::Async,
        status: SubagentRunStatus::Running,
        progress: Some("thinking…".into()),
        started_at: 1000,
        updated_at: 2000,
        ended_at: None,
        child_chat_id: None,
    };
    let local = Session {
        chat_id: "chat-local".into(),
        device_id: rig.core.device_id.clone(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: now,
        subagents: vec![run("r-local")],
        context_usage: None,
        throughput: None,
    };
    let remote = Session {
        chat_id: "chat-remote".into(),
        device_id: "remote-device".into(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: now,
        subagents: vec![run("r-remote")],
        context_usage: None,
        throughput: None,
    };
    rig.core.workspace.record_session(&local);
    rig.core.workspace.record_session(&remote);

    let fixed = rig
        .core
        .sessions
        .recover_orphaned_subagents()
        .expect("sweep");
    assert_eq!(fixed, 1, "only the local row's running run is terminalized");

    let rows = rig.core.workspace.read_sessions().unwrap();
    let l = rows.iter().find(|s| s.chat_id == "chat-local").unwrap();
    assert_eq!(l.subagents[0].status, SubagentRunStatus::Error);
    assert_eq!(l.subagents[0].ended_at, Some(l.subagents[0].updated_at));
    let r = rows.iter().find(|s| s.chat_id == "chat-remote").unwrap();
    assert_eq!(
        r.subagents[0].status,
        SubagentRunStatus::Running,
        "remote device rows are untouched"
    );
    assert_eq!(r.subagents[0].ended_at, None);

    rig.core.sessions.shutdown().await;
}

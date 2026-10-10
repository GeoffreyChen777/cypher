//! Turn-quiesce watchdog and parked self-continuation (2026-08-12 and
//! 2026-08-13 stuck-Working incidents): a parked session resumes on
//! self-continued output, a turn whose Done is lost settles once the stream
//! goes silent with nothing in flight, and a self-started turn — which never
//! gets a Done — settles on the shorter self-turn window.

mod common;

use std::time::Duration;

use cypher_doc::{MessagePart, MessageRole, MessageStatus};
use cypher_engine::{EngineCore, QuiesceWindows, SteerOutcome};
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SessionStatus, ToolCall};

use common::{FeedRig, entries, run_request, status, text, wait_for};

const CHAT: &str = "chat-quiesce";
/// Watchdog window for the prompt-turn tests.
const QUIESCE_MS: u64 = 300;
/// The self-turn tests: a normal window far beyond the test horizon, so a
/// fast park can only have come through the short self-continued window.
const LONG_QUIESCE_MS: u64 = 600_000;
const SELF_QUIESCE_MS: u64 = 400;

fn done(status: DoneStatus) -> AgentEvent {
    common::done_with(status, Some("hs-q"))
}

fn session_started() -> AgentEvent {
    common::session_started(HarnessId::Mock, "mock-1", "/tmp", "hs-q", "a-q")
}

/// The feed models turn boundaries, self-continuation, and a LOST turn-end
/// exactly; accepted steers confirm with a `Steered` boundary. The default
/// 20s self-turn window is longer than `QUIESCE_MS`, so the normal one governs.
fn assemble(main_prompt: &str) -> FeedRig {
    assemble_with(
        main_prompt,
        QuiesceWindows {
            turn: Some(Duration::from_millis(QUIESCE_MS)),
            self_turn: Some(Duration::from_secs(20)),
        },
    )
}

/// The self-turn rig: only the short self-continued window can park in time.
fn assemble_self_turn(main_prompt: &str) -> FeedRig {
    assemble_with(
        main_prompt,
        QuiesceWindows {
            turn: Some(Duration::from_millis(LONG_QUIESCE_MS)),
            self_turn: Some(Duration::from_millis(SELF_QUIESCE_MS)),
        },
    )
}

fn assemble_with(main_prompt: &str, windows: QuiesceWindows) -> FeedRig {
    let rig = common::feed_rig(main_prompt, true);
    rig.core.sessions.set_quiesce_windows(windows);
    rig
}

fn assistant_texts(core: &EngineCore) -> Vec<(String, Option<MessageStatus>)> {
    entries(core, CHAT)
        .into_iter()
        .filter(|e| e.role == MessageRole::Assistant)
        .map(|e| {
            let text = e
                .parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            (text, e.status)
        })
        .collect()
}

/// Steps 1+2 of the incident: a completed turn parks the session; the agent
/// then self-continues (background-task re-invocation) with no prompt. The
/// output must land in the transcript (it used to be dropped), the session
/// must read Working while it streams, and — with no Done ever coming for a
/// self-started turn — the watchdog must settle it back to Idle.
#[tokio::test]
async fn parked_self_continuation_folds_and_requiesces() {
    let rig = assemble("pull waku and benchmark it");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("pull waku and benchmark it"),
            None,
        )
        .await
        .expect("dispatch");

    // Turn 1 completes normally → parked Idle.
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Still going, and healthy.")).unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // Self-continuation: streamed output with NO turn behind it, arriving
    // well past the resume gate (a real one follows a whole agent round
    // trip — the incident's came five minutes after the park).
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.feed
        .send(text("Build finished successfully. Launching Waku."))
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "parked session resumes Working on self-continued output",
    )
    .await;

    // No Done will ever come for a self-started turn: the watchdog parks it.
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "watchdog re-parks the self-continued turn",
    )
    .await;

    // The self-continued output is in the doc as its own COMPLETE entry —
    // this exact text was lost in the incident.
    let texts = assistant_texts(&rig.core);
    assert!(
        texts.iter().any(|(t, s)| {
            t.contains("Build finished successfully") && *s == Some(MessageStatus::Complete)
        }),
        "self-continued output must fold into a complete transcript entry, got {texts:#?}"
    );

    rig.core.sessions.shutdown().await;
}

/// Step 3 of the incident: a steer becomes the next turn, the agent answers,
/// and the turn-end reply is LOST. The session must not read Working forever
/// — the watchdog settles it, and the answer text survives in the doc.
#[tokio::test]
async fn missing_turn_end_settles_instead_of_working_forever() {
    let rig = assemble("pull waku and benchmark it");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("pull waku and benchmark it"),
            None,
        )
        .await
        .expect("dispatch");

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Cloned and building.")).unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // "what about now" — routed into the live run's mailbox; the harness
    // confirms it with a Steered boundary, which re-arms Working.
    let outcome = rig
        .core
        .sessions
        .steer(CHAT, "what about now", None)
        .await
        .expect("steer");
    assert!(matches!(outcome, SteerOutcome::Accepted));
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "steer boundary re-arms Working",
    )
    .await;

    // The agent answers… and its Done is lost upstream. Nothing else comes.
    rig.feed.send(text("Done — here are the results.")).unwrap();

    // Old behavior: Working forever (heartbeat keeps the row fresh; no
    // turn timeout). New behavior: the watchdog settles the turn.
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "watchdog settles the turn whose Done was lost",
    )
    .await;
    let texts = assistant_texts(&rig.core);
    assert!(
        texts.iter().any(|(t, s)| {
            t.contains("here are the results") && *s == Some(MessageStatus::Complete)
        }),
        "the lost-Done turn's answer must still finalize in the doc, got {texts:#?}"
    );

    rig.core.sessions.shutdown().await;
}

/// The guard the design comment insists on: a long-running tool call is
/// legitimately silent for minutes — an unresolved tool part must hold the
/// watchdog off no matter how long the stream is quiet.
#[tokio::test]
async fn open_tool_call_never_quiesces() {
    let rig = assemble("run the slow build");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("run the slow build"),
            None,
        )
        .await
        .expect("dispatch");

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Kicking off the build.")).unwrap();
    rig.feed
        .send(AgentEvent::ToolCall {
            id: "tool-slow".into(),
            call: ToolCall::Exec {
                command: "cargo build --release".into(),
            },
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "run starts Working",
    )
    .await;

    // Several full watchdog windows of silence mid-tool-call.
    tokio::time::sleep(Duration::from_millis(QUIESCE_MS * 4)).await;
    assert_eq!(
        status(&rig.core, CHAT),
        Some(SessionStatus::Working),
        "an unresolved tool call must never quiesce"
    );

    // The tool resolves and the turn ends normally.
    rig.feed
        .send(AgentEvent::ToolResult {
            id: "tool-slow".into(),
            is_error: false,
            output: None,
            diff: None,
        })
        .unwrap();
    rig.feed.send(text("Build done.")).unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "clean turn end",
    )
    .await;

    rig.core.sessions.shutdown().await;
}

/// Post-turn noise (the original eternally-running-session bug) must STILL be
/// gated: a stale tool echo for an id folded in a prior segment does not
/// resume a parked session.
#[tokio::test]
async fn stale_tool_echo_stays_parked() {
    let rig = assemble("do a thing");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("do a thing"), None)
        .await
        .expect("dispatch");

    rig.feed.send(session_started()).unwrap();
    rig.feed
        .send(AgentEvent::ToolCall {
            id: "tool-echoed".into(),
            call: ToolCall::Exec {
                command: "sleep 600".into(),
            },
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::ToolResult {
            id: "tool-echoed".into(),
            is_error: false,
            output: None,
            diff: None,
        })
        .unwrap();
    rig.feed
        .send(text("Started it in the background."))
        .unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // A late tool_call_update re-emitted as a full ToolCall for the OLD id —
    // sent past the resume gate, so it is the seen-tools guard (not the
    // gate) keeping the session parked.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.feed
        .send(AgentEvent::ToolCall {
            id: "tool-echoed".into(),
            call: ToolCall::Exec {
                command: "sleep 600".into(),
            },
        })
        .unwrap();

    // Give the echo ample time to (wrongly) resume the session.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        status(&rig.core, CHAT),
        Some(SessionStatus::Idle),
        "a stale tool echo must not resume a parked session"
    );

    rig.core.sessions.shutdown().await;
}

#[tokio::test]
async fn self_continued_turn_parks_on_the_short_window() {
    let rig = assemble_self_turn("watch the build");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("watch the build"), None)
        .await
        .expect("dispatch");

    // Turn 1 completes normally → parked Idle.
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("I will watch the build.")).unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // Background wake: self-continued output past the resume gate.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.feed
        .send(text("The build is green. Released."))
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "self-continued output resumes Working",
    )
    .await;

    // The short window parks it well inside the 10s wait_for horizon — the
    // normal window (10 min here) could not have.
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "short quiesce parks the self-continued turn",
    )
    .await;

    rig.core.sessions.shutdown().await;
}

#[tokio::test]
async fn steered_turn_keeps_the_normal_window() {
    let rig = assemble_self_turn("watch the build again");
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request("watch the build again"),
            None,
        )
        .await
        .expect("dispatch");

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Watching.")).unwrap();
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // Background wake resumes the session (short window armed)…
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.feed.send(text("Build done.")).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "self-continued output resumes Working",
    )
    .await;

    // …then a real steer takes the turn over: the short window must stand
    // down. The steered turn's reply streams and goes quiet — with the
    // normal window at 10 minutes, the session must STAY Working well past
    // the short window (its Done is genuinely coming).
    rig.core
        .sessions
        .steer(CHAT, "and then?", None)
        .await
        .expect("steer accepted");
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "steered turn is Working",
    )
    .await;
    rig.feed.send(text("Answering the steer.")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        status(&rig.core, CHAT),
        Some(SessionStatus::Working),
        "a steered (prompt-owned) turn must not park on the short window"
    );

    // Clean turn end.
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "steered turn parks at its Done",
    )
    .await;

    rig.core.sessions.shutdown().await;
}

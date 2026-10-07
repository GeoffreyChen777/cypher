//! The SHORT quiesce window for self-continued turns (2026-08-13 incident:
//! "stuck in working after you finished watching the build").
//!
//! A turn the agent starts on its own — a background-task wake — can never
//! receive a harness Done: the adapter has no `session/prompt` outstanding to
//! settle, so the quiesce watchdog is that turn shape's ONLY settle path.
//! With the shared 120s window every background notification ended in ~2min
//! of phantom Working. Self-continued turns now use a much shorter window
//! (`CYPHER_SELF_TURN_QUIESCE_MS`); prompt/steer turns keep the normal one.
//!
//! This file exists separately from `turn_quiesce.rs` because the env knobs
//! are process-global: here the NORMAL window is set far beyond the test
//! horizon, so a fast park can only have come through the short path.

mod common;

use std::sync::Once;
use std::time::Duration;

use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SessionStatus};

use common::{FeedRig, run_request, status, text, wait_for};

const CHAT: &str = "chat-self-quiesce";
/// Normal window: far beyond the test horizon — any park inside the test
/// window must have come through the self-continued path.
const QUIESCE_MS: u64 = 600_000;
/// Short window under test.
const SELF_QUIESCE_MS: u64 = 400;

fn init_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: called before any engine (and thus any reader of the vars)
        // exists in this test process.
        unsafe {
            std::env::set_var("CYPHER_TURN_QUIESCE_MS", QUIESCE_MS.to_string());
            std::env::set_var("CYPHER_SELF_TURN_QUIESCE_MS", SELF_QUIESCE_MS.to_string());
        }
    });
}

fn done(status: DoneStatus) -> AgentEvent {
    common::done_with(status, Some("hs-sq"))
}

fn session_started() -> AgentEvent {
    common::session_started(HarnessId::Mock, "mock-1", "/tmp", "hs-sq", "a-sq")
}

/// Feed-by-hand harness: accepted steers confirm with a `Steered` boundary.
fn assemble(main_prompt: &str) -> FeedRig {
    init_env();
    common::feed_rig(main_prompt, true)
}

#[tokio::test]
async fn self_continued_turn_parks_on_the_short_window() {
    let rig = assemble("watch the build");
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
    let rig = assemble("watch the build again");
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

//! Turn-quiesce watchdog and parked self-continuation (2026-08-12 and
//! 2026-08-13 stuck-Working incidents): a parked session resumes on
//! self-continued output, a turn whose Done is lost settles once the stream
//! goes silent with nothing in flight, and a self-started turn — which never
//! gets a Done — settles on the shorter self-turn window. A turn the watchdog
//! parks with its Done still outstanding is provisional (2026-10-09
//! re-park loop): any sign of life reopens it on the normal window, its
//! questions still reach the user, and a run that ends before it settles
//! journals it interrupted.

mod common;

use std::time::Duration;

use cypher_doc::{MessagePart, MessageRole, MessageStatus};
use cypher_engine::{EngineCore, QuiesceWindows, RunJournal, SteerOutcome};
use cypher_proto::{AgentEvent, DoneStatus, HarnessId, SessionStatus, ToolCall, UserInputQuestion};
use tokio::sync::mpsc;

use common::{FeedRig, TestHarness, entries, run_request, status, text, wait_for};

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

/// The quiesced-turn rig: a short self-turn window under a longer turn one,
/// so a reopened turn that wrongly ran on the self-turn window would park
/// well before the assertion that it is still Working.
fn assemble_quiesced(main_prompt: &str) -> FeedRig {
    assemble_with(main_prompt, quiesced_windows())
}

const QUIESCED_TURN_MS: u64 = 800;
const QUIESCED_SELF_MS: u64 = 150;

fn quiesced_windows() -> QuiesceWindows {
    QuiesceWindows {
        turn: Some(Duration::from_millis(QUIESCED_TURN_MS)),
        self_turn: Some(Duration::from_millis(QUIESCED_SELF_MS)),
    }
}

fn journal(dir: &std::path::Path) -> RunJournal {
    RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap()
}

/// Dispatch, stream one answer, and let the watchdog park the turn whose
/// Done has not come (a provider still thinking looks exactly like this).
async fn quiesce_first_turn(rig: &FeedRig, prompt: &str) {
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(prompt), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Reading the code.")).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "run starts Working",
    )
    .await;
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "watchdog parks the silent turn",
    )
    .await;
}

/// The re-park loop: output after a watchdog park is the SAME prompt-owned
/// turn coming back (its Done never came), not a self-started one. It must
/// run on the normal window — on the short self-turn window every quiet
/// reasoning step re-parked it.
#[tokio::test]
async fn quiesced_turn_resumes_on_the_normal_window() {
    let rig = assemble_quiesced("refactor the scheduler");
    quiesce_first_turn(&rig, "refactor the scheduler").await;

    rig.feed.send(text("Here is the plan.")).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "output reopens the quiesced turn",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(QUIESCED_SELF_MS * 3)).await;
    assert_eq!(
        status(&rig.core, CHAT),
        Some(SessionStatus::Working),
        "a reopened prompt turn must not park on the self-turn window"
    );

    // Its real Done lands and closes the journal.
    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "the turn's own Done settles it",
    )
    .await;
    assert!(matches!(
        journal(rig.dir.path()).last_event(CHAT).unwrap(),
        Some((
            _,
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            }
        ))
    ));

    rig.core.sessions.shutdown().await;
}

/// A reasoning heartbeat (an empty delta: redacted thinking, or the pi
/// harness's liveness probe) is proof the quiesced turn is alive: the
/// session reads Working again instead of Idle while the agent works.
#[tokio::test]
async fn heartbeat_reopens_a_quiesced_turn() {
    let rig = assemble_quiesced("think hard");
    quiesce_first_turn(&rig, "think hard").await;

    rig.feed
        .send(AgentEvent::ReasoningDelta {
            text: String::new(),
        })
        .unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Working),
        "a heartbeat reopens the quiesced turn",
    )
    .await;

    rig.feed.send(done(DoneStatus::Completed)).unwrap();
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "the turn's own Done settles it",
    )
    .await;

    rig.core.sessions.shutdown().await;
}

/// A run that ends while its turn is still provisionally parked (the reaper,
/// a child death, the stream closing) closes the journal with a terminal
/// Done — never a dangling one that subagent parents wait on forever and
/// boot recovery reads as a crash mid-stream.
#[tokio::test]
async fn run_end_closes_a_quiesced_turn_in_the_journal() {
    let rig = assemble_quiesced("migrate the database");
    quiesce_first_turn(&rig, "migrate the database").await;

    let FeedRig { core, feed, dir } = rig;
    drop(feed); // the harness stream ends while the turn is parked
    let journal = journal(dir.path());
    wait_for(
        || {
            matches!(
                journal.last_event(CHAT).unwrap(),
                Some((
                    _,
                    AgentEvent::Done {
                        status: DoneStatus::Interrupted,
                        error: Some(_),
                        ..
                    }
                ))
            )
        },
        "the run's end journals the open turn interrupted",
    )
    .await;
    assert!(journal.stale_sessions().unwrap().is_empty());

    core.sessions.shutdown().await;
}

/// A question asked after a watchdog park belongs to the outstanding turn:
/// it must reach the user (AwaitingInput), not be auto-declined as post-turn
/// noise.
#[tokio::test]
async fn question_on_a_quiesced_turn_reaches_the_user() {
    const PROMPT: &str = "pick a migration strategy";
    let (feed_tx, feed_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<()>();
    let (answer_tx, mut answer_rx) = mpsc::unbounded_channel::<usize>();
    let slots = std::sync::Mutex::new(Some((feed_rx, ask_rx)));
    let harness = TestHarness::new(HarnessId::Mock, "Ask", move |request, controls| {
        if request.prompt != PROMPT {
            return common::script(vec![done(DoneStatus::Completed)]);
        }
        let (feed_rx, mut ask_rx) = slots.lock().unwrap().take().expect("one main run");
        let answer_tx = answer_tx.clone();
        tokio::spawn(async move {
            if ask_rx.recv().await.is_none() {
                return;
            }
            let answer = (controls.request_input)(vec![UserInputQuestion {
                id: "q1".into(),
                header: "Strategy".into(),
                question: "Online or offline migration?".into(),
                options: vec!["Online".into(), "Offline".into()],
                multi_select: false,
            }]);
            if let Ok(labels) = answer.await {
                let _ = answer_tx.send(labels.len());
            }
        });
        Ok(common::channel_stream(feed_rx))
    })
    .steering();
    let common::Rig { core, dir: _dir } = common::rig(harness);
    core.sessions.set_quiesce_windows(quiesced_windows());
    core.sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(PROMPT), None)
        .await
        .expect("dispatch");
    feed_tx.send(session_started()).unwrap();
    feed_tx.send(text("Comparing the options.")).unwrap();
    wait_for(
        || status(&core, CHAT) == Some(SessionStatus::Working),
        "run starts Working",
    )
    .await;
    wait_for(
        || status(&core, CHAT) == Some(SessionStatus::Idle),
        "watchdog parks the silent turn",
    )
    .await;

    ask_tx.send(()).unwrap();
    wait_for(
        || status(&core, CHAT) == Some(SessionStatus::AwaitingInput),
        "the question reopens the turn and waits on the user",
    )
    .await;
    // Not auto-declined: no (empty) answer came back to the harness.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        answer_rx.try_recv().is_err(),
        "a quiesced turn's question must not be auto-declined"
    );

    core.sessions.shutdown().await;
}

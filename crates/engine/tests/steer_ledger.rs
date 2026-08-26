//! Steered-ledger at-least-once contract (sessions.rs): the ledger entry and
//! the mailbox send are atomic — the entry goes in BEFORE `try_send` — so a
//! steer the mailbox accepted is always retired by the `Steered` confirmation
//! (pi's parked path emits `Steered` immediately on mailbox receive), and the
//! dying run's exit drain never finds a stale entry to re-dispatch as a
//! second turn. Regression guard for the parked-pi race: a confirmed steer
//! must never be orphan-redelivered.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use cypher_engine::{EngineCore, HarnessRegistry, SteerOutcome};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-steer-ledger";

fn run_request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

fn session_started() -> AgentEvent {
    AgentEvent::SessionStarted {
        harness: HarnessId::Mock,
        model: "mock-1".into(),
        tools: vec![],
        cwd: "/tmp".into(),
        session_id: "hs-ledger".into(),
        assistant_message_id: "a-ledger".into(),
    }
}

fn text(s: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: s.into() }
}

fn done() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: Some("hs-ledger".into()),
    }
}

/// Feed-by-hand harness whose steering mailbox behaves like pi's parked path:
/// a steer message is consumed immediately and confirmed with a `Steered`
/// boundary (before any reply would stream). Non-main `run` calls are counted
/// — an orphan re-dispatch (the exit drain re-running an unconfirmed steer)
/// must never happen for a confirmed steer; the auto-titler's throwaway run is
/// recognized by its prompt template and ignored.
struct SteerLedgerHarness {
    main_prompt: String,
    feed: Mutex<Option<mpsc::UnboundedReceiver<AgentEvent>>>,
    redispatch_count: Arc<AtomicUsize>,
}

#[async_trait]
impl Harness for SteerLedgerHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "SteerLedger"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if request.prompt != self.main_prompt {
            // The auto-titler's one-shot (its template prompt) gets an
            // immediately completed stream; anything else is an orphan
            // re-dispatch and is counted.
            if !request.prompt.contains("concise 3-5 word title") {
                self.redispatch_count.fetch_add(1, Ordering::SeqCst);
            }
            let events = vec![Ok(done())];
            return Ok(futures::stream::iter(events).boxed());
        }
        let feed = self
            .feed
            .lock()
            .unwrap()
            .take()
            .expect("SteerLedgerHarness serves the main dispatch once per test");
        // Merge the test's event feed with mailbox steers: each steer is
        // confirmed immediately with a Steered boundary (pi's parked path),
        // then dropped — this test streams no reply. Ending on interrupt so
        // the run settles promptly instead of on the 3s engine deadline.
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut steering = controls.steering;
        let interrupt = controls.interrupt.clone();
        tokio::spawn(async move {
            let mut feed = futures::stream::unfold(feed, |mut feed| async move {
                feed.recv().await.map(|event| (event, feed))
            })
            .boxed();
            loop {
                tokio::select! {
                    event = feed.next(), if !interrupt.is_cancelled() => match event {
                        Some(event) => {
                            if tx.send(event).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    steer = steering.recv(), if !interrupt.is_cancelled() => match steer {
                        Some(_msg) => {
                            if tx
                                .send(AgentEvent::Steered {
                                    assistant_message_id: Some("prev-ledger".into()),
                                    next_assistant_message_id: Some("next-ledger".into()),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        None => break,
                    },
                    _ = interrupt.cancelled() => break,
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event), rx))
        })
        .boxed())
    }
}

struct Rig {
    core: EngineCore,
    feed: mpsc::UnboundedSender<AgentEvent>,
    redispatch_count: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

fn assemble(main_prompt: &str) -> Rig {
    let (feed, rx) = mpsc::unbounded_channel();
    let registry = HarnessRegistry::new();
    let redispatch_count = Arc::new(AtomicUsize::new(0));
    registry.register(Arc::new(SteerLedgerHarness {
        main_prompt: main_prompt.into(),
        feed: Mutex::new(Some(rx)),
        redispatch_count: redispatch_count.clone(),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    Rig {
        core,
        feed,
        redispatch_count,
        _dir: dir,
    }
}

fn status(core: &EngineCore) -> Option<SessionStatus> {
    core.sessions.session_status(CHAT).map(|s| s.status)
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A steer the mailbox accepted AND the run confirmed (Steered boundary) is
/// retired from the at-least-once ledger. When that run later dies, the exit
/// drain must find nothing and never re-dispatch the message as an orphan
/// turn. Guards the entry-before-send atomic ordering in `steer`/
/// `dispatch_inner`: a stale entry pushed AFTER the send would be drained
/// here and re-dispatched a second time (the parked-pi race).
#[tokio::test]
async fn confirmed_steer_is_never_orphan_redelivered() {
    let rig = assemble("watch the build");
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("watch the build"), None)
        .await
        .expect("dispatch");

    rig.feed.send(session_started()).unwrap();
    rig.feed.send(text("Watching.")).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    // Steer into the parked run: the harness confirms it with a Steered
    // boundary immediately (pi's parked path), retiring the ledger entry.
    let outcome = rig
        .core
        .sessions
        .steer(CHAT, "follow-up", Some("msg-2".to_string()))
        .await
        .expect("steer");
    assert_eq!(outcome, SteerOutcome::Accepted);
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Working),
        "Steered boundary re-arms Working",
    )
    .await;

    // Kill the run: its exit drain must find an empty ledger (the entry was
    // retired at Steered), so no orphan re-dispatch of "follow-up".
    rig.core.sessions.interrupt(CHAT).await.expect("interrupt");
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Idle),
        "interrupt settles Idle",
    )
    .await;
    // Give any (wrong) orphan re-dispatch a beat to surface.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.redispatch_count.load(Ordering::SeqCst),
        0,
        "a confirmed steer must never be orphan-re-dispatched"
    );

    rig.core.sessions.shutdown().await;
}

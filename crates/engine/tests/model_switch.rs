//! Mid-session model switch (sessions.rs `retire_stale_run`): a parked,
//! steerable run keeps the model its harness process was launched with, so a
//! turn asking for a different model must end that run and spawn a fresh one
//! instead of being routed into it. A turn on the SAME settings still routes
//! into the warm process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-model-switch";

fn run_request(prompt: &str, model: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: Some(model.into()),
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

fn done() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: Some("hs-switch".into()),
    }
}

/// Persistent harness: every run answers its prompt, parks, and answers each
/// steer as a new turn until interrupted. Records the model each spawn was
/// launched with (the auto-titler's one-shot is ignored).
struct ParkingHarness {
    launches: Arc<Mutex<Vec<Option<String>>>>,
    resumes: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl Harness for ParkingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Parking"
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
        if request.prompt.contains("concise 3-5 word title") {
            return Ok(futures::stream::iter(vec![Ok(done())]).boxed());
        }
        self.launches.lock().unwrap().push(request.model.clone());
        self.resumes.lock().unwrap().push(request.resume.clone());
        let model = request.model.clone().unwrap_or_default();
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut steering = controls.steering;
        let interrupt = controls.interrupt.clone();
        tokio::spawn(async move {
            let _ = tx.send(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: model.clone(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-switch".into(),
                assistant_message_id: "a-switch".into(),
            });
            let _ = tx.send(AgentEvent::TextDelta {
                text: format!("[{model}] first"),
            });
            let _ = tx.send(done());
            loop {
                tokio::select! {
                    steer = steering.recv() => match steer {
                        Some(_) => {
                            let _ = tx.send(AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            });
                            let _ = tx.send(AgentEvent::TextDelta {
                                text: format!("[{model}] routed"),
                            });
                            let _ = tx.send(done());
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
    launches: Arc<Mutex<Vec<Option<String>>>>,
    resumes: Arc<Mutex<Vec<Option<String>>>>,
    _dir: tempfile::TempDir,
}

fn assemble() -> Rig {
    let registry = HarnessRegistry::new();
    let launches = Arc::new(Mutex::new(Vec::new()));
    let resumes = Arc::new(Mutex::new(Vec::new()));
    registry.register(Arc::new(ParkingHarness {
        launches: launches.clone(),
        resumes: resumes.clone(),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    Rig {
        core,
        launches,
        resumes,
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

async fn send_and_park(rig: &Rig, prompt: &str, model: &str) -> String {
    let run_id = rig
        .core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(prompt, model), None)
        .await
        .expect("dispatch");
    // Let the turn start before waiting for it to park again.
    tokio::time::sleep(Duration::from_millis(50)).await;
    wait_for(|| status(&rig.core) == Some(SessionStatus::Idle), "park").await;
    run_id
}

#[tokio::test]
async fn switching_model_respawns_parked_run() {
    let rig = assemble();

    let first = send_and_park(&rig, "hello", "model-a").await;
    let same = send_and_park(&rig, "again", "model-a").await;
    assert_eq!(first, same, "same settings route into the warm run");
    assert_eq!(*rig.launches.lock().unwrap(), vec![Some("model-a".into())]);

    let switched = send_and_park(&rig, "now on b", "model-b").await;
    assert_ne!(first, switched, "a new model must not reuse the parked run");
    assert_eq!(
        *rig.launches.lock().unwrap(),
        vec![Some("model-a".into()), Some("model-b".into())],
        "the fresh run launches with the newly picked model"
    );
    assert_eq!(
        *rig.resumes.lock().unwrap(),
        vec![None, Some("hs-switch".into())],
        "the fresh run resumes the parked run's harness session"
    );
    assert_eq!(
        rig.core
            .sessions
            .last_request(CHAT)
            .and_then(|r| r.model)
            .as_deref(),
        Some("model-b")
    );

    rig.core.sessions.shutdown().await;
}

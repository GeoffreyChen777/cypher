//! Mid-session model switch (session/engine.rs `retire_stale_run`): a parked,
//! steerable run keeps the model its harness process was launched with, so a
//! turn asking for a different model must end that run and spawn a fresh one
//! instead of being routed into it. The same holds for the permission level,
//! which Pi's permission extension reads from the process environment. A turn
//! on the SAME settings still routes into the warm process.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use cypher_engine::EngineCore;
use cypher_proto::{AgentEvent, HarnessId, RunRequest, SandboxLevel, SessionStatus};

use common::{status, text, wait_for};

const CHAT: &str = "chat-model-switch";

fn run_request(prompt: &str, model: &str) -> RunRequest {
    RunRequest {
        model: Some(model.into()),
        ..common::run_request(prompt)
    }
}

fn done() -> AgentEvent {
    common::done("hs-switch")
}

struct Rig {
    core: EngineCore,
    launches: Arc<Mutex<Vec<Option<String>>>>,
    levels: Arc<Mutex<Vec<SandboxLevel>>>,
    resumes: Arc<Mutex<Vec<Option<String>>>>,
    _dir: tempfile::TempDir,
}

/// Persistent harness: every run answers its prompt, parks, and answers each
/// steer as a new turn until interrupted. Records the model each spawn was
/// launched with (the auto-titler's one-shot is ignored).
fn assemble() -> Rig {
    let launches = Arc::new(Mutex::new(Vec::new()));
    let resumes = Arc::new(Mutex::new(Vec::new()));
    let levels = Arc::new(Mutex::new(Vec::new()));
    let (log_launches, log_resumes, log_levels) =
        (launches.clone(), resumes.clone(), levels.clone());
    let harness = common::TestHarness::new(HarnessId::Mock, "Parking", move |request, controls| {
        if request.prompt.contains("concise 3-5 word title") {
            return common::script(vec![done()]);
        }
        log_launches.lock().unwrap().push(request.model.clone());
        log_resumes.lock().unwrap().push(request.resume.clone());
        log_levels.lock().unwrap().push(request.sandbox);
        let model = request.model.clone().unwrap_or_default();
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut steering = controls.steering;
        let interrupt = controls.interrupt.clone();
        tokio::spawn(async move {
            let _ = tx.send(common::session_started(
                HarnessId::Mock,
                &model,
                "/tmp",
                "hs-switch",
                "a-switch",
            ));
            let _ = tx.send(text(&format!("[{model}] first")));
            let _ = tx.send(done());
            loop {
                tokio::select! {
                    steer = steering.recv() => match steer {
                        Some(_) => {
                            let _ = tx.send(AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            });
                            let _ = tx.send(text(&format!("[{model}] routed")));
                            let _ = tx.send(done());
                        }
                        None => break,
                    },
                    _ = interrupt.cancelled() => break,
                }
            }
        });
        Ok(common::channel_stream(rx))
    })
    .steering();
    let common::Rig { core, dir } = common::rig(harness);
    Rig {
        core,
        launches,
        levels,
        resumes,
        _dir: dir,
    }
}

async fn send_and_park(rig: &Rig, prompt: &str, model: &str) -> String {
    send_request_and_park(rig, run_request(prompt, model)).await
}

async fn send_request_and_park(rig: &Rig, request: RunRequest) -> String {
    let run_id = rig
        .core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, request, None)
        .await
        .expect("dispatch");
    // Let the turn start before waiting for it to park again.
    tokio::time::sleep(Duration::from_millis(50)).await;
    wait_for(
        || status(&rig.core, CHAT) == Some(SessionStatus::Idle),
        "park",
    )
    .await;
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

#[tokio::test]
async fn switching_permission_level_respawns_parked_run() {
    let rig = assemble();
    let at = |prompt: &str, sandbox| RunRequest {
        sandbox,
        ..run_request(prompt, "model-a")
    };

    let first = send_request_and_park(&rig, at("hello", SandboxLevel::WorkspaceWrite)).await;
    let same = send_request_and_park(&rig, at("again", SandboxLevel::WorkspaceWrite)).await;
    assert_eq!(first, same, "the same level routes into the warm run");

    let full = send_request_and_park(&rig, at("go", SandboxLevel::DangerFullAccess)).await;
    assert_ne!(first, full, "a new level must not reuse the parked run");
    assert_eq!(
        *rig.levels.lock().unwrap(),
        vec![SandboxLevel::WorkspaceWrite, SandboxLevel::DangerFullAccess],
        "the fresh run launches with the newly picked level"
    );
    assert_eq!(
        rig.core.sessions.last_request(CHAT).map(|r| r.sandbox),
        Some(SandboxLevel::DangerFullAccess)
    );

    rig.core.sessions.shutdown().await;
}

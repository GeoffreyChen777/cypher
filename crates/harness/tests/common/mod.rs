//! Helpers shared by the harness integration-test binaries. Each binary uses
//! a subset, hence the blanket `dead_code` allowance.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use cypher_harness::{CancellationToken, Harness, RunControls, RunHostContext, SteerMessage};
use cypher_proto::{AgentEvent, DoneStatus, RunRequest, SandboxLevel, UserInputAnswer};

/// A script under `tests/fixtures`, made executable.
pub fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

pub fn request(prompt: &str, model: Option<&str>) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: model.map(Into::into),
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

/// Run controls whose input requests are answered with `label` for every
/// question.
pub fn controls_answering(
    label: &'static str,
) -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![label.into()],
                })
                .collect();
            let _ = tx.send(answers);
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
        host: RunHostContext::default(),
    };
    (controls, steer_tx, token)
}

pub async fn run_to_end(
    harness: &impl Harness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

pub fn dones(events: &[AgentEvent]) -> Vec<(DoneStatus, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, error, .. } => Some((*status, error.clone())),
            _ => None,
        })
        .collect()
}

/// Set `CYPHER_ACP_QUIET_SETTLE_MS` once for this test process. The knob is
/// process-global, so every test in the binary shares the one value.
pub fn init_quiet_settle(ms: u64) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: set before any harness runs in this test process.
        unsafe { std::env::set_var("CYPHER_ACP_QUIET_SETTLE_MS", ms.to_string()) };
    });
}

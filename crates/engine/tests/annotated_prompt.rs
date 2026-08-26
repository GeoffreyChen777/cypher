//! Effective-prompt override (the Comment feature): a Run/Steer command with
//! `agent_prompt` delivers the AUGMENTED prompt to the harness while the doc
//! user entry keeps the VISIBLE prompt. Without the field the harness gets
//! the visible prompt (behavior unchanged), and the retry re-delivers the
//! same override.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use cypher_doc::{MessagePart, MessageRole};
use cypher_engine::{EngineCore, HarnessRegistry, SteerOutcome};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-annotated";

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
        session_id: "hs-annotated".into(),
        assistant_message_id: "a-annotated".into(),
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
        session_id: Some("hs-annotated".into()),
    }
}

/// Records every prompt the harness received — the main run's request AND
/// every mailbox steer (pi's parked path consumes a steer immediately and
/// confirms it with a `Steered` boundary, so the recording covers accepted
/// steers end-to-end).
struct RecordingHarness {
    prompts: Arc<Mutex<Vec<String>>>,
    feed: Mutex<Option<mpsc::UnboundedReceiver<AgentEvent>>>,
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "RecordingHarness"
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
        // The auto-titler's throwaway run (its template prompt) is not a user
        // turn — never recorded.
        if request.prompt.contains("concise 3-5 word title") {
            let events = vec![Ok(done())];
            return Ok(futures::stream::iter(events).boxed());
        }
        self.prompts.lock().unwrap().push(request.prompt.clone());
        let feed = self
            .feed
            .lock()
            .unwrap()
            .take()
            .expect("RecordingHarness serves the main dispatch once per test");
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let prompts = self.prompts.clone();
        let interrupt = controls.interrupt.clone();
        let mut steering = controls.steering;
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
                        Some(msg) => {
                            prompts.lock().unwrap().push(msg.prompt.clone());
                            if tx
                                .send(AgentEvent::Steered {
                                    assistant_message_id: Some("prev-annotated".into()),
                                    next_assistant_message_id: Some("next-annotated".into()),
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
    prompts: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

fn assemble() -> Rig {
    let (feed, rx) = mpsc::unbounded_channel();
    let registry = HarnessRegistry::new();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    registry.register(Arc::new(RecordingHarness {
        prompts: prompts.clone(),
        feed: Mutex::new(Some(rx)),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    Rig {
        core,
        feed,
        prompts,
        _dir: dir,
    }
}

fn status(core: &EngineCore) -> Option<SessionStatus> {
    core.sessions.session_status(CHAT).map(|s| s.status)
}

/// Tolerant read (see e2e.rs): a snapshot mid-segment-write deserializes with
/// fields missing — treat that instant as "not yet".
fn entries(core: &EngineCore) -> Vec<cypher_doc::SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn user_texts(core: &EngineCore) -> Vec<String> {
    entries(core)
        .into_iter()
        .filter(|e| e.role == MessageRole::User)
        .filter_map(|e| {
            e.parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .next()
        })
        .collect()
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

/// A Run carrying `agent_prompt`: the harness receives the AUGMENTED prompt
/// while the doc user entry keeps the VISIBLE prompt.
#[tokio::test]
async fn run_delivers_agent_prompt_but_keeps_visible_entry() {
    let rig = assemble();
    let visible = "check the build";
    let augmented =
        "Conversation annotations (JSON): {\"comments\":[]}\n\nUser request:\ncheck the build";
    rig.core
        .sessions
        .dispatch_augmented(
            CHAT,
            HarnessId::Mock,
            run_request(visible),
            Some(augmented.to_string()),
            None,
        )
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

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![augmented],
        "the harness must receive the effective (augmented) prompt"
    );
    assert_eq!(
        user_texts(&rig.core),
        vec![visible.to_string()],
        "the doc user entry must stay the visible prompt"
    );
    rig.core.sessions.shutdown().await;
}

/// A comment itself is sufficient turn content: the harness receives a
/// non-empty annotation prompt while the document does not invent filler text
/// for the user's visible transcript.
#[tokio::test]
async fn comment_only_run_delivers_annotations_without_visible_filler() {
    let rig = assemble();
    let augmented = concat!(
        "Conversation annotations (JSON): ",
        "{\"comments\":[{\"quotedText\":\"old text\",\"comment\":\"fix this\"}]}",
        "\n\nUser request:\n"
    );
    rig.core
        .sessions
        .dispatch_augmented(
            CHAT,
            HarnessId::Mock,
            run_request(""),
            Some(augmented.to_string()),
            None,
        )
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;

    assert_eq!(*rig.prompts.lock().unwrap(), vec![augmented.to_string()]);
    assert_eq!(
        user_texts(&rig.core),
        vec![String::new()],
        "the document must not synthesize visible filler text"
    );
    rig.core.sessions.shutdown().await;
}

/// Without `agent_prompt` the harness receives the visible prompt — old
/// behavior is byte-compatible.
#[tokio::test]
async fn run_without_agent_prompt_delivers_visible_prompt() {
    let rig = assemble();
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request("plain request"), None)
        .await
        .expect("dispatch");
    rig.feed.send(session_started()).unwrap();
    rig.feed.send(done()).unwrap();
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    assert_eq!(
        *rig.prompts.lock().unwrap(),
        vec!["plain request".to_string()]
    );
    assert_eq!(user_texts(&rig.core), vec!["plain request".to_string()]);
    rig.core.sessions.shutdown().await;
}

/// An ACCEPTED steer carrying `agent_prompt` delivers the augmented prompt to
/// the harness mailbox while the doc keeps the visible steer text.
#[tokio::test]
async fn accepted_steer_delivers_agent_prompt_but_keeps_visible_entry() {
    let rig = assemble();
    let visible_run = "watch the build";
    rig.core
        .sessions
        .dispatch(CHAT, HarnessId::Mock, run_request(visible_run), None)
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

    let visible_steer = "follow-up";
    let augmented_steer =
        "Conversation annotations (JSON): {\"comments\":[]}\n\nUser request:\nfollow-up";
    let outcome = rig
        .core
        .sessions
        .steer_augmented(
            CHAT,
            visible_steer,
            Some(augmented_steer.to_string()),
            Some("msg-2".to_string()),
        )
        .await
        .expect("steer");
    assert_eq!(outcome, SteerOutcome::Accepted);
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Working),
        "Steered boundary re-arms Working",
    )
    .await;
    // The engine sets Working optimistically on acceptance; the harness's
    // parked-path consumption is async — wait for the steer to be recorded.
    wait_for(
        || rig.prompts.lock().unwrap().len() >= 2,
        "harness records the mailbox steer",
    )
    .await;

    let received = rig.prompts.lock().unwrap().clone();
    assert_eq!(
        received,
        vec![visible_run.to_string(), augmented_steer.to_string()],
        "main run + augmented steer delivered to the harness"
    );
    assert_eq!(
        user_texts(&rig.core),
        vec![visible_run.to_string(), visible_steer.to_string()],
        "both doc user entries stay visible"
    );
    rig.core.sessions.shutdown().await;
}

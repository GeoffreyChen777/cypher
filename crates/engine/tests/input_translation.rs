//! Prompt translation (pi `cypher.translation.input.v1`): an
//! `AgentEvent::InputTranslation` stamps the translation onto the USER entry
//! whose text it replaced — the transcript keeps showing the prompt as typed
//! while quotes from it can map back to what the agent read. It is not run
//! activity: no assistant segment, no status change, and it lands even while
//! the session is parked (the extension translates a prompt before the turn it
//! opens has started).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use cypher_doc::{MessagePart, MessageRole, SessionMessageEntry};
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

const CHAT: &str = "chat-input-translation";
const PROMPT: &str = "解释一下这个函数";

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
        pending_attachments: Vec::new(),
        resume: None,
        worktree: None,
    }
}

/// Feed-by-hand harness (same shape as turn_quiesce.rs): the test pushes
/// events through a channel. The auto-titler's side run gets an immediately
/// completed empty stream instead.
struct FeedHarness {
    main_prompt: String,
    feed: Mutex<Option<mpsc::UnboundedReceiver<AgentEvent>>>,
}

#[async_trait]
impl Harness for FeedHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Feed"
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
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        if request.prompt != self.main_prompt {
            let events = vec![Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            })];
            return Ok(futures::stream::iter(events).boxed());
        }
        let feed = self
            .feed
            .lock()
            .unwrap()
            .take()
            .expect("FeedHarness serves the main dispatch once per test");
        Ok(futures::stream::unfold(feed, |mut feed| async move {
            feed.recv().await.map(|event| (Ok(event), feed))
        })
        .boxed())
    }
}

struct Rig {
    core: EngineCore,
    feed: mpsc::UnboundedSender<AgentEvent>,
    _dir: tempfile::TempDir,
}

fn assemble(main_prompt: &str) -> Rig {
    let (feed, rx) = mpsc::unbounded_channel();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FeedHarness {
        main_prompt: main_prompt.into(),
        feed: Mutex::new(Some(rx)),
    }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    Rig {
        core,
        feed,
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

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn prompt_agent_text(core: &EngineCore) -> Option<String> {
    entries(core)
        .into_iter()
        .find(|e| e.role == MessageRole::User)
        .and_then(|e| match e.parts.into_iter().next() {
            Some(MessagePart::Text { agent_text, .. }) => agent_text,
            _ => None,
        })
}

#[tokio::test]
async fn a_prompt_translation_stamps_the_user_entry_even_while_parked() {
    let rig = assemble(PROMPT);
    rig.core
        .sessions
        .dispatch(
            CHAT,
            HarnessId::Mock,
            run_request(PROMPT),
            Some("m1".into()),
        )
        .await
        .expect("dispatch");
    rig.feed
        .send(AgentEvent::TextDelta {
            text: "It parses the config.".into(),
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        })
        .unwrap();
    wait_for(
        || status(&rig.core) == Some(SessionStatus::Idle),
        "park after Done",
    )
    .await;
    let before = entries(&rig.core).len();

    // A translation for text no recent prompt holds is dropped.
    rig.feed
        .send(AgentEvent::InputTranslation {
            source: "something else".into(),
            text: "Something else".into(),
        })
        .unwrap();
    rig.feed
        .send(AgentEvent::InputTranslation {
            source: PROMPT.into(),
            text: "Explain this function".into(),
        })
        .unwrap();
    wait_for(
        || prompt_agent_text(&rig.core).is_some(),
        "the prompt's agent version",
    )
    .await;
    assert_eq!(
        prompt_agent_text(&rig.core).as_deref(),
        Some("Explain this function")
    );
    // The displayed prompt is untouched, no entry was added, and the parked
    // session stays parked.
    let after = entries(&rig.core);
    assert_eq!(after.len(), before);
    assert!(matches!(
        &after[0].parts[0],
        MessagePart::Text { text, .. } if text == PROMPT
    ));
    assert_eq!(status(&rig.core), Some(SessionStatus::Idle));

    rig.core.shutdown().await;
}

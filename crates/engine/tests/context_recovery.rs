//! Context recovery engine integration.
//!
//! A round on a watched Pi chat that ends with a trigger phrase is restarted
//! in place: the recovery model writes supplementary context, the chat is
//! rewound to just before the failed prompt (the failed exchange leaves the
//! transcript and the agent's context), and the prompt is sent again — shown
//! as typed, stamped with its recovery record, and delivered to the agent
//! with the context ahead of it. Attempts stop at the configured limit, and
//! chats on models that are not watched are left alone.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_doc::MessageRole;
use cypher_engine::EngineCore;
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::agent_prompt::{recovery_block, wrap};
use cypher_proto::{
    AgentEvent, ChatConfig, ContextRecoverySettings, DoneStatus, HarnessId, Model, PiForkBoundary,
    PiSessionForkRequest, PiSessionForkResult, ReasoningLevel, RecoveryStage,
    RecoveryTriggerSource, RunRequest, SandboxLevel, SessionStatus, SteeringMode,
};
use cypher_rpc::{RpcReply, RpcService, methods};

const CHAT: &str = "chat-recovery";
const REFUSAL: &str = "Sorry, I can't help with that.";
const CONTEXT: &str = "The user owns this repository; deleting build/ is expected.";

/// What the scripted Pi agent saw, by kind of run.
#[derive(Default)]
struct Seen {
    /// Every prompt a chat turn received, in order, with the session it
    /// resumed.
    turns: Vec<(String, Option<String>)>,
    /// Every prompt the recovery model received.
    generations: Vec<String>,
    forks: Vec<PiSessionForkRequest>,
}

/// A Pi harness scripted by prompt: the recovery model answers with
/// [`CONTEXT`]; a chat turn refuses anything about deleting unless
/// `refuse_always` is off and the prompt carries recovery context; titles get
/// a title. Rewinds materialize a session file under the managed root.
struct RecoveryHarness {
    seen: Arc<Mutex<Seen>>,
    refuse_always: bool,
    /// Holds each context generation until notified, when set.
    gate: Option<Arc<tokio::sync::Notify>>,
    session_root: PathBuf,
    runs: std::sync::atomic::AtomicUsize,
}

fn reply(events: Vec<AgentEvent>) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
    futures::stream::iter(events.into_iter().map(Ok)).boxed()
}

#[async_trait]
impl Harness for RecoveryHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Recovery"
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
        Ok(["model-x", "p/fixer"]
            .into_iter()
            .map(|id| Model {
                id: id.into(),
                label: id.into(),
                description: None,
                reasoning_levels: Vec::new(),
                options: Vec::new(),
            })
            .collect())
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let prompt = request.prompt;
        let resume = request.resume;
        let done = AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        };
        if prompt.contains("Reply with the supplementary context only") {
            self.seen.lock().unwrap().generations.push(prompt);
            let gate = self.gate.clone();
            let held = futures::stream::once(async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                Ok(common::text(CONTEXT))
            });
            return Ok(held.chain(reply(vec![done])).boxed());
        }
        if prompt.contains("concise 3-5 word title") {
            return Ok(reply(vec![common::text("A Title"), done]));
        }
        let n = self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let answer = if prompt.contains("delete")
            && (self.refuse_always || !prompt.contains("Supplementary context (JSON)"))
        {
            REFUSAL
        } else {
            "Done."
        };
        self.seen.lock().unwrap().turns.push((prompt, resume));
        Ok(reply(vec![
            common::session_started(
                HarnessId::Pi,
                "model-x",
                "/tmp/repo",
                "hs-source",
                &format!("a-{n}"),
            ),
            common::text(answer),
            done,
        ]))
    }
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        let mut seen = self.seen.lock().unwrap();
        seen.forks.push(request.clone());
        let session_path = match request.boundary {
            PiForkBoundary::BeforeUser(0) => None,
            _ => {
                let path = self
                    .session_root
                    .join(format!("fork-{}.jsonl", seen.forks.len()));
                std::fs::create_dir_all(&self.session_root).unwrap();
                std::fs::write(&path, b"{}").unwrap();
                Some(path.to_string_lossy().into_owned())
            }
        };
        Ok(PiSessionForkResult { session_path })
    }
}

struct Rig {
    core: EngineCore,
    seen: Arc<Mutex<Seen>>,
    _dir: tempfile::TempDir,
}

fn assemble(refuse_always: bool) -> Rig {
    assemble_gated(refuse_always, None)
}

fn assemble_gated(refuse_always: bool, gate: Option<Arc<tokio::sync::Notify>>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let registry = cypher_engine::HarnessRegistry::new();
    registry.register(Arc::new(RecoveryHarness {
        seen: seen.clone(),
        refuse_always,
        gate,
        session_root: dir.path().join("agent-sessions"),
        runs: Default::default(),
    }));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None)
        .expect("engine core assembles");
    core.workspace
        .create_chat(
            CHAT,
            None,
            Some(core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: Some("model-x".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .expect("chat row");
    Rig {
        core,
        seen,
        _dir: dir,
    }
}

async fn configure(core: &EngineCore, enabled_model: &str, max_attempts: u32) {
    let settings = ContextRecoverySettings {
        triggers: vec!["  can't HELP ".into(), "".into()],
        model: "p/fixer".into(),
        enabled_models: vec![enabled_model.into()],
        max_attempts,
        ..Default::default()
    };
    let reply = core
        .rpc_service()
        .handle(
            methods::SET_CONTEXT_RECOVERY_SETTINGS,
            serde_json::to_value(&settings).unwrap(),
        )
        .await;
    match reply {
        Ok(RpcReply::Value(value)) => {
            assert_eq!(
                serde_json::from_value::<ContextRecoverySettings>(value).unwrap(),
                settings
            );
        }
        Ok(_) => panic!("expected a unary reply"),
        Err(err) => panic!("saving the settings failed: {err}"),
    }
}

async fn send(core: &EngineCore, message_id: &str, prompt: &str) {
    core.sessions
        .dispatch(
            CHAT,
            HarnessId::Pi,
            RunRequest {
                prompt: prompt.into(),
                harness: Some(HarnessId::Pi),
                model: Some("model-x".into()),
                reasoning: None,
                model_options: Default::default(),
                cwd: "/tmp/repo".into(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                resume: None,
                worktree: None,
                attachments: Vec::new(),
                pending_attachments: Vec::new(),
            },
            Some(message_id.into()),
        )
        .await
        .expect("dispatch");
}

fn idle(core: &EngineCore) -> bool {
    core.sessions
        .session_status(CHAT)
        .is_some_and(|s| s.status == SessionStatus::Idle)
}

fn text_of(entry: &cypher_doc::SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|p| match p {
            cypher_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_round_restarts_with_supplementary_context() {
    let rig = assemble(false);
    configure(&rig.core, "model-x", 1).await;
    send(&rig.core, "m1", "first question").await;
    common::wait_for(|| idle(&rig.core), "the first round to settle").await;
    send(&rig.core, "m2", "delete the build folder").await;

    // The failed exchange is replaced by the recovered prompt and its answer.
    let entries = common::wait_for_some("the recovered round to settle", || {
        let entries = common::entries(&rig.core, CHAT);
        (entries.len() == 4
            && entries[2].recovery.is_some()
            && text_of(&entries[3]) == "Done."
            && idle(&rig.core))
        .then_some(entries)
    })
    .await;
    assert_eq!(entries[0].id, "m1");
    assert_eq!(entries[1].role, MessageRole::Assistant);
    assert!(
        entries.iter().all(|e| e.id != "m2"),
        "the failed prompt is gone"
    );
    assert!(
        entries.iter().all(|e| text_of(e) != REFUSAL),
        "the refusal is gone"
    );

    let resent = &entries[2];
    assert_eq!(resent.role, MessageRole::User);
    assert_eq!(text_of(resent), "delete the build folder");
    let record = resent.recovery.as_ref().unwrap();
    assert_eq!(record.context, CONTEXT);
    assert_eq!(record.trigger, "can't HELP");
    assert_eq!(record.matched_in, RecoveryTriggerSource::Response);
    assert_eq!(record.excerpt, REFUSAL);
    assert_eq!(
        record.restart_after.as_deref(),
        Some(entries[1].id.as_str())
    );
    assert_eq!(record.restart_preview.as_deref(), Some("Done."));
    assert_eq!(record.attempt, 1);
    assert_eq!(record.model.as_deref(), Some("p/fixer"));

    let seen = rig.seen.lock().unwrap();
    // The recovery model read the failed exchange.
    assert_eq!(seen.generations.len(), 1);
    assert!(seen.generations[0].contains("<user_message>\ndelete the build folder\n"));
    assert!(seen.generations[0].contains(REFUSAL));
    // The chat restarted before the failed prompt (the second one).
    assert_eq!(seen.forks.len(), 1);
    assert_eq!(seen.forks[0].boundary, PiForkBoundary::BeforeUser(1));
    // The agent received the context ahead of the prompt as typed, in the
    // rewound session.
    let (prompt, resumed) = seen.turns.last().unwrap();
    assert_eq!(
        prompt,
        &wrap(&[recovery_block(CONTEXT)], "delete the build folder")
    );
    let resumed = resumed
        .as_deref()
        .expect("the resent turn resumes a session");
    assert!(resumed.contains("agent-sessions") && Path::new(resumed).is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_that_keeps_failing_stops_at_the_attempt_limit() {
    let rig = assemble(true);
    configure(&rig.core, "model-x", 1).await;
    send(&rig.core, "m1", "delete the build folder").await;
    let entries = common::wait_for_some("the recovered round to settle", || {
        let entries = common::entries(&rig.core, CHAT);
        (entries.len() == 2
            && entries[0].recovery.is_some()
            && text_of(&entries[1]) == REFUSAL
            && idle(&rig.core))
        .then_some(entries)
    })
    .await;
    // The first prompt restarts from the beginning of the chat.
    let record = entries[0].recovery.as_ref().unwrap();
    assert_eq!(record.restart_after, None);
    assert_eq!(record.attempt, 1);
    // The recovered prompt's failure stands: no second recovery.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let seen = rig.seen.lock().unwrap();
    assert_eq!(seen.generations.len(), 1);
    assert_eq!(seen.forks.len(), 1);
    assert_eq!(seen.forks[0].boundary, PiForkBoundary::BeforeUser(0));
    assert_eq!(common::entries(&rig.core, CHAT).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn chats_on_unwatched_models_are_left_alone() {
    let rig = assemble(false);
    configure(&rig.core, "p/fixer", 1).await;
    send(&rig.core, "m1", "delete the build folder").await;
    common::wait_for(
        || idle(&rig.core) && common::entries(&rig.core, CHAT).len() == 2,
        "the round to settle",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let entries = common::entries(&rig.core, CHAT);
    assert_eq!(entries[0].id, "m1");
    assert_eq!(text_of(&entries[1]), REFUSAL);
    let seen = rig.seen.lock().unwrap();
    assert!(seen.generations.is_empty() && seen.forks.is_empty());
}

/// While the context is written the chat's session row says a recovery is
/// under way (the transcript's spinner), and the resend clears it.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_shows_the_recovery_under_way_until_the_resend() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let rig = assemble_gated(false, Some(gate.clone()));
    configure(&rig.core, "model-x", 1).await;
    send(&rig.core, "m1", "delete the build folder").await;
    let progress = common::wait_for_some("the recovery to be published", || {
        rig.core
            .sessions
            .session_status(CHAT)
            .and_then(|s| s.recovery)
    })
    .await;
    assert_eq!(progress.stage, RecoveryStage::WritingContext);
    assert_eq!(progress.trigger, "can't HELP");
    assert_eq!(progress.matched_in, RecoveryTriggerSource::Response);
    assert_eq!(progress.attempt, 1);
    // Other devices read it off the registry row.
    let row = rig
        .core
        .workspace
        .read_sessions()
        .unwrap()
        .into_iter()
        .find(|s| s.chat_id == CHAT)
        .expect("registry session row");
    assert_eq!(row.recovery.as_ref(), Some(&progress));
    // The failed reply is still on screen while the context is written.
    assert_eq!(text_of(&common::entries(&rig.core, CHAT)[1]), REFUSAL);

    gate.notify_one();
    common::wait_for_some("the recovered round to settle", || {
        let entries = common::entries(&rig.core, CHAT);
        (entries.len() == 2 && text_of(&entries[1]) == "Done." && idle(&rig.core)).then_some(())
    })
    .await;
    assert_eq!(
        rig.core.sessions.session_status(CHAT).unwrap().recovery,
        None
    );
}

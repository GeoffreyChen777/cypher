//! Shared fixtures for the engine integration tests: a configurable
//! [`TestHarness`], engine assembly, event constructors, polling, and
//! transcript readers. Each test binary uses a different subset.
#![allow(dead_code)]

pub mod edge;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use cypher_doc::{
    MessagePart, MessageRole, MessageStatus, SessionCommandStatus, SessionMessageEntry,
};
use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};

pub type EventStream = BoxStream<'static, Result<AgentEvent, HarnessError>>;
type RunFn = dyn Fn(RunRequest, RunControls) -> Result<EventStream, HarnessError> + Send + Sync;

/// A harness whose identity is configurable and whose `run` is a closure, so
/// each test only spells out the behaviour it exercises. Defaults: no
/// steering (turn boundary), `Medium` reasoning, no models.
pub struct TestHarness {
    id: HarnessId,
    name: &'static str,
    steering: bool,
    reasoning: &'static [ReasoningLevel],
    run: Box<RunFn>,
}

impl TestHarness {
    pub fn new(
        id: HarnessId,
        name: &'static str,
        run: impl Fn(RunRequest, RunControls) -> Result<EventStream, HarnessError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            id,
            name,
            steering: false,
            reasoning: &[ReasoningLevel::Medium],
            run: Box::new(run),
        }
    }

    /// Replays `events` on every run.
    pub fn scripted(id: HarnessId, name: &'static str, events: Vec<AgentEvent>) -> Self {
        Self::new(id, name, move |_, _| script(events.clone()))
    }

    /// Accept steering at step boundaries.
    pub fn steering(mut self) -> Self {
        self.steering = true;
        self
    }

    pub fn reasoning(mut self, levels: &'static [ReasoningLevel]) -> Self {
        self.reasoning = levels;
        self
    }
}

#[async_trait]
impl Harness for TestHarness {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        self.name
    }
    fn supports_steering(&self) -> bool {
        self.steering
    }
    fn steering_mode(&self) -> SteeringMode {
        if self.steering {
            SteeringMode::StepBoundary
        } else {
            SteeringMode::TurnBoundary
        }
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.reasoning
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<EventStream, HarnessError> {
        (self.run)(request, controls)
    }
}

/// Feed-by-hand harness (id `Mock`, steerable): the test's own dispatch
/// (matched by `main_prompt`) streams whatever the test pushes through the
/// returned sender; any other run — the engine's auto-titler — completes
/// immediately with nothing. With `confirm_steers`, each accepted steer is
/// confirmed with a `Steered` boundary ahead of later feed events, like the
/// ACP adapters do.
pub fn feed_harness(
    main_prompt: &str,
    confirm_steers: bool,
) -> (TestHarness, mpsc::UnboundedSender<AgentEvent>) {
    let (feed_tx, feed_rx) = mpsc::unbounded_channel();
    let feed = std::sync::Mutex::new(Some(feed_rx));
    let main_prompt = main_prompt.to_string();
    let harness = TestHarness::new(HarnessId::Mock, "Feed", move |request, mut controls| {
        if request.prompt != main_prompt {
            return script(vec![done_with(DoneStatus::Completed, None)]);
        }
        let mut feed = feed
            .lock()
            .unwrap()
            .take()
            .expect("FeedHarness serves the main dispatch once per test");
        if !confirm_steers {
            return Ok(channel_stream(feed));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut steering_open = true;
            loop {
                tokio::select! {
                    // Steers first: a Steered boundary always precedes feed
                    // events the test sends after steering (determinism).
                    biased;
                    steer = controls.steering.recv(), if steering_open => match steer {
                        Some(_) => {
                            let boundary = AgentEvent::Steered {
                                assistant_message_id: None,
                                next_assistant_message_id: None,
                            };
                            if tx.send(boundary).is_err() {
                                return;
                            }
                        }
                        None => steering_open = false,
                    },
                    event = feed.recv() => match event {
                        Some(event) => {
                            if tx.send(event).is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
        Ok(channel_stream(rx))
    })
    .steering();
    (harness, feed_tx)
}

/// A finished stream of `events`.
pub fn script(events: Vec<AgentEvent>) -> Result<EventStream, HarnessError> {
    Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
}

/// A completed one-line turn in the request's cwd (`mock-1`, assistant `a-1`).
pub fn reply(
    harness: HarnessId,
    request: &RunRequest,
    session_id: &str,
    text: &str,
) -> Result<EventStream, HarnessError> {
    script(vec![
        session_started(harness, "mock-1", &request.cwd, session_id, "a-1"),
        self::text(text),
        done(session_id),
    ])
}

/// A stream that yields whatever the test pushes into `rx`, ending when the
/// sender drops.
pub fn channel_stream(rx: mpsc::UnboundedReceiver<AgentEvent>) -> EventStream {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (Ok(event), rx))
    })
    .boxed()
}

pub fn session_started(
    harness: HarnessId,
    model: &str,
    cwd: &str,
    session_id: &str,
    assistant_message_id: &str,
) -> AgentEvent {
    AgentEvent::SessionStarted {
        harness,
        model: model.into(),
        tools: vec![],
        cwd: cwd.into(),
        session_id: session_id.into(),
        assistant_message_id: assistant_message_id.into(),
    }
}

pub fn text(s: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: s.into() }
}

/// A completed `Done` carrying `session_id`.
pub fn done(session_id: &str) -> AgentEvent {
    done_with(DoneStatus::Completed, Some(session_id))
}

pub fn done_with(status: DoneStatus, session_id: Option<&str>) -> AgentEvent {
    AgentEvent::Done {
        status,
        result: None,
        error: None,
        session_id: session_id.map(Into::into),
    }
}

/// A plain run request in `/tmp` with no harness/model override.
pub fn run_request(prompt: &str) -> RunRequest {
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

/// Assemble an engine at `dir` with `harnesses` registered.
pub fn assemble_with(
    dir: &Path,
    default: HarnessId,
    harnesses: Vec<Arc<dyn Harness>>,
) -> EngineCore {
    let registry = HarnessRegistry::new();
    for harness in harnesses {
        registry.register(harness);
    }
    EngineCore::assemble(dir, Arc::new(registry), default, None).expect("engine core assembles")
}

/// An engine at `dir` whose only (and default) harness is `harness`.
pub fn engine_at(dir: &Path, harness: impl Harness + 'static) -> EngineCore {
    let id = harness.id();
    assemble_with(dir, id, vec![Arc::new(harness)])
}

/// An engine in its own temp dir (kept alive by the rig).
pub struct Rig {
    pub core: EngineCore,
    pub dir: tempfile::TempDir,
}

/// A rig around [`feed_harness`], with the feed's sender.
pub struct FeedRig {
    pub core: EngineCore,
    pub feed: mpsc::UnboundedSender<AgentEvent>,
    pub dir: tempfile::TempDir,
}

pub fn feed_rig(main_prompt: &str, confirm_steers: bool) -> FeedRig {
    let (harness, feed) = feed_harness(main_prompt, confirm_steers);
    let Rig { core, dir } = rig(harness);
    FeedRig { core, feed, dir }
}

/// One-harness rig whose default harness is that harness.
pub fn rig(harness: impl Harness + 'static) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let core = engine_at(dir.path(), harness);
    Rig { core, dir }
}

/// Poll `predicate` until it holds (10s budget).
pub async fn wait_for(predicate: impl FnMut() -> bool, what: &str) {
    wait_for_within(predicate, what, Duration::from_secs(10)).await;
}

pub async fn wait_for_within(mut predicate: impl FnMut() -> bool, what: &str, budget: Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll `cond` by blocking the calling thread (400 × 25ms).
pub fn wait_blocking(cond: impl Fn() -> bool, what: &str) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// Tolerant transcript read: a snapshot mid-segment-write deserializes with
/// fields missing — treat that instant as "not yet" (empty).
pub fn entries(core: &EngineCore, chat: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

pub fn complete_assistant_count(core: &EngineCore, chat: &str) -> usize {
    entries(core, chat)
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

/// Every command in the chat's ledger as `(id, status, resolution)`.
pub fn command_statuses(
    core: &EngineCore,
    chat: &str,
) -> Vec<(String, SessionCommandStatus, Option<String>)> {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|h| h.doc().read_commands().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|c| (c.id, c.status, c.resolution))
        .collect()
}

pub fn status(core: &EngineCore, chat: &str) -> Option<SessionStatus> {
    core.sessions.session_status(chat).map(|s| s.status)
}

/// A one-text-part transcript entry (no comments, not a continuation).
pub fn message_entry(
    id: &str,
    role: MessageRole,
    text: &str,
    device_id: &str,
    status: Option<MessageStatus>,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: "t0".into(),
            text: text.into(),
            agent_text: None,
        }],
        created_at: 1,
        device_id: device_id.into(),
        status,
        continuation_of: None,
        completed_at: None,
        comments: Vec::new(),
    }
}

/// Run git in `cwd` under a fixed test identity, panicking with its stderr
/// on failure; returns trimmed stdout.
pub async fn git(cwd: &Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .await
        .expect("git spawns");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Init a repo at `dir` on `main` with one committed file `a.txt`.
pub async fn init_repo(dir: &Path, a_txt: &str) {
    std::fs::create_dir_all(dir).expect("repo dir");
    git(dir, &["init", "-b", "main"]).await;
    std::fs::write(dir.join("a.txt"), a_txt).expect("write a.txt");
    git(dir, &["add", "."]).await;
    git(dir, &["commit", "-m", "initial"]).await;
}

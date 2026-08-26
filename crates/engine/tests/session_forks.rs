//! Session Fork (v1) engine integration.
//!
//! - `ForkSession` with a client-minted request id creates a NEW durable root
//!   chat: target transcript = the source prefix (user anchor EXCLUDES the
//!   clicked message and prefills the composer; assistant anchor INCLUDES the
//!   clicked reply), mode + composer text in the reply, source metadata
//!   inherited (device/space/cwd/branch/checkout/config), NEW harness session
//!   path, `room_gen: 2`, `<source title> — Fork` title, no child metadata;
//! - the SOURCE chat, its row, its transcript, and its session are unchanged;
//! - the pi backend receives the source session path + stripped visible user
//!   prompts + the right boundary (BeforeUser ordinal / CloneLeaf);
//! - same-request retries return the SAME chat (no twin);
//! - typed Unavailable for non-Pi / child / live / missing-session /
//!   missing-host / boundary cases;
//! - `EngineCore` shutdown stays clean.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_engine::session_forks::PiForkBackend;
use cypher_engine::{EngineCore, SessionForks};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, ChatConfig, ChildAgentProfile, DoneStatus, HarnessId, Model, PiForkBoundary,
    PiSessionForkRequest, PiSessionForkResult, ReasoningLevel, RunRequest, SandboxLevel,
    SessionForkMode, SessionForkRequest, SessionForkResponse, SessionForkUnavailableReason,
    SessionStatus, SteeringMode, SubagentRunMode,
};
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};

const SOURCE: &str = "chat-source";

/// A Pi harness that records every fork request and returns a deterministic
/// fake new-session path UNDER the managed session root. Runs stream a quick
/// completed reply (like the Side Chat fixture), so dispatched sources park
/// Idle. Each run gets its own assistant message id (a-1, a-2, …) so
/// multi-turn transcripts have distinct settled assistant anchors.
struct RecordingHarness {
    fork_requests: Arc<Mutex<Vec<PiSessionForkRequest>>>,
    /// Explicit paths pushed by a test, popped in order; falls back to a
    /// `fork-<n>.jsonl` counter under the session root.
    session_paths: Arc<Mutex<Vec<String>>>,
    session_root: PathBuf,
    run_counter: std::sync::atomic::AtomicUsize,
}

impl RecordingHarness {
    fn new(session_root: PathBuf) -> Self {
        Self {
            fork_requests: Arc::new(Mutex::new(Vec::new())),
            session_paths: Arc::new(Mutex::new(Vec::new())),
            session_root,
            run_counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn next_path(&self) -> String {
        let mut paths = self.session_paths.lock().unwrap();
        let name = paths
            .pop()
            .unwrap_or_else(|| format!("fork-{}.jsonl", paths.len() + 1));
        self.session_root.join(name).to_string_lossy().into_owned()
    }
    fn next_assistant_id(&self) -> String {
        let n = self
            .run_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        format!("a-{n}")
    }
}

#[async_trait]
impl Harness for RecordingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Recording"
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
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let assistant_message_id = self.next_assistant_id();
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Pi,
                model: "model-x".into(),
                tools: vec![],
                cwd: "/tmp/repo".into(),
                session_id: "hs-source".into(),
                assistant_message_id: assistant_message_id.clone(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "reply text".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: Some("reply text".into()),
                error: None,
                session_id: None,
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        self.fork_requests.lock().unwrap().push(request.clone());
        // An EMPTY-CONTEXT fork BEFORE THE FIRST USER mirrors real pi: the
        // new session file is not persisted until the target's first send, so
        // no session path is returned (and nothing is materialized). Every
        // other boundary materializes the file, so the engine's orphan
        // cleanup (concurrent same-request race) is observable.
        let session_path = match request.boundary {
            PiForkBoundary::BeforeUser(0) => None,
            _ => {
                let path = self.next_path();
                if let Some(parent) = Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, b"{}");
                Some(path)
            }
        };
        Ok(PiSessionForkResult { session_path })
    }
}

/// A harness that starts a session but never completes the run (stays
/// Working forever) — the live-session fork rejection. Emitting
/// `SessionStarted` also stamps the chat's harness session id so the fork
/// validation gets past the MissingSession gate to the LiveSession one.
struct StuckHarness;

#[async_trait]
impl Harness for StuckHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Stuck"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Pi,
            model: "model-x".into(),
            tools: vec![],
            cwd: "/tmp/repo".into(),
            session_id: "hs-live".into(),
            assistant_message_id: "a-1".into(),
        })];
        Ok(futures::stream::iter(events)
            .chain(futures::stream::pending())
            .boxed())
    }
}

struct Rig {
    core: EngineCore,
    fork_requests: Arc<Mutex<Vec<PiSessionForkRequest>>>,
    pi_sessions_root: PathBuf,
    _dir: tempfile::TempDir,
}

fn assemble() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let registry = cypher_engine::HarnessRegistry::new();
    let harness = RecordingHarness::new(dir.path().join("agent-sessions"));
    let fork_requests = harness.fork_requests.clone();
    registry.register(Arc::new(harness));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None)
        .expect("engine core assembles");
    Rig {
        core,
        fork_requests,
        pi_sessions_root: dir.path().join("agent-sessions"),
        _dir: dir,
    }
}

async fn rpc(
    core: &EngineCore,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcError> {
    match core.rpc_service().handle(method, params).await? {
        RpcReply::Value(value) => Ok(value),
        _ => Err(RpcError::Failed("expected unary reply".into())),
    }
}

fn wait_for(cond: impl Fn() -> bool, what: &str) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

async fn fork(core: &EngineCore, request_id: &str, anchor: &str) -> SessionForkResponse {
    let value = rpc(
        core,
        methods::FORK_SESSION,
        serde_json::json!({
            "requestId": request_id,
            "sourceChatId": SOURCE,
            "anchorMessageId": anchor,
        }),
    )
    .await
    .expect("fork RPC succeeds");
    serde_json::from_value(value).expect("typed fork reply")
}

/// Seed the source chat row (Pi config, cwd, harness session) + a two-turn
/// transcript: u1 → a1 (parks Idle), u2 → a2 (parks Idle). The parked Idle
/// runs exercise the fork quiesce path.
async fn seed_source(core: &EngineCore) {
    core.workspace
        .create_chat(
            SOURCE,
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
        .expect("source chat row");
    for (message_id, prompt) in [("m1", "first question"), ("m2", "second question")] {
        core.sessions
            .dispatch(
                SOURCE,
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
                },
                Some(message_id.into()),
            )
            .await
            .expect("dispatch");
        wait_for(
            || {
                core.sessions
                    .session_status(SOURCE)
                    .is_some_and(|s| s.status == SessionStatus::Idle)
            },
            "turn to settle idle",
        );
    }
}

fn source_entries(core: &EngineCore) -> Vec<(String, cypher_doc::MessageRole)> {
    core.doc_host
        .open(SOURCE)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .into_iter()
        .map(|e| (e.id, e.role))
        .collect()
}

/// The id of the source entry at the given 0-based position.
fn source_entry_id(core: &EngineCore, ix: usize) -> String {
    source_entries(core)[ix].0.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn user_fork_excludes_the_clicked_message_and_prefills() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let source_before = source_entries(&rig.core);

    let response = fork(&rig.core, "fork-1", "m2").await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    assert_eq!(created.mode, SessionForkMode::EditUser);
    assert_eq!(created.composer_text.as_deref(), Some("second question"));
    assert_eq!(created.chat.id, "fork-1");
    assert_eq!(created.chat.device_id, rig.core.device_id);
    assert_eq!(created.chat.cwd.as_deref(), Some("/tmp/repo"));
    let source_title = rig
        .core
        .workspace
        .chat(SOURCE)
        .unwrap()
        .expect("source row")
        .title
        .unwrap_or_default();
    assert_eq!(
        created.chat.title.as_deref(),
        Some(format!("{source_title} — Fork").as_str())
    );
    // The fork's harness session is a fresh path UNDER the managed root.
    let session_path = created.chat.harness_session_id.as_deref().unwrap();
    assert!(
        std::fs::canonicalize(Path::new(session_path)).is_ok(),
        "fork session file should be materialized under the managed root: {session_path}"
    );
    assert!(
        session_path.contains("agent-sessions"),
        "fork session path under managed root: {session_path}"
    );
    assert_ne!(session_path, "hs-source");
    assert_eq!(
        created.chat.harness_session_cwd.as_deref(),
        Some("/tmp/repo")
    );
    assert_eq!(created.chat.room_gen, Some(2));
    assert!(created.chat.child.is_none());
    assert!(created.chat.config.is_some());

    // Target transcript = prefix BEFORE the clicked user: u1 + a1 only.
    let target = rig
        .core
        .doc_host
        .open("fork-1")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let first_assistant = source_entry_id(&rig.core, 1);
    assert_eq!(
        target.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["m1", first_assistant.as_str()]
    );

    // The backend saw the source session + stripped prompts + the right boundary.
    let reqs = rig.fork_requests.lock().unwrap();
    let req = reqs.last().expect("one fork request");
    assert_eq!(req.source_session_path, "hs-source");
    assert_eq!(
        req.visible_user_prompts,
        vec!["first question", "second question"]
    );
    assert_eq!(req.boundary, PiForkBoundary::BeforeUser(1));

    // Source unchanged.
    assert_eq!(source_entries(&rig.core), source_before);
    assert_eq!(source_entries(&rig.core).len(), 4);
    drop(reqs);
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_fork_includes_the_clicked_reply() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let first_assistant = source_entry_id(&rig.core, 1);

    let response = fork(&rig.core, "fork-2", &first_assistant).await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    assert_eq!(created.mode, SessionForkMode::ContinueAfterAssistant);
    assert_eq!(created.composer_text, None);
    let target = rig
        .core
        .doc_host
        .open("fork-2")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert_eq!(
        target.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["m1", first_assistant.as_str()]
    );
    let reqs = rig.fork_requests.lock().unwrap();
    assert_eq!(reqs.last().unwrap().boundary, PiForkBoundary::BeforeUser(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn last_assistant_clone_leaf() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let last_assistant = source_entry_id(&rig.core, 3);

    let response = fork(&rig.core, "fork-3", &last_assistant).await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    assert_eq!(created.mode, SessionForkMode::ContinueAfterAssistant);
    let target = rig
        .core
        .doc_host
        .open("fork-3")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let source_ids = source_entries(&rig.core);
    assert_eq!(
        target.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec![
            source_ids[0].0.as_str(),
            source_ids[1].0.as_str(),
            source_ids[2].0.as_str(),
            source_ids[3].0.as_str(),
        ]
    );
    let reqs = rig.fork_requests.lock().unwrap();
    assert_eq!(reqs.last().unwrap().boundary, PiForkBoundary::CloneLeaf);
}

#[tokio::test(flavor = "multi_thread")]
async fn same_request_retry_returns_one_chat() {
    let rig = assemble();
    seed_source(&rig.core).await;

    let first = fork(&rig.core, "fork-retry", "m2").await;
    let SessionForkResponse::Created(first_created) = &first else {
        panic!("expected Created, got {first:?}");
    };
    let second = fork(&rig.core, "fork-retry", "m2").await;
    let SessionForkResponse::Created(second_created) = &second else {
        panic!("expected Created, got {second:?}");
    };
    assert_eq!(first_created.chat.id, second_created.chat.id);
    assert_eq!(
        first_created.chat.harness_session_id,
        second_created.chat.harness_session_id
    );
    // One fork backend call, one target row, one target doc.
    assert_eq!(rig.fork_requests.lock().unwrap().len(), 1);
    let chats = rig.core.workspace.read_chats().unwrap();
    assert_eq!(chats.iter().filter(|c| c.id == "fork-retry").count(), 1);
}

/// A fork BEFORE THE FIRST USER is an EMPTY-CONTEXT fork: real pi does not
/// persist the new session file until the target's first user message lands,
/// so the backend returns NO session path and the target chat is born with
/// `harness_session_id: None` / `harness_session_cwd: None` — its first send
/// starts a FRESH Pi session from empty context. The target transcript is
/// EMPTY (nothing to copy before the first user) and the composer is
/// prefilled with the first user's visible text. A same-request retry stays
/// idempotent (one backend call, one target chat).
#[tokio::test(flavor = "multi_thread")]
async fn first_user_fork_is_empty_with_no_harness_session() {
    let rig = assemble();
    seed_source(&rig.core).await;

    let response = fork(&rig.core, "fork-first", "m1").await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    assert_eq!(created.mode, SessionForkMode::EditUser);
    assert_eq!(created.composer_text.as_deref(), Some("first question"));
    assert_eq!(created.chat.id, "fork-first");
    assert_eq!(created.chat.device_id, rig.core.device_id);
    assert_eq!(created.chat.cwd.as_deref(), Some("/tmp/repo"));

    // Empty transcript: nothing to copy before the first user.
    let target = rig
        .core
        .doc_host
        .open("fork-first")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert!(
        target.is_empty(),
        "first-user fork has an empty transcript: {target:?}"
    );

    // No persisted harness session: the target starts fresh on first send.
    assert_eq!(created.chat.harness_session_id, None);
    assert_eq!(created.chat.harness_session_cwd, None);
    assert_eq!(created.chat.room_gen, Some(2));
    assert!(created.chat.config.is_some());

    // The backend saw the empty-context boundary (BeforeUser(0)) with the
    // source session + stripped prompts.
    let reqs = rig.fork_requests.lock().unwrap();
    let req = reqs.last().expect("one fork request");
    assert_eq!(req.boundary, PiForkBoundary::BeforeUser(0));
    assert_eq!(req.source_session_path, "hs-source");
    assert_eq!(
        req.visible_user_prompts,
        vec!["first question", "second question"]
    );
    drop(reqs);

    // Retry: idempotent — the SAME chat, no second backend call, and the
    // session-less target validates as a legit first-user fork (not a
    // collision).
    let retry = fork(&rig.core, "fork-first", "m1").await;
    let SessionForkResponse::Created(retry_created) = &retry else {
        panic!("expected Created, got {retry:?}");
    };
    assert_eq!(retry_created.chat.id, "fork-first");
    assert_eq!(retry_created.chat.harness_session_id, None);
    assert_eq!(rig.fork_requests.lock().unwrap().len(), 1);
    let chats = rig.core.workspace.read_chats().unwrap();
    assert_eq!(chats.iter().filter(|c| c.id == "fork-first").count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn non_pi_source_is_unavailable() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            SOURCE,
            None,
            Some(rig.core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::ClaudeCode,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .unwrap();
    let response = fork(&rig.core, "fork-x", "m1").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u) if u.reason == SessionForkUnavailableReason::NonPi
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn child_chat_is_unavailable() {
    let rig = assemble();
    // A plain Pi parent row, then a Cypher child chat under it.
    rig.core
        .workspace
        .create_chat(
            "parent",
            None,
            Some(rig.core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .unwrap();
    let parent = rig
        .core
        .workspace
        .chat("parent")
        .unwrap()
        .expect("parent row");
    let child_outcome = rig
        .core
        .workspace
        .create_child_chat(
            &parent,
            "run-1",
            "agent",
            "task",
            SubagentRunMode::Async,
            None,
            ChildAgentProfile {
                system_prompt: "p".into(),
                tools: vec![],
                model: None,
                thinking: None,
            },
            "child title",
        )
        .unwrap();
    let child_id = child_outcome.id().to_string();
    let value = rpc(
        &rig.core,
        methods::FORK_SESSION,
        serde_json::json!({
            "requestId": "fork-child",
            "sourceChatId": child_id,
            "anchorMessageId": "m1",
        }),
    )
    .await
    .unwrap();
    let response: SessionForkResponse = serde_json::from_value(value).unwrap();
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::ChildChat
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn live_session_is_unavailable() {
    let registry = cypher_engine::HarnessRegistry::new();
    registry.register(Arc::new(StuckHarness));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None).unwrap();
    core.workspace
        .create_chat(
            SOURCE,
            None,
            Some(core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .unwrap();
    core.sessions
        .dispatch(
            SOURCE,
            HarnessId::Pi,
            RunRequest {
                prompt: "question".into(),
                harness: Some(HarnessId::Pi),
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: "/tmp/repo".into(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                resume: None,
                worktree: None,
                attachments: Vec::new(),
            },
            Some("m1".into()),
        )
        .await
        .unwrap();
    wait_for(
        || {
            let working = core
                .sessions
                .session_status(SOURCE)
                .is_some_and(|s| s.status == SessionStatus::Working);
            let sessioned = core
                .workspace
                .chat(SOURCE)
                .unwrap()
                .is_some_and(|c| c.harness_session_id.as_deref() == Some("hs-live"));
            working && sessioned
        },
        "run to go Working with a stored session",
    );
    let value = rpc(
        &core,
        methods::FORK_SESSION,
        serde_json::json!({
            "requestId": "fork-live",
            "sourceChatId": SOURCE,
            "anchorMessageId": "m1",
        }),
    )
    .await
    .unwrap();
    let response: SessionForkResponse = serde_json::from_value(value).unwrap();
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::LiveSession
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_harness_session_is_unavailable() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            SOURCE,
            None,
            Some(rig.core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .unwrap();
    let response = fork(&rig.core, "fork-nosession", "m1").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::MissingSession
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_host_is_unavailable() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            SOURCE,
            None,
            Some("other-device".into()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .unwrap();
    let response = fork(&rig.core, "fork-remote", "m1").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::MissingHost
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_anchor_is_unavailable() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let response = fork(&rig.core, "fork-anchor", "nope").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::BoundaryUnavailable
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn temporary_side_chat_is_unavailable() {
    let rig = assemble();
    seed_source(&rig.core).await;
    // Simulate a temporary (host-memory) Side Chat: registered ephemeral, so
    // it must be refused even though its row exists.
    rig.core.sessions.register_ephemeral(SOURCE);
    let response = fork(&rig.core, "fork-temp", "m1").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::TemporarySideChat
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn source_session_file_never_deleted_on_success() {
    // A successful fork keeps the source session + the NEW one; only a
    // failed promotion deletes the NEW one (managed-root guarded — the
    // source is never touched). Uses a SECOND-user anchor so the fork
    // materializes a real session file (a first-user fork returns None).
    let rig = assemble();
    seed_source(&rig.core).await;
    let response = fork(&rig.core, "fork-safe", "m2").await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    // The source session file is untouched (the fake backend only ever
    // materializes the NEW fork file; nothing deleted it).
    let new_path = created.chat.harness_session_id.as_deref().unwrap();
    assert!(std::path::Path::new(new_path).exists());
    // Exactly ONE fork file remains under the managed root (no orphan).
    let forks = std::fs::read_dir(&rig.pi_sessions_root)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("fork-"))
        .count();
    assert_eq!(forks, 1, "exactly one fork session file remains");
}

/// A backend seam that fails the fork with a fixed [`HarnessError`] once.
struct FailingBackend {
    error: Arc<Mutex<Option<HarnessError>>>,
}

impl FailingBackend {
    fn new(error: HarnessError) -> Self {
        Self {
            error: Arc::new(Mutex::new(Some(error))),
        }
    }
}

#[async_trait]
impl PiForkBackend for FailingBackend {
    async fn fork_session(
        &self,
        _request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        Err(self.error.lock().unwrap().take().expect("single failure"))
    }
}

/// A backend whose `fork_session` blocks until BOTH concurrent calls arrive
/// (so the same-request race deterministically runs the backend twice), then
/// materializes a distinct session file per call.
struct BarrierBackend {
    session_root: PathBuf,
    barrier: Arc<tokio::sync::Barrier>,
    requests: Arc<Mutex<Vec<PiSessionForkRequest>>>,
    counter: std::sync::atomic::AtomicUsize,
}

impl BarrierBackend {
    fn new(session_root: PathBuf, barrier: Arc<tokio::sync::Barrier>) -> Self {
        Self {
            session_root,
            barrier,
            requests: Arc::new(Mutex::new(Vec::new())),
            counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl PiForkBackend for BarrierBackend {
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        self.requests.lock().unwrap().push(request.clone());
        self.barrier.wait().await;
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let path = self.session_root.join(format!("fork-concurrent-{n}.jsonl"));
        let _ = std::fs::create_dir_all(&self.session_root);
        let _ = std::fs::write(&path, b"{}");
        Ok(PiSessionForkResult {
            session_path: Some(path.to_string_lossy().into_owned()),
        })
    }
}

/// A harness that completes runs normally until `stuck` is flipped, then
/// stays Working forever — lets a test create a fork while the source is
/// Idle, then flip the source to Working BEFORE the retry.
struct ToggleHarness {
    fork_requests: Arc<Mutex<Vec<PiSessionForkRequest>>>,
    session_root: PathBuf,
    stuck: Arc<std::sync::atomic::AtomicBool>,
    run_counter: std::sync::atomic::AtomicUsize,
}

impl ToggleHarness {
    fn new(session_root: PathBuf) -> Self {
        Self {
            fork_requests: Arc::new(Mutex::new(Vec::new())),
            session_root,
            stuck: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            run_counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Harness for ToggleHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Toggle"
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
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let n = self
            .run_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let assistant_message_id = format!("a-{n}");
        let start = Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Pi,
            model: "model-x".into(),
            tools: vec![],
            cwd: "/tmp/repo".into(),
            session_id: "hs-toggle".into(),
            assistant_message_id: assistant_message_id.clone(),
        });
        if self.stuck.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(futures::stream::iter(vec![start])
                .chain(futures::stream::pending())
                .boxed());
        }
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            start,
            Ok(AgentEvent::TextDelta {
                text: "reply text".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: Some("reply text".into()),
                error: None,
                session_id: None,
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
    async fn fork_session(
        &self,
        request: PiSessionForkRequest,
    ) -> Result<PiSessionForkResult, HarnessError> {
        self.fork_requests.lock().unwrap().push(request.clone());
        let path = self.session_root.join("fork-toggle.jsonl");
        let _ = std::fs::write(&path, b"{}");
        Ok(PiSessionForkResult {
            session_path: Some(path.to_string_lossy().into_owned()),
        })
    }
}

/// Expected backend errors become typed Unavailable responses: a missing Pi
/// CLI on the hosting device is Unsupported with actionable guidance.
#[tokio::test(flavor = "multi_thread")]
async fn backend_not_installed_is_typed_unsupported() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let forks = SessionForks::with_backend(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        rig.pi_sessions_root.clone(),
        Arc::new(FailingBackend::new(HarnessError::NotInstalled("pi".into()))),
    );
    let response = forks
        .fork(SessionForkRequest {
            request_id: "fork-be-notinstalled".into(),
            source_chat_id: SOURCE.into(),
            anchor_message_id: "m2".into(),
        })
        .await
        .expect("typed reply");
    let SessionForkResponse::Unavailable(u) = &response else {
        panic!("expected Unavailable, got {response:?}");
    };
    assert_eq!(u.reason, SessionForkUnavailableReason::Unsupported);
    assert!(
        u.message.to_lowercase().contains("install") && u.message.contains("device"),
        "actionable update/install message: {}",
        u.message
    );
}

/// A prompt-mapping protocol refusal from the backend is BoundaryUnavailable,
/// never a generic RPC failure.
#[tokio::test(flavor = "multi_thread")]
async fn backend_mapping_protocol_error_is_boundary_unavailable() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let forks = SessionForks::with_backend(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        rig.pi_sessions_root.clone(),
        Arc::new(FailingBackend::new(HarnessError::Protocol(
            "session fork mapping: ambiguous — refusing positional guess".into(),
        ))),
    );
    let response = forks
        .fork(SessionForkRequest {
            request_id: "fork-be-mapping".into(),
            source_chat_id: SOURCE.into(),
            anchor_message_id: "m2".into(),
        })
        .await
        .expect("typed reply");
    let SessionForkResponse::Unavailable(u) = &response else {
        panic!("expected Unavailable, got {response:?}");
    };
    assert_eq!(u.reason, SessionForkUnavailableReason::BoundaryUnavailable);
}

/// A genuine I/O failure from the backend stays an EngineError (surfaces as
/// an RPC `Failed`, not a typed Unavailable).
#[tokio::test(flavor = "multi_thread")]
async fn backend_io_error_is_engine_error() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let forks = SessionForks::with_backend(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        rig.pi_sessions_root.clone(),
        Arc::new(FailingBackend::new(HarnessError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "boom",
        )))),
    );
    let err = forks
        .fork(SessionForkRequest {
            request_id: "fork-be-io".into(),
            source_chat_id: SOURCE.into(),
            anchor_message_id: "m2".into(),
        })
        .await
        .expect_err("I/O failure surfaces as EngineError");
    assert!(err.to_string().contains("boom"), "{err}");
}

/// A lost-reply retry returns the EXISTING target chat even when the source
/// is now running — the idempotence check runs before the LiveSession gate
/// and never re-invokes the backend.
#[tokio::test(flavor = "multi_thread")]
async fn lost_reply_retry_returns_existing_even_when_source_live() {
    let registry = cypher_engine::HarnessRegistry::new();
    let dir = tempfile::tempdir().unwrap();
    let harness = ToggleHarness::new(dir.path().join("agent-sessions"));
    let fork_requests = harness.fork_requests.clone();
    let stuck = harness.stuck.clone();
    registry.register(Arc::new(harness));
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None).unwrap();
    core.workspace
        .create_chat(
            SOURCE,
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
        .expect("source chat row");
    core.sessions
        .dispatch(
            SOURCE,
            HarnessId::Pi,
            RunRequest {
                prompt: "first question".into(),
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
            },
            Some("m1".into()),
        )
        .await
        .expect("dispatch");
    wait_for(
        || {
            core.sessions
                .session_status(SOURCE)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "first turn to settle idle",
    );

    // Create the fork while the source is Idle.
    let first = fork(&core, "fork-live-retry", "m1").await;
    assert!(matches!(first, SessionForkResponse::Created(_)));
    assert_eq!(fork_requests.lock().unwrap().len(), 1);

    // Flip the source to Working (never settles) and retry.
    stuck.store(true, std::sync::atomic::Ordering::Relaxed);
    core.sessions
        .dispatch(
            SOURCE,
            HarnessId::Pi,
            RunRequest {
                prompt: "second question".into(),
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
            },
            Some("m2".into()),
        )
        .await
        .expect("dispatch");
    wait_for(
        || {
            core.sessions
                .session_status(SOURCE)
                .is_some_and(|s| s.status == SessionStatus::Working)
        },
        "turn to go Working",
    );

    let retry = fork(&core, "fork-live-retry", "m1").await;
    let SessionForkResponse::Created(created) = &retry else {
        panic!("retry must return the existing fork, got {retry:?}");
    };
    assert_eq!(created.chat.id, "fork-live-retry");
    assert_eq!(
        fork_requests.lock().unwrap().len(),
        1,
        "idempotent retry must not re-run the backend"
    );
}

/// `requestId == sourceChatId` is rejected outright — the target would shadow
/// the source row.
#[tokio::test(flavor = "multi_thread")]
async fn request_id_equal_to_source_chat_is_rejected() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let response = fork(&rig.core, SOURCE, "m2").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::BoundaryUnavailable
    ));
    assert_eq!(rig.fork_requests.lock().unwrap().len(), 0);
}

/// An existing chat under the request id that does NOT match the source
/// (different cwd) is a collision — refused, never returned as the fork.
#[tokio::test(flavor = "multi_thread")]
async fn existing_target_mismatching_source_is_refused() {
    let rig = assemble();
    seed_source(&rig.core).await;
    rig.core
        .workspace
        .create_chat(
            "fork-collide",
            None,
            Some(rig.core.device_id.as_str()),
            Some(ChatConfig {
                harness: HarnessId::Pi,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/other/dir".into()),
        )
        .unwrap();
    let response = fork(&rig.core, "fork-collide", "m2").await;
    assert!(matches!(
        response,
        SessionForkResponse::Unavailable(ref u)
            if u.reason == SessionForkUnavailableReason::BoundaryUnavailable
    ));
    assert_eq!(rig.fork_requests.lock().unwrap().len(), 0);
}

/// Two concurrent forks with the SAME request id: the backend may run twice,
/// but only ONE target chat is published and the losing call's orphan session
/// file is best-effort deleted under the managed root.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_same_request_publishes_once_and_cleans_the_orphan() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let backend = Arc::new(BarrierBackend::new(
        rig.pi_sessions_root.clone(),
        barrier.clone(),
    ));
    let forks = SessionForks::with_backend(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        rig.pi_sessions_root.clone(),
        backend.clone(),
    );
    let req = || SessionForkRequest {
        request_id: "fork-concurrent".into(),
        source_chat_id: SOURCE.into(),
        anchor_message_id: "m2".into(),
    };
    let (r1, r2) = tokio::join!(forks.fork(req()), forks.fork(req()));
    assert!(matches!(
        r1.expect("first reply"),
        SessionForkResponse::Created(_)
    ));
    assert!(matches!(
        r2.expect("second reply"),
        SessionForkResponse::Created(_)
    ));
    // The backend legitimately ran twice (both raced the helper).
    assert_eq!(backend.requests.lock().unwrap().len(), 2);
    // Exactly ONE target chat row + doc.
    let chats = rig.core.workspace.read_chats().unwrap();
    assert_eq!(
        chats.iter().filter(|c| c.id == "fork-concurrent").count(),
        1
    );
    // Exactly ONE session file remains (the loser's orphan was deleted), and
    // it is the one the surviving chat references.
    let chat = rig
        .core
        .workspace
        .chat("fork-concurrent")
        .unwrap()
        .expect("target row");
    let referenced = PathBuf::from(chat.harness_session_id.as_deref().unwrap());
    let files: Vec<PathBuf> = std::fs::read_dir(&rig.pi_sessions_root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("fork-concurrent-"))
        })
        .collect();
    assert_eq!(files.len(), 1, "orphan session cleaned: {files:?}");
    assert_eq!(files[0], referenced);
}

/// The fork row is stamped as NEW sidebar activity (birth `now` for both
/// `last_message_at` and `last_seen_at`) while preserving the endpoint
/// preview from the newest copied entry — never buried under the source's
/// old transcript timestamp.
#[tokio::test(flavor = "multi_thread")]
async fn fork_is_stamped_as_new_sidebar_activity() {
    let rig = assemble();
    seed_source(&rig.core).await;
    let before = chrono::Utc::now();
    let response = fork(&rig.core, "fork-fresh", "m2").await;
    let SessionForkResponse::Created(created) = &response else {
        panic!("expected Created, got {response:?}");
    };
    let after = chrono::Utc::now();
    let last_at = created.chat.last_message_at.expect("fresh timestamp");
    assert!(
        last_at >= before - chrono::Duration::seconds(5)
            && last_at <= after + chrono::Duration::seconds(5),
        "last_message_at is the creation time, got {last_at}"
    );
    // Same birth `now` for last_seen_at (never flashes an unseen badge).
    let seen = created.chat.last_seen_at.expect("seen on birth");
    assert!(
        (seen - last_at).num_milliseconds().abs() < 1000,
        "last_seen_at == last_message_at == birth now"
    );
    // Endpoint preview preserved from the NEWEST copied entry (a1's reply).
    assert_eq!(
        created.chat.last_message_preview.as_deref(),
        Some("reply text")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn forwardable_marks_fork_session() {
    // FORK_SESSION is device-addressable (the source host owns the session).
    let registry = cypher_engine::HarnessRegistry::new();
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Pi, None).unwrap();
    let reply = rpc(
        &core,
        methods::FORK_SESSION,
        serde_json::json!({
            "requestId": "fork-fwd",
            "sourceChatId": "remote-chat",
            "anchorMessageId": "m1",
            "targetDeviceId": "other-device",
        }),
    )
    .await;
    // No links attached: forwarding is unavailable (offline), NOT UnknownMethod
    // — proving the method is recognized and routed as forwardable.
    let err = reply.err().expect("forward attempt fails without links");
    assert!(
        err.to_string().contains("remote routing unavailable")
            || err.to_string().contains("cannot reach device"),
        "{err}"
    );
}

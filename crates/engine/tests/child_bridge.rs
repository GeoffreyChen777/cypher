//! Cypher child-subagent bridge (`StartSubagent` + `WatchAgentEvents`):
//! - `StartSubagent` idempotently creates the same-device child Chat with
//!   additive Cypher-owned metadata (parent chat id + parent run id + agent /
//!   task / mode + persisted profile + channel), Pi config, and a titled row;
//!   queues the normal durable Run command; returns `childChatId`.
//! - strict bounded params reject oversized frames before any row is written;
//! - a missing parent or a parent hosted elsewhere is rejected;
//! - the child run executes through the normal harness and its terminal
//!   `Done` is observable through the replayable `WatchAgentEvents` stream;
//! - parent `DeleteChat` cascades to child rows/docs/session interruption.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use cypher_engine::{EngineCore, HarnessRegistry};
use cypher_harness::{Harness, HarnessError, RunControls};
use cypher_proto::{
    AgentEvent, ChildAgentProfile, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
    SandboxLevel, SessionStatus, SteeringMode, SubagentRunMode,
};
use cypher_rpc::{RpcError, RpcReply, RpcService, methods};

const PARENT: &str = "chat-parent";

fn done_ok() -> AgentEvent {
    AgentEvent::Done {
        status: DoneStatus::Completed,
        result: Some("planner result text".into()),
        error: None,
        session_id: None,
    }
}

/// One-liner harness serving every harness id (Pi included): the child run
/// streams a fixed sequence ending in Done with the final result text.
struct OneLinerHarness;

#[async_trait]
impl Harness for OneLinerHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "OneLiner"
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
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Pi,
                model: "anthropic/claude-sonnet-4".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "child-hs".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "planner result text".into(),
            }),
            Ok(done_ok()),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

struct Rig {
    core: EngineCore,
    _dir: tempfile::TempDir,
}

fn assemble() -> Rig {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(OneLinerHarness));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    Rig { core, _dir: dir }
}

fn start_params(run_id: &str, parent: &str) -> serde_json::Value {
    serde_json::json!({
        "parentChatId": parent,
        "runId": run_id,
        "agent": "planner",
        "task": "Plan the panel",
        "mode": "async",
        "systemPrompt": "You are the planner.",
        "tools": ["read", "bash"],
        "model": "anthropic/claude-sonnet-4",
        "messageRoot": "/tmp/pi-subagents-messages/abc",
        "childIndex": 0,
    })
}

async fn start(core: &EngineCore, run_id: &str, parent: &str) -> Result<String, RpcError> {
    let reply = core
        .rpc_service()
        .handle(methods::START_SUBAGENT, start_params(run_id, parent))
        .await?;
    let RpcReply::Value(value) = reply else {
        panic!("StartSubagent must be unary");
    };
    value
        .get("childChatId")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| RpcError::Failed("no childChatId in reply".into()))
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

#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_creates_child_and_queues_run() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            PARENT,
            None,
            Some(rig.core.device_id.as_str()),
            Some(cypher_proto::ChatConfig {
                harness: HarnessId::Pi,
                model: Some("parent-model".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
            }),
            Some("/tmp/repo".into()),
        )
        .expect("parent chat");

    let child_id = start(&rig.core, "run-1", PARENT).await.expect("start ok");
    let child = rig
        .core
        .workspace
        .chat(&child_id)
        .expect("chat read")
        .expect("child row exists");
    assert!(child.is_child(), "child row carries child metadata");
    assert_eq!(child.parent_chat_id(), Some(PARENT));
    assert_eq!(child.device_id, rig.core.device_id, "same-device child");
    assert_eq!(
        child.cwd.as_deref(),
        Some("/tmp/repo"),
        "inherits parent cwd"
    );
    assert_eq!(child.space_id, None, "project-less parent has no space");
    let meta = child.child.expect("metadata");
    assert_eq!(meta.agent, "planner");
    assert_eq!(meta.parent_run_id, "run-1");
    assert_eq!(meta.task, "Plan the panel");
    assert_eq!(meta.tool_call_id, None);
    assert_eq!(meta.profile.system_prompt, "You are the planner.");
    assert_eq!(meta.profile.tools, vec!["read", "bash"]);
    assert_eq!(
        meta.profile.model.as_deref(),
        Some("anthropic/claude-sonnet-4")
    );
    // The messaging channel root is HOST-LOCAL — never persisted on the synced
    // child row (an absolute /tmp path would be stale after a reboot and leak
    // across the sync boundary). The profile alone is the durable child env.
    let serialized = serde_json::to_value(&meta).expect("child metadata serializes");
    assert!(
        serialized.get("messageRoot").is_none() && serialized.get("channel").is_none(),
        "no host-local channel path on the persisted child row"
    );
    let config = child.config.expect("Pi-configured");
    assert_eq!(config.harness, HarnessId::Pi);
    assert_eq!(config.model.as_deref(), Some("anthropic/claude-sonnet-4"));

    // The queued Run executes through the normal harness: the child chat ends
    // up with a user entry + a terminal assistant entry (harness emits Done).
    wait_for(
        || {
            rig.core
                .doc_host
                .open(&child_id)
                .ok()
                .and_then(|h| h.doc().read_entries().ok())
                .is_some_and(|entries| {
                    entries.iter().any(|e| {
                        e.role == cypher_doc::MessageRole::Assistant
                            && e.parts.iter().any(|p| {
                                matches!(p, cypher_doc::MessagePart::Text { text, .. } if text.contains("planner result text"))
                            })
                    })
                })
        },
        "child run to stream its terminal result",
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_is_idempotent_by_parent_and_run() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            PARENT,
            None,
            Some(rig.core.device_id.as_str()),
            None,
            Some("/tmp".into()),
        )
        .expect("parent chat");

    let first = start(&rig.core, "run-1", PARENT)
        .await
        .expect("first start");
    let second = start(&rig.core, "run-1", PARENT)
        .await
        .expect("second start");
    assert_eq!(
        first, second,
        "same (parent, run) returns the existing child"
    );
    let children = rig
        .core
        .workspace
        .child_chats(PARENT)
        .expect("children list");
    assert_eq!(children.len(), 1, "no duplicate child rows");

    // A different run id mints a distinct child.
    let other = start(&rig.core, "run-2", PARENT)
        .await
        .expect("other start");
    assert_ne!(first, other);
    let children = rig
        .core
        .workspace
        .child_chats(PARENT)
        .expect("children list");
    assert_eq!(children.len(), 2);
    rig.core.shutdown().await;
}

/// CRITICAL: a retried `StartSubagent` for the same `(parentChatId, runId)`
/// must NOT queue a second initial Run — the durable command plane holds one
/// run command, so the child doc ends up with EXACTLY ONE user prompt entry.
#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_retry_queues_exactly_one_run() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            PARENT,
            None,
            Some(rig.core.device_id.as_str()),
            None,
            Some("/tmp".into()),
        )
        .expect("parent chat");

    // Two StartSubagent calls, back to back (before the first run even
    // dispatches — the idempotence must hold at ANY interleaving).
    let a = start(&rig.core, "run-1", PARENT).await.expect("first");
    let b = start(&rig.core, "run-1", PARENT).await.expect("retry");
    assert_eq!(a, b);

    // Let the (single) queued run execute through the harness.
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&a)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "child run to settle",
    );
    let entries = rig
        .core
        .doc_host
        .open(&a)
        .expect("doc open")
        .doc()
        .read_entries()
        .expect("entries");
    let user_entries = entries
        .iter()
        .filter(|e| e.role == cypher_doc::MessageRole::User)
        .count();
    assert_eq!(
        user_entries, 1,
        "two StartSubagent calls must produce exactly one user prompt/run command"
    );
    rig.core.shutdown().await;
}

/// CONCURRENCY: N racing `StartSubagent` calls for the same (parent, run)
/// must serialize through the engine-side mutex — one child row, and exactly
/// one initial Run queued (never a twin from the read-then-create scan race,
/// never a double-queue).
#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_concurrent_calls_make_one_child_one_run() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(
            PARENT,
            None,
            Some(rig.core.device_id.as_str()),
            None,
            Some("/tmp".into()),
        )
        .expect("parent chat");

    // Fire 8 concurrent starts before any run dispatches. All borrow the same
    // core; the RPC mutex serializes their critical sections.
    let futures: Vec<_> = (0..8).map(|_| start(&rig.core, "run-1", PARENT)).collect();
    let results = futures::future::join_all(futures).await;
    for result in &results {
        result.as_ref().expect("every concurrent start succeeds");
    }
    let child_ids: std::collections::HashSet<String> =
        results.into_iter().map(|r| r.expect("start ok")).collect();
    assert_eq!(
        child_ids.len(),
        1,
        "concurrent starts of the same (parent, run) must mint exactly one child"
    );
    let child_id = child_ids.into_iter().next().expect("one child");
    let children = rig
        .core
        .workspace
        .child_chats(PARENT)
        .expect("children list");
    assert_eq!(children.len(), 1, "no duplicate child rows");

    // Let the (single) queued run execute, then count user prompt entries.
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&child_id)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "the single child run to settle",
    );
    let entries = rig
        .core
        .doc_host
        .open(&child_id)
        .expect("doc open")
        .doc()
        .read_entries()
        .expect("entries");
    let user_entries = entries
        .iter()
        .filter(|e| e.role == cypher_doc::MessageRole::User)
        .count();
    assert_eq!(
        user_entries, 1,
        "8 concurrent starts must queue exactly one initial run"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_validates_parent_and_bounds() {
    let rig = assemble();

    // Missing parent.
    let err = start(&rig.core, "run-1", "no-such-parent")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("parent chat not found"), "{err}");

    // Oversized task / runId / systemPrompt are rejected before any write.
    rig.core
        .workspace
        .create_chat(PARENT, None, Some(rig.core.device_id.as_str()), None, None)
        .expect("parent chat");
    let mut params = start_params("run-1", PARENT);
    params["task"] = serde_json::json!("x".repeat(501));
    let err = rig
        .core
        .rpc_service()
        .handle(methods::START_SUBAGENT, params)
        .await
        .err()
        .expect("start must be rejected");
    assert!(err.to_string().contains("task too long"), "{err}");

    let mut params = start_params("run-1", PARENT);
    params["runId"] = serde_json::json!("r".repeat(201));
    let err = rig
        .core
        .rpc_service()
        .handle(methods::START_SUBAGENT, params)
        .await
        .err()
        .expect("start must be rejected");
    assert!(err.to_string().contains("runId too long"), "{err}");

    let mut params = start_params("run-1", PARENT);
    params["systemPrompt"] = serde_json::json!("p".repeat(70 * 1024));
    let err = rig
        .core
        .rpc_service()
        .handle(methods::START_SUBAGENT, params)
        .await
        .err()
        .expect("start must be rejected");
    assert!(err.to_string().contains("systemPrompt too large"), "{err}");

    // Optional model/thinking are bounded too.
    let mut params = start_params("run-1", PARENT);
    params["model"] = serde_json::json!("m".repeat(201));
    let err = rig
        .core
        .rpc_service()
        .handle(methods::START_SUBAGENT, params)
        .await
        .err()
        .expect("start must be rejected");
    assert!(err.to_string().contains("model invalid"), "{err}");

    let mut params = start_params("run-1", PARENT);
    params["thinking"] = serde_json::json!("t".repeat(33));
    let err = rig
        .core
        .rpc_service()
        .handle(methods::START_SUBAGENT, params)
        .await
        .err()
        .expect("start must be rejected");
    assert!(err.to_string().contains("thinking invalid"), "{err}");

    // No child row was minted by any rejected frame.
    assert!(
        rig.core
            .workspace
            .child_chats(PARENT)
            .expect("children list")
            .is_empty()
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_refuses_a_parent_hosted_elsewhere() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(PARENT, None, Some("remote-device".into()), None, None)
        .expect("parent chat");
    let err = start(&rig.core, "run-1", PARENT).await.unwrap_err();
    assert!(
        err.to_string().contains("not hosted on this device"),
        "{err}"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_agent_events_replays_terminal_done() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(PARENT, None, Some(rig.core.device_id.as_str()), None, None)
        .expect("parent chat");
    let child_id = start(&rig.core, "run-1", PARENT).await.expect("start ok");

    // The harness streams its fixed sequence immediately — wait for the child
    // run to settle (its session flips Idle after Done).
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&child_id)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "child run to settle",
    );

    // WatchAgentEvents replays from the journal: the stream must contain a
    // terminal Done whose result carries the child's final text.
    let reply = rig
        .core
        .rpc_service()
        .handle(
            methods::WATCH_AGENT_EVENTS,
            serde_json::json!({ "chatId": child_id }),
        )
        .await
        .expect("watch starts");
    let RpcReply::Stream(stream) = reply else {
        panic!("WatchAgentEvents must stream");
    };
    // The stream is live (never ends), so drain until the terminal Done.
    let mut stream = Box::pin(stream);
    let done = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(item) = stream.next().await {
            if item.get("type").and_then(|t| t.as_str()) == Some("done") {
                return item;
            }
        }
        panic!("stream ended before a Done event");
    })
    .await
    .expect("a Done event is replayed within 5s");
    assert_eq!(
        done.get("result").and_then(|r| r.as_str()),
        Some("planner result text")
    );
    rig.core.shutdown().await;
}

/// A harness that records the `RunControls.host` it received (the engine's
/// child-env seam) so a test can assert the child run got the persisted
/// profile + chat identity, while the harness itself streams a quick Done.
struct CapturingHarness {
    host: Arc<Mutex<Option<cypher_harness::RunHostContext>>>,
}

#[async_trait]
impl Harness for CapturingHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }
    fn display_name(&self) -> &str {
        "Capture"
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
        *self.host.lock().unwrap() = Some(controls.host);
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Pi,
                model: "anthropic/claude-sonnet-4".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "child-hs".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "child output".into(),
            }),
            Ok(done_ok()),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

/// The engine builds the child run's `RunHostContext` from the persisted
/// `ChildChat` metadata (never from `RunRequest`): the child chat's run sees
/// its own chat id and the full child env (system prompt / tools / model /
/// channel), which the pi harness injects as env + CLI flags.
#[tokio::test(flavor = "multi_thread")]
async fn child_runs_receive_child_env_via_host_context() {
    let host = Arc::new(Mutex::new(None));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CapturingHarness { host: host.clone() }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.workspace
        .create_chat(PARENT, None, Some(core.device_id.as_str()), None, None)
        .expect("parent chat");
    let child_id = start(&core, "run-1", PARENT).await.expect("start ok");
    wait_for(
        || host.lock().unwrap().is_some(),
        "child run to reach the harness",
    );
    let received = host.lock().unwrap().clone().expect("host captured");
    assert_eq!(received.chat_id.as_deref(), Some(child_id.as_str()));
    let child_env = received.child.expect("child env present");
    assert_eq!(child_env.system_prompt, "You are the planner.");
    assert_eq!(child_env.tools, vec!["read", "bash"]);
    assert_eq!(
        child_env.model.as_deref(),
        Some("anthropic/claude-sonnet-4")
    );
    // The messaging channel is engine-LOCAL: the initial run gets it from the
    // runtime map (registered by StartSubagent, never synced/persisted).
    assert_eq!(
        child_env.channel_root.as_deref(),
        Some("/tmp/pi-subagents-messages/abc")
    );
    assert_eq!(child_env.run_id, "run-1");
    assert_eq!(child_env.agent, "planner");
    assert_eq!(child_env.child_index, 0);
    // The channel is consumed at first dispatch (one-shot for the initial
    // run) — later child turns have no channel.
    assert!(
        core.sessions.take_child_channel(&child_id).is_none(),
        "the local channel is consumed by the initial run"
    );
    core.shutdown().await;
}

/// ORPHAN RECOVERY: an existing child whose initial Run never made it into
/// the durable ledger (the crash gap between row creation and queueing, or a
/// child whose only run command was rejected/expired/cancelled) must be
/// recovered by a retried `StartSubagent` — the fresh local channel is
/// registered and exactly one initial Run is queued (never skipped as an
/// ordinary retry, never double-queued).
#[tokio::test(flavor = "multi_thread")]
async fn start_subagent_recovers_an_orphan_existing_child() {
    let host = Arc::new(Mutex::new(None));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(CapturingHarness { host: host.clone() }));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.workspace
        .create_chat(
            PARENT,
            None,
            Some(core.device_id.as_str()),
            None,
            Some("/tmp".into()),
        )
        .expect("parent chat");
    let parent = core
        .workspace
        .chat(PARENT)
        .expect("read")
        .expect("parent exists");

    // The crash gap: the child row exists (a StartSubagent died between
    // create_child_chat and queue_command) but no initial Run was ever queued
    // — no Pending/Applied Run command, no dispatch evidence.
    let orphan = core
        .workspace
        .create_child_chat(
            &parent,
            "run-1",
            "planner",
            "Plan the panel",
            SubagentRunMode::Async,
            None,
            ChildAgentProfile {
                system_prompt: "You are the planner.".into(),
                tools: vec!["read".into()],
                model: None,
                thinking: None,
            },
            "planner · Plan the panel",
        )
        .expect("orphan row created");
    let orphan_id = orphan.id().to_string();

    // The retried StartSubagent sees the Existing row, finds NO initial-run
    // evidence, and recovers: reuses the orphan row, registers the fresh
    // local channel, and queues exactly one initial Run.
    let child_id = start(&core, "run-1", PARENT)
        .await
        .expect("orphan recovery start");
    assert_eq!(child_id, orphan_id, "recovery reuses the orphan row");

    // The recovered run dispatches with the freshly registered channel (the
    // crash-gap recovery must not lose the parent's message root).
    wait_for(
        || host.lock().unwrap().is_some(),
        "recovered run to reach the harness",
    );
    let received = host.lock().unwrap().clone().expect("host captured");
    let child_env = received.child.expect("child env present");
    assert_eq!(
        child_env.channel_root.as_deref(),
        Some("/tmp/pi-subagents-messages/abc")
    );
    assert_eq!(child_env.run_id, "run-1");

    // Exactly one initial Run was queued (recovery is not a duplicate).
    wait_for(
        || {
            core.sessions
                .session_status(&child_id)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "recovered run to settle",
    );
    let entries = core
        .doc_host
        .open(&child_id)
        .expect("doc open")
        .doc()
        .read_entries()
        .expect("entries");
    let user_entries = entries
        .iter()
        .filter(|e| e.role == cypher_doc::MessageRole::User)
        .count();
    assert_eq!(
        user_entries, 1,
        "orphan recovery queues exactly one initial run"
    );
    core.shutdown().await;
}

/// A LATER child turn (after the initial run consumed the channel — e.g. a
/// restart, or a queued follow-up command) keeps the persisted profile but
/// has NO messaging channel: the child runs `PI_SUBAGENT_ROLE=child` without
/// the channel env, so its messaging tools honestly report unavailable.
#[tokio::test(flavor = "multi_thread")]
async fn later_child_turn_has_no_messaging_channel() {
    // A harness that records EVERY run's host context (so the follow-up turn's
    // channel absence is asserted) and streams a quick Done per run.
    struct RecordingHarness(Arc<Mutex<Vec<cypher_harness::RunHostContext>>>);
    #[async_trait]
    impl Harness for RecordingHarness {
        fn id(&self) -> HarnessId {
            HarnessId::Pi
        }
        fn display_name(&self) -> &str {
            "Recording"
        }
        // Non-steerable so a follow-up command forces a FRESH run task (whose
        // host context is what this test inspects) instead of routing into the
        // parked session.
        fn supports_steering(&self) -> bool {
            false
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
            self.0.lock().unwrap().push(controls.host.clone());
            let events: Vec<Result<AgentEvent, HarnessError>> = vec![
                Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Pi,
                    model: "anthropic/claude-sonnet-4".into(),
                    tools: vec![],
                    cwd: request.cwd.clone(),
                    session_id: "child-hs".into(),
                    assistant_message_id: "a-1".into(),
                }),
                Ok(done_ok()),
            ];
            Ok(futures::stream::iter(events).boxed())
        }
    }

    let hosts = Arc::new(Mutex::new(Vec::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(RecordingHarness(hosts.clone())));
    let dir = tempfile::tempdir().unwrap();
    let core = EngineCore::assemble(dir.path(), Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.workspace
        .create_chat(PARENT, None, Some(core.device_id.as_str()), None, None)
        .expect("parent chat");
    let child_id = start(&core, "run-1", PARENT).await.expect("start ok");
    // Initial run settles (its channel was registered then consumed).
    wait_for(
        || {
            core.sessions
                .session_status(&child_id)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "initial child run to settle",
    );
    // Queue a SECOND normal run command (a later child turn).
    core.doc_host
        .queue_command(
            &child_id,
            cypher_doc::SessionCommandPayload::Run {
                request: cypher_proto::RunRequest {
                    prompt: "Follow-up turn".into(),
                    harness: Some(HarnessId::Pi),
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: "/tmp".into(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    resume: None,
                    worktree: None,
                    attachments: Vec::new(),
                    pending_attachments: Vec::new(),
                },
                message_id: String::new(),

                agent_prompt: None,
            },
        )
        .expect("queue follow-up");
    wait_for(
        || hosts.lock().unwrap().len() >= 2,
        "the follow-up run to reach the harness",
    );
    let recorded = hosts.lock().unwrap().clone();
    let first = recorded[0].child.as_ref().expect("first run child env");
    assert_eq!(
        first.channel_root.as_deref(),
        Some("/tmp/pi-subagents-messages/abc"),
        "the initial run carries the engine-local channel"
    );
    let second = recorded[1].child.as_ref().expect("second run child env");
    assert_eq!(
        second.channel_root, None,
        "a later child turn has NO messaging channel (profile only)"
    );
    assert_eq!(
        second.run_id, "run-1",
        "the persisted profile still applies"
    );
    assert_eq!(second.agent, "planner");
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_delete_cascades_to_children() {
    let rig = assemble();
    rig.core
        .workspace
        .create_chat(PARENT, None, Some(rig.core.device_id.as_str()), None, None)
        .expect("parent chat");
    let child_id = start(&rig.core, "run-1", PARENT).await.expect("start ok");
    // Let the child run fully settle first: cascade deletion interrupts live
    // children best-effort, and a settled run is the clean (deterministic)
    // case — its terminal bookkeeping has landed before the row goes.
    wait_for(
        || {
            rig.core
                .sessions
                .session_status(&child_id)
                .is_some_and(|s| s.status == SessionStatus::Idle)
        },
        "child run to settle before cascade delete",
    );

    let reply = rig
        .core
        .rpc_service()
        .handle(
            methods::MUTATE,
            serde_json::json!({ "op": "deleteChat", "chatId": PARENT }),
        )
        .await
        .expect("delete chat");
    let RpcReply::Value(_) = reply else {
        panic!("Mutate must be unary");
    };
    // Both rows are gone (child cascade), including the child's session row.
    // The child teardown is a spawned task, so wait for it to land.
    assert!(rig.core.workspace.chat(PARENT).expect("read").is_none());
    // The child teardown is a spawned task and interrupts a possibly-live
    // run first (settlement is bounded at ~3s), so wait generously.
    wait_for(
        || rig.core.workspace.chat(&child_id).expect("read").is_none(),
        "child row cascade delete",
    );
    assert!(
        rig.core
            .workspace
            .read_sessions()
            .expect("sessions")
            .iter()
            .all(|s| s.chat_id != child_id)
    );
    rig.core.shutdown().await;
}
